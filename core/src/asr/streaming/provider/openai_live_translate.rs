use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{http::Request, Message};

use crate::config::AsrConfig;
use crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE;

use super::{
    authenticated_request, live_translation, pcm16_base64, resample_16k_to_24k, service_settings,
    CloudEvent, InitializationEvent,
};

const TRANSCRIPTION_MODEL: &str = "gpt-realtime-whisper";

pub(super) fn build_request(config: &AsrConfig, key: &str) -> Result<Request<()>, String> {
    let model = &service_settings(config, SERVICE_OPENAI_REALTIME_TRANSLATE)?.model;
    let mut url = reqwest::Url::parse("wss://api.openai.com/v1/realtime/translations").unwrap();
    url.query_pairs_mut().append_pair("model", model);
    authenticated_request(url.into(), key, false)
}

pub(super) fn session_update(config: &AsrConfig) -> Result<Value, String> {
    let target = config
        .live_translation_target
        .as_deref()
        .ok_or("Select an automatic translation target for OpenAI Realtime Translation")?;
    let target = crate::providers::openai_translation_language(target)?;
    Ok(json!({"type": "session.update", "session": {"audio": {
        "input": {"transcription": {"model": TRANSCRIPTION_MODEL}, "noise_reduction": null},
        "output": {"language": target}
    }}}))
}

fn error_detail(value: &Value) -> String {
    let error = &value["error"];
    let mut detail = error["message"]
        .as_str()
        .unwrap_or("OpenAI Realtime Translation failed")
        .to_owned();
    for field in ["type", "code", "param", "event_id"] {
        if let Some(value) = error[field].as_str() {
            detail.push_str(&format!("; {field}={value}"));
        }
    }
    detail
}

pub(super) fn initialization_event(value: &Value, update: &Value) -> InitializationEvent {
    match value["type"].as_str() {
        Some("session.updated") => {
            if value.pointer("/session/audio/output/language")
                == update.pointer("/session/audio/output/language")
                && value
                    .pointer("/session/audio/input/transcription/model")
                    .and_then(Value::as_str)
                    == Some(TRANSCRIPTION_MODEL)
            {
                InitializationEvent::Ready
            } else {
                InitializationEvent::Failed("OpenAI did not confirm the requested translation target and source transcription model".into())
            }
        }
        Some("error") => InitializationEvent::Failed(error_detail(value)),
        Some("session.closed") => InitializationEvent::Failed(
            "OpenAI closed the translation session before configuration completed".into(),
        ),
        _ => InitializationEvent::Pending,
    }
}

pub(super) fn audio_message(samples: &[f32]) -> Message {
    Message::Text(
        json!({"type": "session.input_audio_buffer.append",
        "audio": pcm16_base64(&resample_16k_to_24k(samples))})
        .to_string()
        .into(),
    )
}

