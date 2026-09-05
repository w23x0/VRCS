use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::time::Duration;

use crate::config::ApiProfile;

use super::{LlmError, LlmProgress, LlmRequest};

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

pub(super) async fn generate(
    http: &reqwest::Client,
    profile: &ApiProfile,
    api_key: &str,
    request: LlmRequest<'_>,
    on_progress: Option<&LlmProgress>,
) -> Result<String, LlmError> {
    let model = model_id(request.model)?;
    let method = if on_progress.is_some() {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };
    let response = http
        .post(format!("{BASE_URL}/models/{model}:{method}"))
        .timeout(Duration::from_millis(profile.timeout_ms))
        .header("x-goog-api-key", api_key)
        .json(&request_body(&request))
        .send()
        .await
        .map_err(network_error)?;
    let status = response.status();
    if !status.is_success() {
        return Err(response_error(response).await);
    }
    if let Some(progress) = on_progress {
        return stream_response(response, progress).await;
    }
    let value = response.json().await.map_err(invalid_response)?;
    parse_response(&value)
}

pub(super) async fn list_models(
    http: &reqwest::Client,
    api_key: &str,
) -> Result<Vec<String>, LlmError> {
    let response = http
        .get(format!("{BASE_URL}/models"))
        .header("x-goog-api-key", api_key)
        .send()
        .await
        .map_err(network_error)?;
    let status = response.status();
    if !status.is_success() {
        return Err(response_error(response).await);
    }
    let value = response.json().await.map_err(invalid_response)?;
    let models = extract_models(&value);
    if models.is_empty() {
        return Err(LlmError {
            code: "llm.invalid_response",
            detail: "Gemini did not return any models that support generateContent".into(),
            retryable: false,
        });
    }
    Ok(models)
}

fn request_body(request: &LlmRequest<'_>) -> Value {
    let mut body = json!({
        "systemInstruction": { "parts": [{ "text": request.instructions }] },
        "contents": [{ "role": "user", "parts": [{ "text": request.input }] }],
        "generationConfig": { "maxOutputTokens": request.max_output_tokens }
    });
    let model = request.model.trim().trim_start_matches("models/");
    let (thinking, reserve) = if model.starts_with("gemini-2.5-flash") {
        let budget = if request.thinking_enabled { 1024 } else { 0 };
        (Some(json!({ "thinkingBudget": budget })), budget)
    } else if model.starts_with("gemini-2.5-pro") {
        let budget = if request.thinking_enabled { 2048 } else { 128 };
        (Some(json!({ "thinkingBudget": budget })), budget)
    } else if (model.starts_with("gemini-3.") || model.starts_with("gemini-3-"))
        && !model.contains("-image")
    {
        (
            Some(json!({ "thinkingLevel": if request.thinking_enabled { "high" } else { "low" } })),
            if request.thinking_enabled { 8192 } else { 4096 },
        )
    } else {
        (None, 0)
    };
    if let Some(thinking) = thinking {
        body["generationConfig"]["thinkingConfig"] = thinking;
    }
    // Thinking and visible text share the output limit. Levels are not hard budgets.
    body["generationConfig"]["maxOutputTokens"] =
        json!(request.max_output_tokens.max(512).saturating_add(reserve));
    body
}

fn model_id(model: &str) -> Result<&str, LlmError> {
    let model = model.trim().strip_prefix("models/").unwrap_or(model.trim());
    if model.is_empty()
        || model.len() > 200
        || !model
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_' | b'.'))
    {
        return Err(LlmError {
            code: "llm.invalid_profile",
            detail: "Gemini model ID is invalid".into(),
            retryable: false,
        });
    }
    Ok(model)
}

