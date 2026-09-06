use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::asr::AsrService;
use crate::config::{AppConfig, TranslationConfig};
use crate::db::conversations::{publish_latest_catalog, ConversationCatalog};
use crate::db::Database;
use crate::models::{now_iso8601, LiveTranscription, Subtitle, SubtitleTranslation};
use crate::subtitle_output::{SubtitleLifecyclePublisher, TranslationFailure};
use crate::translation::{same_translation_language, TranslationDispatcher};

#[derive(Clone)]
pub(crate) struct PipelineDependencies {
    asr: Arc<Mutex<AsrService>>,
    database: Arc<Mutex<Database>>,
    live: broadcast::Sender<LiveTranscription>,
    conversation_catalog: broadcast::Sender<ConversationCatalog>,
    translation: TranslationDispatcher,
    config: Arc<std::sync::RwLock<AppConfig>>,
    language_session: Arc<std::sync::RwLock<crate::language_session::ActiveLanguageSession>>,
    output: SubtitleLifecyclePublisher,
}

impl PipelineDependencies {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        asr: Arc<Mutex<AsrService>>,
        database: Arc<Mutex<Database>>,
        live: broadcast::Sender<LiveTranscription>,
        conversation_catalog: broadcast::Sender<ConversationCatalog>,
        translation: TranslationDispatcher,
        config: Arc<std::sync::RwLock<AppConfig>>,
        language_session: Arc<std::sync::RwLock<crate::language_session::ActiveLanguageSession>>,
        output: SubtitleLifecyclePublisher,
    ) -> Self {
        Self {
            asr,
            database,
            live,
            conversation_catalog,
            translation,
            config,
            language_session,
            output,
        }
    }

    pub(crate) fn publish_live_translation(
        &self,
        source: &str,
        snapshot: crate::models::LiveTranslation,
    ) {
        self.output.live_translation(source, snapshot);
    }

    pub(crate) fn publish_live(&self, event: LiveTranscription) {
        match &event {
            LiveTranscription::Partial {
                utterance_id,
                source,
                text,
                language,
            } => self
                .output
                .asr_partial(utterance_id, source, text, language.as_deref()),
            LiveTranscription::Failed {
                utterance_id,
                source,
                code,
                detail,
            } => self
                .output
                .asr_failed(utterance_id.as_deref(), source, code, detail),
            LiveTranscription::AudioLevel { .. } => {}
        }
        let _ = self.live.send(event);
    }

    pub(crate) fn cancel_recognition(&self, utterance_id: &str, source: &str, reason: &str) {
        self.output.asr_cancelled(utterance_id, source, reason);
    }

    pub(crate) fn reset_recognition(&self, source: &str) {
        self.output.asr_reset(source);
    }

    pub(crate) async fn transcribe_and_publish(
        &self,
        segment: Vec<f32>,
        source: &'static str,
    ) -> Result<(), String> {
        let message_id = format!("utterance-{}", uuid::Uuid::new_v4());
        let rms = (segment.iter().map(|sample| sample * sample).sum::<f32>()
            / segment.len().max(1) as f32)
            .sqrt();
        let peak = segment.iter().copied().map(f32::abs).fold(0.0, f32::max);
        tracing::debug!(
            source,
            samples = segment.len(),
            duration_seconds = segment.len() as f64 / 16_000.0,
            rms,
            peak,
            "sending speech segment to ASR"
        );
        let transcriber = Arc::clone(&self.asr);
        let transcription = match tokio::task::spawn_blocking(move || {
            transcriber.lock().expect("asr lock").transcribe(&segment)
        })
        .await
        {
            Ok(Ok(transcription)) => transcription,
            Ok(Err(detail)) => {
                self.output.asr_failed(
                    Some(&message_id),
                    source,
                    "asr.transcription_failed",
                    &detail,
                );
                return Err(detail);
            }
            Err(error) => {
                let detail = format!("Recognition task exited unexpectedly: {error}");
                self.output
                    .asr_failed(Some(&message_id), source, "asr.task_failed", &detail);
                return Err(detail);
            }
        };
        tracing::debug!(
            source,
            text_length = transcription.text.chars().count(),
            language = transcription.language.as_deref().unwrap_or("unknown"),
            "ASR transcription completed"
        );
        self.publish_text(
            transcription.text,
            transcription.language,
            source,
            message_id,
        )
        .await
    }

    pub(crate) async fn publish_text(
        &self,
        text: String,
        language: Option<String>,
        source: &'static str,
        message_id: String,
    ) -> Result<(), String> {
        self.publish_text_with_translation(text, language, source, message_id, None)
            .await
    }

    pub(crate) async fn publish_native_translation(
        &self,
        source: &'static str,
        result: crate::asr::LiveTranslationResult,
    ) -> Result<(), String> {
        let transcript = result.transcript;
        let translation = SubtitleTranslation {
            text: transcript.translation.trim().to_owned(),
            source_language: transcript.language.clone(),
            target_language: transcript.target_language,
            provider: crate::providers::GEMINI_PROVIDER.into(),
            model: Some(result.model),
            created_at: now_iso8601(),
        };
        self.publish_text_with_translation(
            transcript.text,
            transcript.language,
            source,
            transcript.utterance_id,
            Some(translation),
        )
        .await
    }

    async fn publish_text_with_translation(
        &self,
        text: String,
        language: Option<String>,
        source: &'static str,
        message_id: String,
        native: Option<SubtitleTranslation>,
    ) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            self.output.asr_cancelled(&message_id, source, "empty");
            return Ok(());
        }
        let subtitle = Subtitle {
            id: None,
            conversation_id: None,
            text,
            language,
            started_at: None,
            ended_at: None,
            source: source.into(),
            created_at: now_iso8601(),
            translations: Vec::new(),
        };
        let database = Arc::clone(&self.database);
        let conversation_catalog = self.conversation_catalog.clone();
        let native_record = native
            .clone()
            .filter(|translation| !translation.text.is_empty());
        let (saved, native_error) = match tokio::task::spawn_blocking(move || {
            let database = database
                .lock()
                .map_err(|_| "Database lock is unavailable".to_string())?;
            let mut saved = database
                .add_subtitle(&subtitle)
                .map_err(|error| error.to_string())?;
            let mut native_error = None;
            if let Some(translation) = native_record {
                match database.save_translation(saved.id.expect("saved subtitle id"), &translation)
                {
                    Ok(_) => saved.translations.push(translation),
                    Err(error) => native_error = Some(error.to_string()),
                }
            }
            publish_latest_catalog(&database, &conversation_catalog);
            Ok::<_, String>((saved, native_error))
        })
        .await
        {
            Ok(Ok(saved)) => saved,
            Ok(Err(detail)) => {
                self.output
                    .asr_failed(Some(&message_id), source, "asr.storage_failed", &detail);
                return Err(detail);
            }
            Err(error) => {
                let detail = format!("Subtitle storage task exited unexpectedly: {error}");
                self.output
                    .asr_failed(Some(&message_id), source, "asr.storage_failed", &detail);
                return Err(detail);
            }
        };
        let (mut translation_targets, translation_prompt, api_profiles, include_vrcx_context) = {
            let config = self.config.read().expect("config lock");
            let language = self
                .language_session
                .read()
                .expect("language session lock")
                .resolve(&config);
            (
                automatic_translation_targets(
                    &language.translation,
                    source,
                    saved.language.as_deref(),
                ),
                language.translation.prompt,
                config.asr.api_profiles.clone(),
                config.vrcx.enabled && config.vrcx.include_in_llm_context,
            )
        };
        if let (Some(targets), Some(native)) = (&mut translation_targets, &native) {
            targets.retain(|target| target.target_language != native.target_language);
        }
        let mut output_targets = translation_targets
            .as_ref()
            .map(|targets| {
                targets
                    .iter()
                    .map(|target| target.target_language.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(native) = &native {
            if !native.text.is_empty() {
                output_targets.insert(0, native.target_language.clone());
            }
        }
        self.output.subtitle_stored_with_message(
            saved.clone(),
            !output_targets.is_empty(),
            output_targets,
            &message_id,
        );
        if let Some(native) = &native {
            if let Some(detail) = native_error {
                self.output
                    .translation_failed_with_message(TranslationFailure {
                        subtitle_id: saved.id.unwrap(),
                        code: "translation.storage_failed".into(),
                        detail,
                        target_language: &native.target_language,
                        preferred: true,
                        message_id: &message_id,
                        source,
                    });
            } else if !native.text.is_empty() {
                self.output.translation_completed_with_message(
                    saved.id.unwrap(),
                    native.clone(),
                    true,
                    &message_id,
                    source,
                );
            } else if !saved.language.as_deref().is_some_and(|language| {
                same_translation_language(language, &native.target_language)
            }) {
                self.output
                    .translation_failed_with_message(TranslationFailure {
                        subtitle_id: saved.id.unwrap(),
                        code: "translation.live_incomplete".into(),
                        detail: "Live translation ended before the translated sentence arrived"
                            .into(),
                        target_language: &native.target_language,
                        preferred: true,
                        message_id: &message_id,
                        source,
                    });
            }
        }
        if let Some(targets) = translation_targets {
            if targets.is_empty() {
                return Ok(());
            }
            let failed_targets = targets.clone();
            if let Err(detail) = self.translation.enqueue(
                saved.clone(),
                targets,
                translation_prompt,
                api_profiles,
                message_id.clone(),
                include_vrcx_context,
                native.is_none(),
            ) {
                if let Some(subtitle_id) = saved.id {
                    for (index, target) in failed_targets.iter().enumerate() {
                        self.output
                            .translation_failed_with_message(TranslationFailure {
                                subtitle_id,
                                code: "translation.queue_full".into(),
                                detail: detail.clone(),
                                target_language: &target.target_language,
                                preferred: native.is_none() && index == 0,
                                message_id: &message_id,
                                source,
                            });
                    }
                }
                tracing::warn!(%detail, "automatic translation was not queued");
            }
        }
        Ok(())
    }
}

fn automatic_translation_targets(
    config: &TranslationConfig,
    source: &str,
    source_language: Option<&str>,
) -> Option<Vec<crate::config::TranslationTargetConfig>> {
    if config.mode != "automatic" {
        return None;
    }
    let targets = if source == "microphone" {
        &config.microphone_targets
    } else {
        &config.speaker_targets
    };
    let targets = targets
        .iter()
        .filter(|target| {
            !source_language
                .is_some_and(|source| same_translation_language(source, &target.target_language))
        })
        .cloned()
        .collect::<Vec<_>>();
    (!targets.is_empty()).then_some(targets)
}

#[cfg(test)]
mod tests {
    use super::automatic_translation_targets;
    use crate::config::{TranslationConfig, TranslationTargetConfig};

    fn native_result() -> crate::asr::LiveTranslationResult {
        crate::asr::LiveTranslationResult {
            transcript: crate::models::LiveTranslation {
                utterance_id: "native-1".into(),
                text: "Hello.".into(),
                language: Some("en".into()),
                translation: "你好。".into(),
                target_language: "zh-Hans".into(),
            },
            model: "gemini-3.5-live-translate-preview".into(),
        }
    }

    #[tokio::test]
    async fn native_translation_is_stored_and_extra_targets_are_not_preferred() {
        let events = crate::domain_events::DomainEventHub::new();
        let dependencies = super::super::tests::test_dependencies(events);
        let mut presentation = dependencies.output.subscribe_presentation_events();
        let mut translation_events = dependencies.output.subscribe_translations();
        {
            let mut config = dependencies.config.write().unwrap();
            config.translation.mode = "automatic".into();
            config.translation.speaker_targets = vec![
                TranslationTargetConfig::new("zh-Hans"),
                TranslationTargetConfig::new("ja"),
            ];
        }
        dependencies
            .publish_native_translation("speaker", native_result())
            .await
            .unwrap();
        let history = dependencies
            .database
            .lock()
            .unwrap()
            .subtitle_history(10)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].text, "Hello.");
        assert_eq!(history[0].translations.len(), 1);
        assert_eq!(history[0].translations[0].text, "你好。");
        assert_eq!(history[0].translations[0].provider, "gemini");
        assert_eq!(
            history[0].translations[0].model.as_deref(),
            Some("gemini-3.5-live-translate-preview")
        );
        assert!(matches!(presentation.try_recv().unwrap(),
            crate::subtitle_output::PresentationEvent::Final { subtitle, .. }
                if subtitle.translations.len() == 1));
        assert!(matches!(
            translation_events.recv().await.unwrap(),
            crate::subtitle_output::TranslationEvent::TranslationCompleted {
                preferred: true,
                ..
            }
        ));
        let event =
            tokio::time::timeout(std::time::Duration::from_secs(2), translation_events.recv())
                .await
                .unwrap()
                .unwrap();
        assert!(matches!(event,
            crate::subtitle_output::TranslationEvent::TranslationStarted { target_language, preferred: false, .. }
                if target_language == "ja"));
    }

    #[tokio::test]
    async fn failed_native_translation_storage_preserves_source_without_success_event() {
        let events = crate::domain_events::DomainEventHub::new();
        let dependencies = super::super::tests::test_dependencies(events);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("storage-failure.db");
        *dependencies.database.lock().unwrap() = crate::db::Database::open(&path).unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_translation BEFORE INSERT ON subtitle_translations BEGIN SELECT RAISE(FAIL, 'test storage failure'); END;").unwrap();
        let mut translations = dependencies.output.subscribe_translations();
        dependencies
            .publish_native_translation("speaker", native_result())
            .await
            .unwrap();
        let history = dependencies
            .database
            .lock()
            .unwrap()
            .subtitle_history(10)
            .unwrap();
        assert_eq!(history[0].text, "Hello.");
        assert!(history[0].translations.is_empty());
        assert!(matches!(translations.try_recv().unwrap(),
            crate::subtitle_output::TranslationEvent::TranslationFailed { code, .. }
                if code == "translation.storage_failed"));
        assert!(translations.try_recv().is_err());
    }

    #[tokio::test]
    async fn native_only_does_not_enqueue_llm_and_missing_translation_keeps_source() {
        let events = crate::domain_events::DomainEventHub::new();
        let dependencies = super::super::tests::test_dependencies(events);
        let mut translations = dependencies.output.subscribe_translations();
        {
            let mut config = dependencies.config.write().unwrap();
            config.translation.mode = "automatic".into();
            config.translation.microphone_targets = vec![TranslationTargetConfig::new("zh-Hans")];
        }
        dependencies
            .publish_native_translation("microphone", native_result())
            .await
            .unwrap();
        assert!(matches!(
            translations.try_recv().unwrap(),
            crate::subtitle_output::TranslationEvent::TranslationCompleted { .. }
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), translations.recv())
                .await
                .is_err()
        );
        let mut missing = native_result();
        missing.transcript.utterance_id = "native-2".into();
        missing.transcript.translation.clear();
        dependencies
            .publish_native_translation("microphone", missing)
            .await
            .unwrap();
        let history = dependencies
            .database
            .lock()
            .unwrap()
            .subtitle_history(10)
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].source, "microphone");
        assert_eq!(history[0].text, "Hello.");
        assert!(history[0].translations.is_empty());
        assert!(matches!(translations.try_recv().unwrap(),
            crate::subtitle_output::TranslationEvent::TranslationFailed { code, .. }
                if code == "translation.live_incomplete"));
    }

    #[test]
    fn automatic_mode_translates_microphone_with_its_own_target() {
        let config = TranslationConfig {
            mode: "automatic".into(),
            speaker_targets: vec![TranslationTargetConfig::new("zh-Hans")],
            microphone_targets: vec![TranslationTargetConfig::new("ja")],
            ..TranslationConfig::default()
        };

        let microphone = automatic_translation_targets(&config, "microphone", None).unwrap();
        assert_eq!(microphone[0].target_language, "ja");
        let speaker = automatic_translation_targets(&config, "speaker", None).unwrap();
        assert_eq!(speaker[0].target_language, "zh-Hans");
    }

    #[test]
    fn automatic_mode_skips_matching_source_and_target_languages() {
        let config = TranslationConfig {
            mode: "automatic".into(),
            speaker_targets: vec![TranslationTargetConfig::new("zh-Hans")],
            microphone_targets: vec![TranslationTargetConfig::new("ja")],
            ..TranslationConfig::default()
        };

        assert!(automatic_translation_targets(&config, "speaker", Some("zh")).is_none());
        assert!(automatic_translation_targets(&config, "microphone", Some("ja")).is_none());
        assert!(automatic_translation_targets(&config, "speaker", Some("ja")).is_some());
        assert!(automatic_translation_targets(&config, "speaker", None).is_some());
    }

    #[test]
    fn non_automatic_modes_do_not_translate_any_voice_automatically() {
        for mode in ["disabled", "manual"] {
            let config = TranslationConfig {
                mode: mode.into(),
                ..TranslationConfig::default()
            };

            assert!(automatic_translation_targets(&config, "microphone", Some("ja")).is_none());
            assert!(automatic_translation_targets(&config, "speaker", Some("en")).is_none());
        }
    }
}
