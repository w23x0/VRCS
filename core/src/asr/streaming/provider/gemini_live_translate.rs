use serde_json::{json, Value};

use crate::asr::streaming::LiveTranslationResult;
use crate::config::AsrConfig;
use crate::models::LiveTranslation;
use crate::providers::SERVICE_GEMINI_LIVE_TRANSLATE;

use super::{service_settings, CloudEvent, MAX_TRANSCRIPT_BYTES};

const MAX_DISPLAY_CHARS: usize = 160;

fn append_display_text(current: &mut String, delta: &str) {
    if delta.is_empty() {
        return;
    }
    current.push_str(delta);

    // These are display boundaries only, not paired translation completions.
    let mut start = 0;
    for (index, character) in current.char_indices() {
        let tail = &current[index + character.len_utf8()..];
        let sentence_end = matches!(character, '。' | '！' | '？' | '!' | '?' | '\n')
            || (character == '.'
                && tail.starts_with(|c: char| c.is_whitespace() || matches!(c, '"' | '”' | '’')));
        if sentence_end {
            let next = tail.trim_start_matches(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '。' | '！' | '？' | '!' | '?' | '.' | '"' | '”' | '’' | '」' | '』'
                    )
            });
            if !next.is_empty() {
                start = current.len() - next.len();
            }
        }
    }
    current.drain(..start);

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
    snapshot: Option<LiveTranslation>,
    input: String,
    output: String,
}

fn sentence_end(text: &str, flush: bool) -> Option<usize> {
    for (index, c) in text.char_indices() {
        let end = index + c.len_utf8();
        let tail = &text[end..];
        let decimal = c == '.'
            && text[..index].ends_with(|c: char| c.is_ascii_digit())
            && (tail.starts_with(|c: char| c.is_ascii_digit()) || (tail.is_empty() && !flush));
        let terminal = matches!(c, '。' | '！' | '？' | '!' | '?' | '\n')
            || (c == '.'
                && (tail.is_empty()
                    || tail
                        .starts_with(|c: char| c.is_whitespace() || matches!(c, '"' | '”' | '’'))));
        if terminal && !decimal && !text[..end].trim().is_empty() {
            let rest = tail.trim_start_matches(|c| {
                matches!(
                    c,
                    '。' | '！' | '？' | '!' | '?' | '.' | '"' | '”' | '’' | '」' | '』'
                )
            });
            return Some(text.len() - rest.len());
        }
    }
    (flush && !text.trim().is_empty()).then_some(text.len())
}

impl State {
    fn take_completed(&mut self, config: &AsrConfig, flush: bool) -> Vec<LiveTranslationResult> {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return Vec::new();
        };
        let same_language = snapshot.language.as_deref().is_some_and(|language| {
            crate::translation::same_translation_language(language, &snapshot.target_language)
        });
        let mut completed = Vec::new();
        while let Some(input_end) = sentence_end(&self.input, flush) {
            let output_end = if same_language {
                0
            } else {
                match sentence_end(&self.output, flush) {
                    Some(end) => end,
                    None if flush => 0,
                    None => break,
                }
            };
            let mut transcript = snapshot.clone();
            transcript.text = self
                .input
                .drain(..input_end)
                .collect::<String>()
                .trim()
                .to_owned();
            transcript.translation = self
                .output
                .drain(..output_end)
                .collect::<String>()
                .trim()
                .to_owned();
            if !transcript.text.is_empty() {
                completed.push(LiveTranslationResult {
                    transcript,
                    model: config.service_settings[SERVICE_GEMINI_LIVE_TRANSLATE]
                        .model
                        .clone(),
                });
                snapshot.utterance_id = format!("live-{}", uuid::Uuid::new_v4());
            }
        }
        if !completed.is_empty() {
            snapshot.text.clear();
            snapshot.translation.clear();
            append_display_text(&mut snapshot.text, &self.input);
            append_display_text(&mut snapshot.translation, &self.output);
        }
        completed
    }
}

pub(super) fn finish(config: &AsrConfig, state: &mut State) -> Option<CloudEvent> {
    let completed = state.take_completed(config, true);
    (!completed.is_empty()).then(|| CloudEvent::LiveTranslation {
        snapshot: state.snapshot.clone().unwrap(),
        completed,
    })
}

pub(super) fn setup(config: &AsrConfig) -> Result<Value, String> {
    let settings = service_settings(config, SERVICE_GEMINI_LIVE_TRANSLATE)?;
    let target = config
        .live_translation_target
        .as_deref()
        .ok_or("Select an automatic translation target for Gemini Live Translate")?;
    crate::providers::validate_live_translation_language(target)?;
    Ok(json!({ "setup": {
        "model": format!("models/{}", settings.model.trim_start_matches("models/")),
        "inputAudioTranscription": {},
        "outputAudioTranscription": {},
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "translationConfig": {"targetLanguageCode": target, "echoTargetLanguage": false}
        }
    }}))
}