async fn stream_response(
    response: reqwest::Response,
    on_progress: &LlmProgress,
) -> Result<String, LlmError> {
    let mut chunks = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut output = String::new();
    let mut event_data = String::new();
    let mut completed = false;
    while let Some(chunk) = chunks.next().await {
        buffer.extend_from_slice(&chunk.map_err(network_error)?);
        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let line = buffer.drain(..=newline).collect::<Vec<_>>();
            completed |= process_sse_line(&line, &mut event_data, &mut output, on_progress)?;
        }
    }
    if !buffer.is_empty() {
        completed |= process_sse_line(&buffer, &mut event_data, &mut output, on_progress)?;
    }
    completed |= process_sse_line(b"\n", &mut event_data, &mut output, on_progress)?;
    completed_response(output, completed)
}

fn process_sse_line(
    line: &[u8],
    event_data: &mut String,
    output: &mut String,
    on_progress: &LlmProgress,
) -> Result<bool, LlmError> {
    let line = std::str::from_utf8(line).map_err(|error| LlmError {
        code: "llm.invalid_response",
        detail: error.to_string(),
        retryable: false,
    })?;
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(data) = line.strip_prefix("data:") {
        event_data.push_str(data.strip_prefix(' ').unwrap_or(data));
        event_data.push('\n');
    }
    if !line.is_empty() || event_data.is_empty() {
        return Ok(false);
    }
    let value = serde_json::from_str(event_data).map_err(|error| LlmError {
        code: "llm.invalid_response",
        detail: error.to_string(),
        retryable: false,
    })?;
    event_data.clear();
    let completed = response_completed(&value)?;
    if let Some(text) = extract_raw_text(&value) {
        output.push_str(&text);
        on_progress(output);
    }
    Ok(completed)
}

fn response_completed(value: &Value) -> Result<bool, LlmError> {
    if let Some(error) = value.get("error") {
        let status = error
            .get("code")
            .and_then(Value::as_u64)
            .and_then(|code| u16::try_from(code).ok())
            .and_then(|code| StatusCode::from_u16(code).ok())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        return Err(status_error(status, value));
    }
    if let Some(reason) = value
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(response_failure(
            format!("Gemini blocked the prompt: {reason}"),
            false,
        ));
    }
    match first_candidate(value)
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(Value::as_str)
    {
        Some("STOP") => Ok(true),
        None | Some("" | "FINISH_REASON_UNSPECIFIED") => Ok(false),
        Some(reason) => Err(response_failure(
            format!("Gemini generation stopped: {reason}"),
            false,
        )),
    }
}

fn completed_response(output: String, completed: bool) -> Result<String, LlmError> {
    if !completed {
        return Err(response_failure(
            "Gemini response ended without a completion signal".into(),
            true,
        ));
    }
    let output = output.trim().to_owned();
    if output.is_empty() {
        return Err(response_failure(
            "Gemini completed the response without text".into(),
            true,
        ));
    }
    Ok(output)
}

fn response_failure(detail: String, retryable: bool) -> LlmError {
    LlmError {
        code: "llm.request_failed",
        detail,
        retryable,
    }
}

fn first_candidate(value: &Value) -> Option<&Value> {
    value
        .get("candidates")?
        .as_array()?
        .iter()
        .find(|candidate| candidate.get("index").and_then(Value::as_u64).unwrap_or(0) == 0)
}

fn parse_response(value: &Value) -> Result<String, LlmError> {
    completed_response(
        extract_raw_text(value).unwrap_or_default(),
        response_completed(value)?,
    )
}

fn extract_raw_text(value: &Value) -> Option<String> {
    let text = first_candidate(value)?
        .pointer("/content/parts")?
        .as_array()?
        .iter()
        .filter(|part| part.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    Some(text).filter(|text| !text.is_empty())
}

fn extract_models(value: &Value) -> Vec<String> {
    let mut models = value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| {
            model
                .get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_some_and(|methods| methods.iter().any(|method| method == "generateContent"))
        })
        .filter_map(|model| model.get("name").and_then(Value::as_str))
        .map(|name| name.strip_prefix("models/").unwrap_or(name).trim())
        .filter(|name| !name.is_empty() && name.len() <= 200)
        .map(str::to_owned)
        .take(500)
        .collect::<Vec<_>>();
    models.sort_by_key(|model| model.to_ascii_lowercase());
    models.dedup();
    models
}