pub(super) fn normalize_event(
    config: &AsrConfig,
    value: &Value,
    state: &mut live_translation::State,
) -> Result<Option<CloudEvent>, String> {
    let delta = value["delta"].as_str().unwrap_or_default();
    match value["type"].as_str() {
        Some("session.input_transcript.delta") => {
            live_translation::append(config, state, delta, "", None)
        }
        Some("session.output_transcript.delta") => {
            live_translation::append(config, state, "", delta, None)
        }
        Some("error") => {
            // A recoverable session error must not terminate the current subtitle.
            tracing::warn!(detail = %error_detail(value), "OpenAI translation session error");
            Ok(None)
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AsrConfig {
        AsrConfig {
            backend: SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hant".into()),
            ..Default::default()
        }
    }

    #[test]
    fn target_codes_match_the_api_without_changing_stored_targets() {
        for (target, wire) in [
            ("zh-Hans", "zh"),
            ("zh-Hant", "zh"),
            ("pt-BR", "pt"),
            ("pt-PT", "pt"),
            ("fil", "tl"),
            ("nb", "no"),
            ("en", "en"),
        ] {
            let mut config = config();
            config.live_translation_target = Some(target.into());
            let update = session_update(&config).unwrap();
            assert_eq!(update["session"]["audio"]["output"]["language"], wire);
            let mut confirmation = update.clone();
            confirmation["type"] = json!("session.updated");
            assert!(matches!(
                initialization_event(&confirmation, &update),
                InitializationEvent::Ready
            ));
            let mut state = live_translation::State::default();
            live_translation::append(&config, &mut state, "Hello.", "Translation.", None).unwrap();
            assert_eq!(state.snapshot.unwrap().target_language, target);
            assert_eq!(config.live_translation_target.as_deref(), Some(target));
        }
        let mut config = config();
        config.live_translation_target = Some("yue-Hant".into());
        assert!(session_update(&config).unwrap_err().contains("yue-Hant"));
    }

    #[test]
    fn translation_endpoint_and_configuration_are_independent_of_transcription() {
        let config = config();
        let request = build_request(&config, "test-key").unwrap();
        assert_eq!(
            request.uri().to_string(),
            "wss://api.openai.com/v1/realtime/translations?model=gpt-realtime-translate"
        );
        assert_eq!(request.headers()["Authorization"], "Bearer test-key");
        let update = session_update(&config).unwrap();
        assert_eq!(update["session"]["audio"]["output"]["language"], "zh");
        assert!(matches!(
            initialization_event(&json!({"type":"session.created"}), &update),
            InitializationEvent::Pending
        ));
        let mut confirmation = update.clone();
        confirmation["type"] = json!("session.updated");
        assert!(matches!(
            initialization_event(&confirmation, &update),
            InitializationEvent::Ready
        ));
        confirmation["session"]["audio"]["output"]["language"] = json!("en");
        assert!(matches!(
            initialization_event(&confirmation, &update),
            InitializationEvent::Failed(_)
        ));
        confirmation["session"]["audio"]["output"]["language"] = json!("zh");
        confirmation["session"]["audio"]["input"]["transcription"] = Value::Null;
        assert!(matches!(
            initialization_event(&confirmation, &update),
            InitializationEvent::Failed(_)
        ));
        assert!(session_update(&AsrConfig::default()).is_err());
    }

    #[test]
    fn audio_is_24k_pcm_and_preserves_silence() {
        use base64::Engine as _;
        for size in [3200, 100] {
            let Message::Text(message) = audio_message(&vec![0.0; size]) else {
                panic!()
            };
            let value: Value = serde_json::from_str(&message).unwrap();
            assert_eq!(value["type"], "session.input_audio_buffer.append");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(value["audio"].as_str().unwrap())
                .unwrap();
            assert_eq!(bytes, vec![0; size * 3]);
        }
    }

    #[test]
    fn independent_deltas_keep_repetitions_and_ignore_audio_and_timestamps() {
        let config = config();
        let mut state = live_translation::State::default();
        let mut results = Vec::new();
        for (kind, delta) in [
            ("output_transcript", "你好。"),
            ("input_transcript", "hello"),
            ("output_audio", "SGVsbG8="),
            ("input_transcript", " hello."),
            ("input_transcript", " Value 3."),
            ("input_transcript", "14."),
            ("output_transcript", "数值3.14。"),
        ] {
            if let Some(CloudEvent::LiveTranslation { completed, .. }) = normalize_event(&config, &json!({"type": format!("session.{kind}.delta"), "delta": delta, "elapsed_ms": 200}), &mut state).unwrap() {
                results.extend(completed);
            }
        }
        if let Some(CloudEvent::LiveTranslation { completed, .. }) =
            live_translation::finish(&config, &mut state)
        {
            results.extend(completed);
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].transcript.text, "hello hello. Value 3.14.");
        assert_eq!(results[0].transcript.translation, "你好。数值3.14。");
        assert!(results.iter().all(|r| r.provider == "openai"
            && r.model == "gpt-realtime-translate"
            && r.transcript.language.is_none()));
        assert!(live_translation::finish(&config, &mut state).is_none());
    }

    #[test]
    fn recoverable_errors_keep_pending_text_and_report_details() {
        let config = config();
        let mut state = live_translation::State::default();
        normalize_event(
            &config,
            &json!({"type":"session.input_transcript.delta","delta":"Hello"}),
            &mut state,
        )
        .unwrap();
        let error = json!({"type":"error","error":{"type":"invalid_request_error","code":"bad_audio","param":"audio","message":"Invalid audio"}});
        assert!(normalize_event(&config, &error, &mut state)
            .unwrap()
            .is_none());
        let detail = error_detail(&error);
        assert!(detail.contains("bad_audio") && detail.contains("param=audio"));
        assert_eq!(state.input, "Hello");
        assert!(matches!(
            initialization_event(&error, &session_update(&config).unwrap()),
            InitializationEvent::Failed(_)
        ));
    }
}
