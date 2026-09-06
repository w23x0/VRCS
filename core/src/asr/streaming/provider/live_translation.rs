use super::{CloudEvent, MAX_TRANSCRIPT_BYTES};
use std::time::Duration;
use tokio::time::Instant;

use crate::asr::streaming::LiveTranslationResult;
use crate::config::AsrConfig;
use crate::models::LiveTranslation;
use crate::providers::SERVICE_GEMINI_LIVE_TRANSLATE;

const SETTLE_DELAY: Duration = Duration::from_millis(800);

pub(super) const MAX_DISPLAY_CHARS: usize = 160;

pub(super) fn append_display_text(current: &mut String, delta: &str) {
    if delta.is_empty() {
        return;
    }
    current.push_str(delta);

    let excess = current.chars().count().saturating_sub(MAX_DISPLAY_CHARS);
    if excess > 0 {
        let cut = current.char_indices().nth(excess).unwrap().0;
        // Prefer a word or clause boundary within the retained window.
        let cut = current[cut..]
            .char_indices()
            .find_map(|(index, c)| {
                let end = cut + index + c.len_utf8();
                ((c.is_whitespace() || matches!(c, ',' | '，' | ';' | '；'))
                    && !current[end..].trim().is_empty())
                .then_some(end)
            })
            .unwrap_or(cut);
        current.drain(..cut);
    }
    let leading = current.len() - current.trim_start().len();
    current.drain(..leading);
}

#[derive(Default)]
pub(super) struct State {
    pub(super) snapshot: Option<LiveTranslation>,
    pub(super) input: String,
    pub(super) output: String,
    completed_sentence: bool,
    last_update: Option<Instant>,
}

fn terminal(c: char) -> bool {
    matches!(
        c,
        '.' | '。'
            | '．'
            | '｡'
            | '!'
            | '！'
            | '?'
            | '？'
            | '؟'
            | '‼'
            | '⁇'
            | '⁈'
            | '⁉'
            | '\n'
            | '\r'
    )
}

fn closing(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '”' | '’' | '」' | '』' | ')' | '）' | ']' | '】' | '》' | '»'
    )
}

fn has_content(text: &str) -> bool {
    text.chars().any(|c| {
        !c.is_whitespace()
            && !terminal(c)
            && !closing(c)
            && !matches!(
                c,
                '“' | '‘' | '「' | '『' | '(' | '（' | '[' | '【' | '《' | '«'
            )
    })
}

impl State {
    fn take_completed(&mut self, config: &AsrConfig, flush: bool) -> Vec<LiveTranslationResult> {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return Vec::new();
        };
        let same_language = config.backend == SERVICE_GEMINI_LIVE_TRANSLATE
            && snapshot.language.as_deref().is_some_and(|language| {
                crate::translation::same_translation_language(language, &snapshot.target_language)
            });
        if !has_content(&self.input) || (!flush && !same_language && !has_content(&self.output)) {
            return Vec::new();
        }
        // The streams do not share sentence IDs and may use different punctuation.
        // Finalize one settled block without leaving a sentence on either side.
        let mut transcript = snapshot.clone();
        transcript.text = std::mem::take(&mut self.input).trim().to_owned();
        transcript.translation = std::mem::take(&mut self.output).trim().to_owned();
        tracing::debug!(
            source_chars = transcript.text.chars().count(),
            target_chars = transcript.translation.chars().count(),
            flush,
            "finalizing native translation block"
        );
        snapshot.utterance_id = format!("live-{}", uuid::Uuid::new_v4());
        snapshot.text.clear();
        snapshot.translation.clear();
        self.completed_sentence = true;
        self.last_update = None;
        vec![LiveTranslationResult {
            transcript,
            provider: crate::providers::recognition_service(&config.backend)
                .unwrap()
                .0
                .into(),
            model: config.service_settings[&config.backend].model.clone(),
        }]
    }
}

pub(super) fn poll(config: &AsrConfig, state: &mut State) -> Option<CloudEvent> {
    if !state
        .last_update
        .is_some_and(|last| last.elapsed() >= SETTLE_DELAY)
    {
        return None;
    }
    let completed = state.take_completed(config, false);
    (!completed.is_empty()).then(|| CloudEvent::LiveTranslation {
        snapshot: state.snapshot.clone().unwrap(),
        completed,
    })
}

pub(super) fn finish(config: &AsrConfig, state: &mut State) -> Option<CloudEvent> {
    let completed = state.take_completed(config, true);
    (!completed.is_empty()).then(|| CloudEvent::LiveTranslation {
        snapshot: state.snapshot.clone().unwrap(),
        completed,
    })
}