pub(super) fn normalize_event(
    config: &AsrConfig,
    value: &Value,
    state: &mut State,
) -> Result<Option<CloudEvent>, String> {
    if let Some(error) = value.get("error") {
        return Err(error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Gemini Live Translate failed")
            .to_owned());
    }
    if value.get("goAway").is_some() {
        return Err("Gemini Live Translate is closing the session".into());
    }
    let Some(content) = value.get("serverContent") else {
        return Ok(None);
    };
    if content.get("interrupted").and_then(Value::as_bool) == Some(true) {
        *state = State::default();
        return Ok(Some(CloudEvent::Failed {
            utterance_id: None,
            reset_session: true,
            code: "asr.live_translation_interrupted".into(),
            detail: "Gemini Live Translate output was interrupted".into(),
        }));
    }
    let input = content.get("inputTranscription");
    let output = content.get("outputTranscription");
    let text = input
        .and_then(|v| v.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let translation = output
        .and_then(|v| v.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if text.is_empty() && translation.is_empty() {
        return Ok(None);
    }
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
        return Err("Gemini Live Translate transcript limit reached; restart recognition".into());
    }
    append_display_text(&mut snapshot.text, text);
    append_display_text(&mut snapshot.translation, translation);
    state.input.push_str(text);
    state.output.push_str(translation);
    if let Some(language) = input
        .and_then(|v| v.get("languageCode"))
        .and_then(Value::as_str)
    {
        snapshot.language = Some(language.to_owned());
    }
    let completed = state.take_completed(config, false);
    Ok(Some(CloudEvent::LiveTranslation {
        snapshot: state.snapshot.clone().unwrap(),
        completed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AsrConfig {
        AsrConfig {
            backend: SERVICE_GEMINI_LIVE_TRANSLATE.into(),
            live_translation_target: Some("zh-Hant".into()),
            ..Default::default()
        }
    }

    #[test]
    fn setup_requires_target_and_requests_both_transcripts() {
        let value = setup(&config()).unwrap();
        assert_eq!(
            value.pointer("/setup/generationConfig/translationConfig/targetLanguageCode"),
            Some(&json!("zh-Hant"))
        );
        assert_eq!(
            value.pointer("/setup/generationConfig/responseModalities"),
            Some(&json!(["AUDIO"]))
        );
        assert!(value.pointer("/setup/inputAudioTranscription").is_some());
        assert!(value.pointer("/setup/outputAudioTranscription").is_some());
        assert!(setup(&AsrConfig::default()).is_err());
    }

    #[test]
    fn audio_and_turn_markers_do_not_drop_or_finalize_text() {
        let mut state = State::default();
        let first = normalize_event(
            &config(),
            &json!({"serverContent": {
                "modelTurn": {"parts": [{"inlineData": {"data": "AA=="}}]},
                "outputTranscription": {"text": "你好"}, "turnComplete": true
            }}),
            &mut state,
        )
        .unwrap()
        .unwrap();
        let CloudEvent::LiveTranslation {
            snapshot: first, ..
        } = first
        else {
            panic!("expected snapshot")
        };
        assert_eq!(first.translation, "你好");
        assert!(first.text.is_empty());
        let second = normalize_event(
            &config(),
            &json!({"serverContent": {
                "inputTranscription": {"text": "hello hello", "languageCode": "en"}
            }}),
            &mut state,
        )
        .unwrap()
        .unwrap();
        let CloudEvent::LiveTranslation {
            snapshot: second, ..
        } = second
        else {
            panic!("expected snapshot")
        };
        assert_eq!(first.utterance_id, second.utterance_id);
        assert_eq!(second.text, "hello hello");
        assert_eq!(second.translation, "你好");
        assert!(normalize_event(
            &config(),
            &json!({"serverContent": {"generationComplete": true}}),
            &mut state
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn real_continuous_stream_pairs_sentences_in_arrival_order() {
        // Synthetic English speech, live service capture on 2026-09-05.
        // Audio and transport metadata are excluded from this fixture.
        let messages: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/gemini_live_translate.json")).unwrap();
        let mut state = State::default();
        let mut results = Vec::new();
        for message in messages {
            let event = normalize_event(&config(), &message, &mut state).unwrap();
            let Some(CloudEvent::LiveTranslation { completed, .. }) = event else {
                panic!("expected independent stream update");
            };
            results.extend(completed);
        }
        let snapshot = state.snapshot.unwrap();
        assert!(snapshot.text.is_empty());
        assert!(snapshot.translation.is_empty());
        assert_eq!(results.len(), 5);
        assert_eq!(results[0].transcript.text, "Hello.");
        assert_eq!(results[0].transcript.translation, "你好。");
        assert_eq!(results[4].transcript.text, "Please.");
        assert_eq!(results[4].transcript.translation, "谢谢。");
        assert_eq!(snapshot.language.as_deref(), Some("en"));
    }

    #[test]
    fn sentences_roll_independently_and_keep_late_translation() {
        let mut state = State::default();
        for (input, output, expected_input, expected_output) in [
            ("Hello.", "你好。", "", ""),
            (" Next", "", "Next", ""),
            (" sentence.", "下一", "Next sentence.", "下一"),
            ("", "句。", "", ""),
            ("", "再见。新的", "", "新的"),
        ] {
            let event = normalize_event(
                &config(),
                &json!({"serverContent": {
                    "inputTranscription": {"text": input},
                    "outputTranscription": {"text": output}
                }}),
                &mut state,
            )
            .unwrap()
            .unwrap();
            let CloudEvent::LiveTranslation { snapshot, .. } = event else {
                panic!("expected snapshot")
            };
            assert_eq!(snapshot.text, expected_input);
            assert_eq!(snapshot.translation, expected_output);
        }
    }

    #[test]
    fn display_keeps_punctuation_decimals_and_bounds_long_streams() {
        let mut text = String::new();
        for delta in ["Value 3.", "14!", "”", " "] {
            append_display_text(&mut text, delta);
        }
        assert_eq!(text, "Value 3.14!” ");
        append_display_text(&mut text, "Next sentence.");
        assert_eq!(text, "Next sentence.");

        text.clear();
        for _ in 0..10_000 {
            append_display_text(&mut text, "汉字🙂");
            assert!(text.chars().count() <= MAX_DISPLAY_CHARS);
            assert!(text.ends_with("汉字🙂"));
        }
        text = "a".repeat(MAX_DISPLAY_CHARS);
        append_display_text(&mut text, " recent words");
        assert_eq!(text, "recent words");
    }

    #[test]
    fn full_sentences_survive_display_limits_and_stop_flushes_once() {
        let mut state = State::default();
        let original = "word ".repeat(100);
        let translated = "文字".repeat(200);
        let event = normalize_event(
            &config(),
            &json!({"serverContent": {
                "inputTranscription": {"text": original, "languageCode": "en"},
                "outputTranscription": {"text": translated}
            }}),
            &mut state,
        )
        .unwrap()
        .unwrap();
        let CloudEvent::LiveTranslation {
            snapshot,
            completed,
        } = event
        else {
            panic!()
        };
        assert!(completed.is_empty());
        assert!(snapshot.text.chars().count() <= MAX_DISPLAY_CHARS);
        let CloudEvent::LiveTranslation { completed, .. } = finish(&config(), &mut state).unwrap()
        else {
            panic!()
        };
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].transcript.text, original.trim());
        assert_eq!(completed[0].transcript.translation, translated);
        assert!(finish(&config(), &mut state).is_none());
    }

    #[test]
    fn same_language_does_not_wait_for_translation_and_interrupt_clears_pending_text() {
        let mut state = State::default();
        let event = normalize_event(
            &config(),
            &json!({"serverContent": {
                "inputTranscription": {"text": "你好。", "languageCode": "zh-Hant"}
            }}),
            &mut state,
        )
        .unwrap()
        .unwrap();
        let CloudEvent::LiveTranslation { completed, .. } = event else {
            panic!()
        };
        assert_eq!(completed.len(), 1);
        assert!(completed[0].transcript.translation.is_empty());
        normalize_event(
            &config(),
            &json!({"serverContent": {
                "inputTranscription": {"text": "pending", "languageCode": "en"}
            }}),
            &mut state,
        )
        .unwrap();
        normalize_event(
            &config(),
            &json!({"serverContent": {"interrupted": true}}),
            &mut state,
        )
        .unwrap();
        assert!(finish(&config(), &mut state).is_none());
        assert!(state.input.is_empty());
        assert!(state.output.is_empty());
    }

    #[test]
    fn sentence_boundaries_preserve_decimals_and_missing_translation() {
        assert_eq!(sentence_end("3.", false), None);
        assert_eq!(sentence_end("3.14 is pi. Next", false), Some(11));
        let mut state = State::default();
        normalize_event(
            &config(),
            &json!({"serverContent": {
                "inputTranscription": {"text": "Hello. Tail", "languageCode": "en"}
            }}),
            &mut state,
        )
        .unwrap();
        let CloudEvent::LiveTranslation { completed, .. } = finish(&config(), &mut state).unwrap()
        else {
            panic!()
        };
        assert_eq!(completed.len(), 2);
        assert_eq!(completed[0].transcript.text, "Hello.");
        assert_eq!(completed[1].transcript.text, "Tail");
        assert!(completed
            .iter()
            .all(|result| result.transcript.translation.is_empty()));
        assert_ne!(
            completed[0].transcript.utterance_id,
            completed[1].transcript.utterance_id
        );
    }

    #[test]
    fn errors_and_limits_are_bounded() {
        let mut state = State::default();
        assert!(
            normalize_event(&config(), &json!({"error":{"message":"quota"}}), &mut state).is_err()
        );
        assert!(normalize_event(&config(), &json!({"serverContent":{"inputTranscription":{"text":"a".repeat(MAX_TRANSCRIPT_BYTES + 1)}}}), &mut state).is_err());
    }
}