fn network_error(error: reqwest::Error) -> LlmError {
    LlmError {
        code: if error.is_timeout() {
            "llm.timeout"
        } else {
            "llm.network_failed"
        },
        detail: error.to_string(),
        retryable: true,
    }
}

fn invalid_response(error: reqwest::Error) -> LlmError {
    if error.is_timeout() || error.is_body() {
        return network_error(error);
    }
    LlmError {
        code: "llm.invalid_response",
        detail: error.to_string(),
        retryable: false,
    }
}

async fn response_error(response: reqwest::Response) -> LlmError {
    let status = response.status();
    let value = response.json().await.unwrap_or_else(|_| json!({
        "error": { "message": format!("Gemini returned HTTP {status} without a JSON error body") }
    }));
    status_error(status, &value)
}

fn status_error(status: StatusCode, value: &Value) -> LlmError {
    let detail = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("Gemini request failed")
        .to_owned();
    let (code, retryable) = match status.as_u16() {
        400 => ("llm.invalid_request", false),
        401 | 403 => ("llm.authentication_failed", false),
        404 => ("llm.model_not_found", false),
        408 => ("llm.timeout", true),
        429 => ("llm.rate_limited", true),
        500..=599 => ("llm.provider_unavailable", true),
        _ => ("llm.request_failed", false),
    };
    LlmError {
        code,
        detail,
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse_response(chunks: Vec<Vec<u8>>) -> reqwest::Response {
        let body = reqwest::Body::wrap_stream(futures_util::stream::iter(
            chunks.into_iter().map(Ok::<_, std::io::Error>),
        ));
        tokio_tungstenite::tungstenite::http::Response::builder()
            .header("content-type", "text/event-stream")
            .body(body)
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn rejects_unsuccessful_or_unfinished_gemini_streams() {
        for (event, reason, retryable) in [
            (
                json!({"candidates":[{"finishReason":"MAX_TOKENS"}]}),
                "MAX_TOKENS",
                false,
            ),
            (
                json!({"promptFeedback":{"blockReason":"SAFETY"}}),
                "SAFETY",
                false,
            ),
            (
                json!({"error":{"code":503,"message":"Temporarily unavailable"}}),
                "Temporarily unavailable",
                true,
            ),
            (
                json!({"usageMetadata":{"totalTokenCount":12}}),
                "completion",
                true,
            ),
        ] {
            let partial =
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n";
            let response = sse_response(vec![format!("{partial}data: {event}\n\n").into_bytes()]);
            let error = stream_response(response, &|_| {}).await.unwrap_err();
            assert!(error.detail.contains(reason), "{error:?}");
            assert_eq!(error.retryable, retryable);
        }
    }

    #[tokio::test]
    async fn accepts_complete_gemini_sse_events_across_byte_boundaries() {
        let stream = concat!(
            ": heartbeat\r\n\r\n",
            "data: {\"candidates\":[{\"content\":\n",
            "data: {\"parts\":[{\"text\":\"你好\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n",
            "data: {\"usageMetadata\":{\"totalTokenCount\":12}}\n\n",
        );
        let chunks = stream.as_bytes().iter().map(|byte| vec![*byte]).collect();
        assert_eq!(
            stream_response(sse_response(chunks), &|_| {})
                .await
                .unwrap(),
            "你好"
        );
    }

    #[test]
    fn gemini_thinking_and_output_budgets_follow_model_capabilities() {
        for (model, thinking, expected_config, reserve) in [
            ("gemini-2.5-flash", false, json!({"thinkingBudget":0}), 0),
            (
                "gemini-2.5-flash",
                true,
                json!({"thinkingBudget":1024}),
                1024,
            ),
            ("gemini-2.5-pro", false, json!({"thinkingBudget":128}), 128),
            (
                "models/gemini-3.7-flash",
                false,
                json!({"thinkingLevel":"low"}),
                4096,
            ),
        ] {
            let body = request_body(&LlmRequest {
                model,
                instructions: "Translate",
                input: "Hello",
                max_output_tokens: 256,
                thinking_enabled: thinking,
            });
            assert_eq!(body["generationConfig"]["thinkingConfig"], expected_config);
            assert!(
                body["generationConfig"]["maxOutputTokens"]
                    .as_u64()
                    .unwrap()
                    >= 256 + reserve
            );
        }
    }

    #[test]
    fn maps_request_to_native_gemini_shape() {
        let body = request_body(&LlmRequest {
            model: "gemini-2.5-flash",
            instructions: "Translate",
            input: "こんにちは",
            max_output_tokens: 256,
            thinking_enabled: false,
        });
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Translate");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "こんにちは");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 512);
    }

    #[test]
    fn extracts_text_and_generate_models() {
        let response = json!({
            "candidates": [{ "finishReason": "STOP", "content": { "parts": [
                { "text": "private reasoning", "thought": true },
                { "text": "hello" }
            ] } }]
        });
        assert_eq!(parse_response(&response).unwrap(), "hello");

        let models = extract_models(&json!({ "models": [
            { "name": "models/gemini-2.5-flash", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/embedding-001", "supportedGenerationMethods": ["embedContent"] }
        ] }));
        assert_eq!(models, ["gemini-2.5-flash"]);
    }

    #[test]
    fn json_responses_preserve_failure_reasons_and_do_not_merge_candidates() {
        for reason in ["MAX_TOKENS", "SAFETY", "RECITATION"] {
            let response = json!({"candidates":[{"finishReason":reason,"content":{"parts":[{"text":"partial"}]}}]});
            let error = parse_response(&response).unwrap_err();
            assert_eq!(error.code, "llm.request_failed");
            assert!(error.detail.contains(reason));
            assert!(!error.retryable);
        }
        let empty = parse_response(&json!({"candidates":[{"finishReason":"STOP"}]})).unwrap_err();
        assert!(empty.detail.contains("without text"));
        assert!(empty.retryable);
        let response = json!({"candidates":[
            {"index":1,"finishReason":"MAX_TOKENS","content":{"parts":[{"text":"alternative"}]}},
            {"index":0,"finishReason":"STOP","content":{"parts":[{"text":" translation "}]}}
        ]});
        assert_eq!(parse_response(&response).unwrap(), "translation");
    }

    #[tokio::test]
    async fn non_json_http_errors_preserve_status_and_retryability() {
        for (status, code, retryable) in [
            (503, "llm.provider_unavailable", true),
            (429, "llm.rate_limited", true),
            (403, "llm.authentication_failed", false),
        ] {
            let response = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(status)
                .body("<html>upstream error</html>")
                .unwrap()
                .into();
            let error = response_error(response).await;
            assert_eq!(error.code, code);
            assert_eq!(error.retryable, retryable);
            assert!(error.detail.contains(&status.to_string()));
        }
    }

    #[test]
    fn appends_stream_chunks() {
        let mut output = String::new();
        let mut event_data = String::new();
        let snapshots = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&snapshots);
        let progress = move |text: &str| captured.lock().unwrap().push(text.to_owned());
        process_sse_line(
            br#"data: {"candidates":[{"content":{"parts":[{"text":"hel"}]}}]}"#,
            &mut event_data,
            &mut output,
            &progress,
        )
        .unwrap();
        process_sse_line(b"\n", &mut event_data, &mut output, &progress).unwrap();
        process_sse_line(
            br#"data: {"candidates":[{"content":{"parts":[{"text":" lo"}]}}]}"#,
            &mut event_data,
            &mut output,
            &progress,
        )
        .unwrap();
        process_sse_line(b"\n", &mut event_data, &mut output, &progress).unwrap();
        assert_eq!(output, "hel lo");
        assert_eq!(*snapshots.lock().unwrap(), ["hel", "hel lo"]);
    }
}