pub(super) fn append(
    config: &AsrConfig,
    state: &mut State,
    text: &str,
    translation: &str,
    language: Option<&str>,
) -> Result<Option<CloudEvent>, String> {
    if text.is_empty() && translation.is_empty() {
        return Ok(None);
    }
    // A sentence may be published before a later delta adds more end marks.
    // Do not let those marks become the next source/translation pair.
    fn trim_continuation<'a>(completed: bool, buffer: &str, delta: &'a str) -> &'a str {
        if completed && buffer.trim().is_empty() {
            delta.trim_start_matches(|c: char| {
                c.is_whitespace() || terminal(c) || (closing(c) && c != '"' && c != '\'')
            })
        } else {
            delta
        }
    }
    let text = trim_continuation(state.completed_sentence, &state.input, text);
    let translation = trim_continuation(state.completed_sentence, &state.output, translation);
    let snapshot = state.snapshot.get_or_insert_with(|| LiveTranslation {
        utterance_id: format!("live-{}", uuid::Uuid::new_v4()),
        text: String::new(),
        language: None,
        translation: String::new(),
        target_language: config.live_translation_target.clone().unwrap_or_default(),
    });
    if state.input.len() + text.len() > MAX_TRANSCRIPT_BYTES
        || state.output.len() + translation.len() > MAX_TRANSCRIPT_BYTES
    {
        return Err("Live translation transcript limit reached; restart recognition".into());
    }
    append_display_text(&mut snapshot.text, text);
    append_display_text(&mut snapshot.translation, translation);
    state.input.push_str(text);
    state.output.push_str(translation);
    if let Some(language) = language {
        snapshot.language = Some(language.to_owned());
    }
    if !text.trim().is_empty() || !translation.trim().is_empty() {
        state.last_update = Some(Instant::now());
    }
    Ok(Some(CloudEvent::LiveTranslation {
        snapshot: state.snapshot.clone().unwrap(),
        completed: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unequal_sentence_counts_preserve_the_entire_bilingual_block() {
        for service in [
            crate::providers::SERVICE_GEMINI_LIVE_TRANSLATE,
            crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE,
        ] {
            for (source, target) in [
                ("How are you?", "你好。还好吗？"),
                ("Hello. How are you?", "你好，最近怎么样？"),
            ] {
                let config = AsrConfig {
                    backend: service.into(),
                    live_translation_target: Some("zh-Hans".into()),
                    ..Default::default()
                };
                let mut state = State::default();
                let Some(CloudEvent::LiveTranslation { completed, .. }) =
                    append(&config, &mut state, source, target, None).unwrap()
                else {
                    panic!()
                };
                assert!(
                    completed.is_empty(),
                    "Do not finalize a pair on the first punctuation mark: {completed:?}"
                );
                let Some(CloudEvent::LiveTranslation { completed, .. }) =
                    finish(&config, &mut state)
                else {
                    panic!()
                };
                assert_eq!(completed.len(), 1);
                assert_eq!(completed[0].transcript.text, source);
                assert_eq!(completed[0].transcript.translation, target);
                assert!(finish(&config, &mut state).is_none());
            }
        }
    }

    #[test]
    fn pending_display_keeps_sentences_until_the_block_is_stored() {
        for text in [
            "Hello. Next",
            "Ready?! Go!",
            "你好。还好吗？",
            "Value 3.14? Next",
            "Hello\r\nNext",
        ] {
            let mut display = String::new();
            append_display_text(&mut display, text);
            assert_eq!(display, text);
        }
    }

    #[test]
    fn settling_waits_for_translation_and_keeps_the_same_message_id() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        append(&config, &mut state, "Hello. How are you?", "", None).unwrap();
        let id = state.snapshot.as_ref().unwrap().utterance_id.clone();
        state.last_update = Some(Instant::now() - SETTLE_DELAY);
        assert!(poll(&config, &mut state).is_none());
        append(&config, &mut state, "", "你好，最近怎么样？", None).unwrap();
        assert!(poll(&config, &mut state).is_none());
        state.last_update = Some(Instant::now() - SETTLE_DELAY);
        let Some(CloudEvent::LiveTranslation {
            completed,
            snapshot,
        }) = poll(&config, &mut state)
        else {
            panic!()
        };
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].transcript.utterance_id, id);
        assert_eq!(completed[0].transcript.text, "Hello. How are you?");
        assert_eq!(completed[0].transcript.translation, "你好，最近怎么样？");
        assert_ne!(snapshot.utterance_id, id);
        assert!(poll(&config, &mut state).is_none());
        assert!(finish(&config, &mut state).is_none());
    }

    #[test]
    fn both_protocols_keep_punctuation_deltas_in_the_same_block() {
        for service in [
            crate::providers::SERVICE_GEMINI_LIVE_TRANSLATE,
            crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE,
        ] {
            let config = AsrConfig {
                backend: service.into(),
                live_translation_target: Some("zh-Hans".into()),
                ..Default::default()
            };
            let mut state = State::default();
            let mut results = Vec::new();
            for (input, delta) in [
                (true, "Ready?"),
                (false, "准备好了吗？"),
                (true, "!"),
                (false, "！"),
                (false, "走吧！"),
                (true, " Go!"),
                (true, "\r\n"),
                (false, "\n"),
                (true, "Value 3."),
                (true, "14?"),
                (false, "数值是3.14吗？"),
                (true, "Stop．"),
                (false, "停止｡"),
            ] {
                let event = if service == crate::providers::SERVICE_GEMINI_LIVE_TRANSLATE {
                    let key = if input { "inputTranscription" } else { "outputTranscription" };
                    super::super::gemini_live_translate::normalize_event(&config, &json!({"serverContent": {key: {"text": delta}}}), &mut state)
                } else {
                    let kind = if input { "input" } else { "output" };
                    super::super::openai_live_translate::normalize_event(&config, &json!({"type": format!("session.{kind}_transcript.delta"), "delta": delta}), &mut state)
                }.unwrap();
                if let Some(CloudEvent::LiveTranslation { completed, .. }) = event {
                    results.extend(completed);
                }
            }
            let Some(CloudEvent::LiveTranslation { completed, .. }) = finish(&config, &mut state)
            else {
                panic!()
            };
            results.extend(completed);
            assert_eq!(
                results
                    .iter()
                    .map(|r| (
                        r.transcript.text.as_str(),
                        r.transcript.translation.as_str()
                    ))
                    .collect::<Vec<_>>(),
                vec![(
                    "Ready?! Go!\r\nValue 3.14?Stop．",
                    "准备好了吗？！走吧！\n数值是3.14吗？停止｡"
                )],
                "{service}"
            );
            assert!(finish(&config, &mut state).is_none());
        }
    }
}
