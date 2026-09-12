use super::*;

#[derive(Default)]
pub(super) struct Timing {
    previews: Vec<(String, String)>,
}

pub(in super::super) fn append_timed(
    config: &AsrConfig,
    state: &mut State,
    text: &str,
    translation: &str,
) -> Result<Option<CloudEvent>, String> {
    if text.is_empty() && translation.is_empty() {
        return Ok(None);
    }
    state.timing.get_or_insert_with(Timing::default);
    append(config, state, text, translation, None)
}

fn join_sentences<'a>(sentences: impl Iterator<Item = &'a str>) -> String {
    let mut joined = String::new();
    for sentence in sentences {
        append_sentence(&mut joined, sentence);
    }
    joined
}

fn append_sentence(current: &mut String, sentence: &str) {
    if !current.is_empty() {
        current.push(' ');
    }
    current.push_str(sentence.trim());
}

fn take_targets(buffer: &mut String, flush_tail: bool) -> Vec<String> {
    let mut sentences = Vec::new();
    while let Some(end) = sentence_end(buffer, false)
        .or_else(|| (flush_tail && !buffer.is_empty()).then_some(buffer.len()))
    {
        let text = buffer.drain(..end).collect::<String>().trim().to_owned();
        if has_content(&text) {
            sentences.push(text);
        }
    }
    sentences
}

fn preview_pairs(state: &State) -> (Vec<(String, String)>, String) {
    let mut targets = state.targets.iter().map(String::as_str).collect::<Vec<_>>();
    if has_content(&state.output) {
        targets.push(state.output.trim());
    }
    if state.sources.len() != 1 {
        return (Vec::new(), join_sentences(targets.into_iter()));
    }
    let mut previews: Vec<(String, String)> = Vec::new();
    let mut live = String::new();
    for (index, target) in targets.into_iter().enumerate() {
        if let Some(source) = state.sources.get(index) {
            previews.push((source.utterance_id.clone(), target.to_owned()));
        } else if has_content(&state.input) || state.sources.is_empty() {
            append_sentence(&mut live, target);
        } else if let Some((_, text)) = previews.last_mut() {
            append_sentence(text, target);
        } else {
            append_sentence(&mut live, target);
        }
    }
    (previews, live)
}

impl Timing {
    pub(super) fn collect(
        &mut self,
        state: &mut State,
        config: &AsrConfig,
        flush: bool,
    ) -> Option<CloudEvent> {
        let mut snapshot = state.snapshot.clone()?;
        let idle = state
            .last_input
            .into_iter()
            .chain(state.last_output)
            .all(|last| last.elapsed() >= SOURCE_IDLE_DELAY);
        let mut completed = Vec::new();
        for text in take_sentences(&mut state.input, state.last_input, flush) {
            let mut source = snapshot.clone();
            source.utterance_id = format!("live-source-{}", uuid::Uuid::new_v4());
            source.text = text;
            source.translation.clear();
            completed.push(result(config, source.clone(), true));
            state.sources.push_back(source);
        }
        state
            .targets
            .extend(take_targets(&mut state.output, flush || idle));

        let mut translations = Vec::new();
        let mut live_translation = String::new();
        let settled = flush || (idle && !state.targets.is_empty());
        if settled && !state.sources.is_empty() {
            self.previews.clear();
            if state.sources.len() == state.targets.len() {
                while let (Some(mut source), Some(target)) =
                    (state.sources.pop_front(), state.targets.pop_front())
                {
                    source.translation = target;
                    translations.push(result(config, source, false));
                }
            } else if state.targets.is_empty() {
                translations.extend(
                    state
                        .sources
                        .drain(..)
                        .map(|source| result(config, source, false)),
                );
            } else {
                let source_ids = state
                    .sources
                    .iter()
                    .map(|source| source.utterance_id.clone())
                    .collect();
                let mut grouped = state.sources.front().unwrap().clone();
                grouped.text =
                    join_sentences(state.sources.iter().map(|source| source.text.as_str()));
                grouped.translation = join_sentences(state.targets.iter().map(String::as_str));
                let mut update = result(config, grouped, false);
                update.source_utterance_ids = source_ids;
                translations.push(update);
                state.sources.clear();
                state.targets.clear();
            }
        } else {
            let (previews, live) = preview_pairs(state);
            live_translation = live;
            for (id, _) in self
                .previews
                .iter()
                .filter(|(id, _)| !previews.iter().any(|(next_id, _)| next_id == id))
            {
                let Some(mut source) = state
                    .sources
                    .iter()
                    .find(|source| source.utterance_id == *id)
                    .cloned()
                else {
                    continue;
                };
                source.translation.clear();
                translations.push(result(config, source, true));
            }
            for (id, text) in &previews {
                if self
                    .previews
                    .iter()
                    .any(|(previous_id, previous_text)| previous_id == id && previous_text == text)
                {
                    continue;
                }
                let Some(mut source) = state
                    .sources
                    .iter()
                    .find(|source| source.utterance_id == *id)
                    .cloned()
                else {
                    continue;
                };
                source.translation = text.clone();
                translations.push(result(config, source, true));
            }
            self.previews = previews;
        }

        snapshot.text.clear();
        snapshot.translation.clear();
        if !flush {
            append_display_text(&mut snapshot.text, &state.input);
            append_display_text(&mut snapshot.translation, &live_translation);
        }
        let changed = state.snapshot.as_ref() != Some(&snapshot);
        state.snapshot = Some(snapshot.clone());
        (!completed.is_empty() || !translations.is_empty() || changed).then_some(
            CloudEvent::LiveTranslation {
                snapshot,
                completed,
                translations,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_deltas_preview_on_source_rows_before_the_window_settles() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        let Some(CloudEvent::LiveTranslation { completed, .. }) =
            append_timed(&config, &mut state, "First. Second", "").unwrap()
        else {
            panic!()
        };
        assert_eq!(completed.len(), 1);

        let Some(CloudEvent::LiveTranslation { translations, .. }) =
            append_timed(&config, &mut state, "", "第一句。第二").unwrap()
        else {
            panic!()
        };
        assert_eq!(translations.len(), 1);
        assert!(translations.iter().all(|update| update.pending));
        for (source, preview) in completed.iter().zip(&translations) {
            assert_eq!(
                source.transcript.utterance_id,
                preview.transcript.utterance_id
            );
        }
        assert_eq!(translations[0].transcript.translation, "第一句。");
    }

    #[test]
    fn live_snapshot_excludes_translation_already_shown_on_source_rows() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        append_timed(&config, &mut state, "First. Second", "").unwrap();

        let Some(CloudEvent::LiveTranslation {
            translations,
            snapshot,
            ..
        }) = append_timed(&config, &mut state, "", "第一句。第二句。第三").unwrap()
        else {
            panic!()
        };
        assert_eq!(translations.len(), 1);
        assert_eq!(translations[0].transcript.translation, "第一句。");
        assert_eq!(snapshot.text, "Second");
        assert_eq!(snapshot.translation, "第二句。 第三");
    }

    #[test]
    fn merged_target_does_not_shift_the_next_translation_to_the_wrong_source() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        append_timed(&config, &mut state, "First. Second. Third", "").unwrap();

        let Some(CloudEvent::LiveTranslation {
            translations,
            snapshot,
            ..
        }) = append_timed(&config, &mut state, "", "第一句和第二句。第三").unwrap()
        else {
            panic!()
        };

        assert!(translations.is_empty());
        assert_eq!(snapshot.text, "Third");
        assert_eq!(snapshot.translation, "第一句和第二句。 第三");
    }

    #[test]
    fn an_existing_row_preview_is_cleared_when_the_window_becomes_ambiguous() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        append_timed(&config, &mut state, "First.", "").unwrap();
        state.last_input = Some(Instant::now() - END_MARK_DELAY);
        poll(&config, &mut state).unwrap();
        append_timed(&config, &mut state, "", "第一").unwrap();

        let Some(CloudEvent::LiveTranslation {
            translations,
            snapshot,
            ..
        }) = append_timed(&config, &mut state, " Second. Third", "").unwrap()
        else {
            panic!()
        };

        assert_eq!(translations.len(), 1);
        assert!(translations[0].pending);
        assert!(translations[0].transcript.translation.is_empty());
        assert_eq!(snapshot.translation, "第一");
    }

    #[test]
    fn completed_targets_wait_for_a_stable_window() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        let Some(CloudEvent::LiveTranslation {
            completed: mut sources,
            ..
        }) = append_timed(&config, &mut state, "First. Second.", "").unwrap()
        else {
            panic!()
        };
        assert_eq!(sources.len(), 1);
        let Some(CloudEvent::LiveTranslation {
            translations,
            snapshot,
            ..
        }) = append_timed(&config, &mut state, "", "第一句。第二").unwrap()
        else {
            panic!()
        };
        assert_eq!(translations.len(), 1);
        assert!(translations[0].pending);
        assert_eq!(translations[0].transcript.translation, "第一句。");
        assert_eq!(snapshot.translation, "第二");
        assert_eq!(snapshot.text, "Second.");

        append_timed(&config, &mut state, "", "句。").unwrap();
        state.last_input = Some(Instant::now() - SOURCE_IDLE_DELAY);
        state.last_output = Some(Instant::now() - SOURCE_IDLE_DELAY);
        let Some(CloudEvent::LiveTranslation {
            completed,
            translations,
            ..
        }) = poll(&config, &mut state)
        else {
            panic!()
        };
        sources.extend(completed);
        assert_eq!(sources.len(), 2);
        assert_eq!(translations.len(), 2);
        for (source, target) in sources.iter().zip(&translations) {
            assert_eq!(
                source.transcript.utterance_id,
                target.transcript.utterance_id
            );
            assert!(!target.pending);
        }
        assert_eq!(translations[0].transcript.translation, "第一句。");
        assert_eq!(translations[1].transcript.translation, "第二句。");
    }

    #[test]
    fn split_target_is_kept_with_its_source_window() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        append_timed(&config, &mut state, "First. Second.", "").unwrap();
        append_timed(&config, &mut state, "", "第一句。补充一句。第二句。").unwrap();
        state.last_input = Some(Instant::now() - SOURCE_IDLE_DELAY);
        state.last_output = Some(Instant::now() - SOURCE_IDLE_DELAY);
        let Some(CloudEvent::LiveTranslation { translations, .. }) = poll(&config, &mut state)
        else {
            panic!()
        };
        assert_eq!(translations.len(), 1);
        assert_eq!(translations[0].transcript.text, "First. Second.");
        assert_eq!(
            translations[0].transcript.translation,
            "第一句。 补充一句。 第二句。"
        );
    }

    #[test]
    fn merged_target_is_kept_with_its_source_window() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        append_timed(&config, &mut state, "First. Second.", "").unwrap();
        append_timed(&config, &mut state, "", "第一句和第二句。").unwrap();
        state.last_input = Some(Instant::now() - SOURCE_IDLE_DELAY);
        state.last_output = Some(Instant::now() - SOURCE_IDLE_DELAY);
        let Some(CloudEvent::LiveTranslation { translations, .. }) = poll(&config, &mut state)
        else {
            panic!()
        };
        assert_eq!(translations.len(), 1);
        assert_eq!(translations[0].transcript.text, "First. Second.");
        assert_eq!(translations[0].transcript.translation, "第一句和第二句。");
    }

    #[test]
    fn complete_sentences_can_share_a_frame_without_losing_their_translation() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        append_timed(&config, &mut state, "Yes. Yes.", "").unwrap();
        let Some(CloudEvent::LiveTranslation {
            mut translations, ..
        }) = append_timed(&config, &mut state, "", "是。是。").unwrap()
        else {
            panic!()
        };
        let Some(CloudEvent::LiveTranslation {
            translations: tail, ..
        }) = finish(&config, &mut state)
        else {
            panic!()
        };
        translations.extend(tail);
        translations.retain(|update| !update.pending);
        assert_eq!(translations.len(), 2);
        assert!(translations
            .iter()
            .all(|t| t.transcript.translation == "是。"));
        assert_ne!(
            translations[0].transcript.utterance_id,
            translations[1].transcript.utterance_id
        );
    }

    #[test]
    fn an_unfinished_target_stays_in_preview_when_another_source_starts() {
        let config = AsrConfig {
            backend: crate::providers::SERVICE_OPENAI_REALTIME_TRANSLATE.into(),
            live_translation_target: Some("zh-Hans".into()),
            ..Default::default()
        };
        let mut state = State::default();
        append_timed(&config, &mut state, "First.", "").unwrap();
        state.last_input = Some(Instant::now() - END_MARK_DELAY);
        let Some(CloudEvent::LiveTranslation { completed, .. }) = poll(&config, &mut state) else {
            panic!()
        };
        let id = completed[0].transcript.utterance_id.clone();
        let Some(CloudEvent::LiveTranslation {
            snapshot,
            translations,
            ..
        }) = append_timed(&config, &mut state, "", "第一").unwrap()
        else {
            panic!()
        };
        assert_eq!(translations.len(), 1);
        assert!(translations[0].pending);
        assert_eq!(translations[0].transcript.utterance_id, id);
        assert_eq!(translations[0].transcript.translation, "第一");
        assert!(snapshot.translation.is_empty());

        let Some(CloudEvent::LiveTranslation {
            snapshot,
            translations,
            ..
        }) = append_timed(&config, &mut state, " Second.", "").unwrap()
        else {
            panic!()
        };
        assert!(snapshot.translation.is_empty());
        assert!(translations.is_empty());

        let Some(CloudEvent::LiveTranslation {
            snapshot,
            translations,
            ..
        }) = append_timed(&config, &mut state, "", "句。").unwrap()
        else {
            panic!()
        };
        assert!(snapshot.translation.is_empty());
        assert_eq!(translations.len(), 1);
        assert!(translations[0].pending);
        assert_eq!(translations[0].transcript.translation, "第一句。");

        let Some(CloudEvent::LiveTranslation {
            translations,
            snapshot,
            ..
        }) = append_timed(&config, &mut state, "", "第二句。").unwrap()
        else {
            panic!()
        };
        assert!(translations.is_empty());
        assert_eq!(snapshot.translation, "第二句。");
        state.last_input = Some(Instant::now() - SOURCE_IDLE_DELAY);
        state.last_output = Some(Instant::now() - SOURCE_IDLE_DELAY);
        let Some(CloudEvent::LiveTranslation {
            translations: tail, ..
        }) = poll(&config, &mut state)
        else {
            panic!()
        };
        assert_eq!(tail.len(), 2);
        assert!(tail.iter().all(|update| !update.pending));
        assert_eq!(tail[0].transcript.utterance_id, id);
        assert_eq!(tail[0].transcript.translation, "第一句。");
        assert_eq!(tail[1].transcript.translation, "第二句。");
    }
}
