//! OpenAI-compatible `/chat/completions` client.
//!
//! Any server that speaks the OpenAI streaming protocol works here: OpenAI
//! itself, Moonshot, DeepSeek, vLLM, llama.cpp and Ollama's `/v1` shim. Two
//! properties matter more than breadth:
//!
//!   * the `reqwest::Client` is built once and reused, so TLS sessions and
//!     connections survive across the many turns of an agent run;
//!   * a request is never retried once output has reached the caller, because
//!     replaying it would emit a second copy of the same answer.
//!
//! Parsing is deliberately separated from transport: the private `StreamAssembler`
//! turns raw SSE lines into events with no I/O, which is what the unit tests drive.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc::UnboundedSender;

use harness_core::{HarnessError, ProviderConfig, Result, TokenUsage, ToolSpec};

use crate::provider::Provider;
use crate::request::{ChatRequest, ProviderEvent, ProviderResponse};
use harness_core::{Message, Role, ToolCall};

/// Base exponential backoff between attempts.
const BACKOFF_BASE: Duration = Duration::from_millis(250);
/// Cap on the exponent so a long retry chain cannot sleep for hours.
const BACKOFF_MAX_SHIFT: u32 = 5;
/// How much of an error body is quoted back in the error message.
const ERROR_BODY_LIMIT: usize = 480;

pub struct OpenAiProvider {
    id: String,
    model: String,
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    /// Validated once at construction so a bad header fails fast.
    custom_headers: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>,
    /// Latched once a model rejects `temperature`, so that wasted round-trip is
    /// paid once per provider instead of once per turn.
    temperature_rejected: std::sync::atomic::AtomicBool,
    max_retries: u32,
}

impl OpenAiProvider {
    pub fn new(
        id: impl Into<String>,
        config: &ProviderConfig,
        model: impl Into<String>,
        api_key: Option<String>,
        timeout: Duration,
        max_retries: u32,
    ) -> Result<Self> {
        let id = id.into();
        let base_url = config.base_url().trim().to_string();
        if base_url.is_empty() {
            return Err(HarnessError::Config(format!(
                "provider `{id}` has an empty base_url"
            )));
        }

        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|err| HarnessError::Provider {
                provider: id.clone(),
                message: format!("failed to build the HTTP client: {err}"),
            })?;

        // Gateways route on headers, so a bad one must fail at construction
        // rather than silently sending a request that gets rejected upstream.
        let mut custom_headers = Vec::with_capacity(config.custom_headers.len());
        for (name, value) in &config.custom_headers {
            let header_name =
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                    HarnessError::Config(format!(
                        "provider `{id}` declares an invalid header name `{name}`"
                    ))
                })?;
            let header_value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| HarnessError::Config(format!(
                    "provider `{id}` header `{name}` has a value that is not valid in an HTTP header"
                )))?;
            custom_headers.push((header_name, header_value));
        }

        Ok(Self {
            endpoint: join_url(&base_url, "/chat/completions"),
            client,
            api_key: api_key.filter(|key| !key.trim().is_empty()),
            custom_headers,
            temperature_rejected: std::sync::atomic::AtomicBool::new(false),
            id,
            model: model.into(),
            max_retries,
        })
    }

    fn failure(&self, message: impl Into<String>) -> HarnessError {
        HarnessError::Provider {
            provider: self.id.clone(),
            message: message.into(),
        }
    }

    async fn stream_once(
        &self,
        request: &ChatRequest,
        model: &str,
        events: &UnboundedSender<ProviderEvent>,
    ) -> std::result::Result<ProviderResponse, AttemptError> {
        let mut builder = self
            .client
            .post(self.endpoint.as_str())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&build_body(model, request));

        if let Some(key) = &self.api_key {
            let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| AttemptError::permanent("API key is not a valid HTTP header value"))?;
            builder = builder.header(reqwest::header::AUTHORIZATION, value);
        }

        for (name, value) in &self.custom_headers {
            builder = builder.header(name.clone(), value.clone());
        }

        let response = builder
            .send()
            .await
            .map_err(|err| AttemptError::transport(err.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AttemptError {
                message: format!("HTTP {status}{}", body_detail(&body)),
                retryable: status.as_u16() == 429 || status.is_server_error(),
                emitted: false,
            });
        }

        let mut assembler = StreamAssembler::default();
        let mut buffer: Vec<u8> = Vec::new();
        let mut emitted = false;
        let mut body = response.bytes_stream();

        'stream: while let Some(next) = body.next().await {
            let chunk = next.map_err(|err| AttemptError {
                message: err.to_string(),
                retryable: true,
                emitted,
            })?;
            buffer.extend_from_slice(&chunk);

            while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = String::from_utf8_lossy(&buffer[..newline])
                    .trim_end_matches('\r')
                    .to_string();
                buffer.drain(..=newline);

                for event in assembler.push_line(&line) {
                    emitted = true;
                    events
                        .send(event)
                        .map_err(|_| AttemptError::sink_closed())?;
                }
                if assembler.is_done() {
                    break 'stream;
                }
            }
        }

        // A well-behaved server terminates every SSE record with a newline, but a
        // truncated final chunk is still worth parsing before giving up on it.
        if !assembler.is_done() && !buffer.is_empty() {
            let line = String::from_utf8_lossy(&buffer).to_string();
            for event in assembler.push_line(&line) {
                events
                    .send(event)
                    .map_err(|_| AttemptError::sink_closed())?;
            }
        }

        let tool_calls = assembler.take_tool_calls();
        for call in &tool_calls {
            events
                .send(ProviderEvent::ToolCall(call.clone()))
                .map_err(|_| AttemptError::sink_closed())?;
        }

        let text = assembler.text().to_string();
        let content = if text.is_empty() && !tool_calls.is_empty() {
            None
        } else {
            Some(text)
        };
        let message = Message {
            role: Role::Assistant,
            content,
            tool_calls: tool_calls.clone(),
            tool_call_id: None,
            name: None,
        };

        Ok(ProviderResponse {
            message,
            tool_calls,
            usage: assembler.usage().unwrap_or_default(),
            finish_reason: assembler.finish_reason().map(str::to_string),
        })
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn stream(
        &self,
        request: ChatRequest,
        events: UnboundedSender<ProviderEvent>,
    ) -> Result<ProviderResponse> {
        let model = if request.model.trim().is_empty() {
            self.model.clone()
        } else {
            request.model.trim().to_string()
        };

        let mut attempt = 0u32;
        let mut request = request;
        let mut dropped_temperature = false;

        // A provider instance outlives a single turn, so remember an earlier
        // refusal instead of spending a rejected round-trip on every turn.
        if self
            .temperature_rejected
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            request.temperature = None;
            dropped_temperature = true;
        }

        loop {
            match self.stream_once(&request, &model, &events).await {
                Ok(response) => return Ok(response),
                Err(failure) => {
                    // Gateways front models that pin their sampling parameters and
                    // reject a `temperature` outright. The agent spec still asks
                    // for it, so drop it once and say so, rather than failing a
                    // run over a knob the model refuses to expose.
                    if !dropped_temperature
                        && request.temperature.is_some()
                        && !failure.emitted
                        && !failure.retryable
                        && failure.message.contains("temperature")
                    {
                        tracing::warn!(
                            provider = %self.id,
                            "model rejected `temperature`; retrying without it"
                        );
                        self.temperature_rejected
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        request.temperature = None;
                        dropped_temperature = true;
                        continue;
                    }

                    if !should_retry(&failure, attempt, self.max_retries) {
                        return Err(self.failure(failure.message));
                    }
                    let delay = backoff_delay(attempt);
                    tracing::warn!(
                        provider = %self.id,
                        attempt = attempt + 1,
                        delay_ms = delay.as_millis() as u64,
                        error = %failure.message,
                        "provider request failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }
}

/// One failed attempt, plus whether it is safe to try again.
#[derive(Debug)]
struct AttemptError {
    message: String,
    retryable: bool,
    /// True once the sink has received part of this attempt's output.
    emitted: bool,
}

impl AttemptError {
    fn transport(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            emitted: false,
        }
    }

    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            emitted: false,
        }
    }

    fn sink_closed() -> Self {
        Self {
            message: "the event sink was closed before the turn finished".to_string(),
            retryable: false,
            emitted: true,
        }
    }
}

fn backoff_delay(attempt: u32) -> Duration {
    BACKOFF_BASE.saturating_mul(1u32 << attempt.min(BACKOFF_MAX_SHIFT))
}

/// A retry is refused once any delta reached the sink: replaying the request
/// would emit a second copy of an answer the caller has already seen.
fn should_retry(failure: &AttemptError, attempt: u32, max_retries: u32) -> bool {
    failure.retryable && !failure.emitted && attempt < max_retries
}

fn body_detail(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let snippet: String = trimmed.chars().take(ERROR_BODY_LIMIT).collect();
    if snippet.len() < trimmed.len() {
        return format!(": {snippet}…");
    }
    format!(": {snippet}")
}

/// Joins a base URL and a path without doubling or dropping the slash.
fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    match (base.is_empty(), path.is_empty()) {
        (true, _) => path.to_string(),
        (false, true) => base.to_string(),
        (false, false) => format!("{base}/{path}"),
    }
}

/// Renders the request body.
///
/// Field order is cosmetic. A provider rebuilds the prompt from the parsed
/// object, so `tools` appearing after `messages` in the JSON does not change
/// what a prefix cache sees; the cache keys on the token sequence, which is
/// decided by the *content* of `messages` and `tools`. The prefix therefore
/// stays cacheable only when that content is stable turn to turn — which is the
/// agent loop's job (keep the leading messages byte-identical, elide from the
/// middle), not a reordering here.
fn build_body(model: &str, request: &ChatRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), Value::String(model.to_string()));
    body.insert(
        "messages".into(),
        Value::Array(request.messages.iter().map(wire_message).collect()),
    );
    body.insert("stream".into(), Value::Bool(true));
    body.insert("stream_options".into(), json!({ "include_usage": true }));

    if let Some(temperature) = request.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = request.max_tokens {
        body.insert("max_tokens".into(), json!(max_tokens));
    }
    // Absent rather than empty: the key only means something to a model that
    // reasons, and an endpoint that rejects an unknown field would otherwise
    // break exactly the models that have no use for it. The value is validated
    // before it gets here, because the gateway answers an unrecognised level
    // with an opaque upstream failure rather than a usable message.
    if let Some(effort) = &request.reasoning_effort {
        body.insert("reasoning_effort".into(), Value::String(effort.clone()));
    }
    // Absent rather than empty: an endpoint that rejects `tools: []` would
    // otherwise break exactly the turns that should cost the least.
    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(request.tools.iter().map(wire_tool_spec).collect()),
        );
    }

    Value::Object(body)
}

fn wire_message(message: &Message) -> Value {
    let mut out = Map::new();
    out.insert("role".into(), json!(message.role));
    out.insert(
        "content".into(),
        match &message.content {
            Some(content) => Value::String(content.clone()),
            None => Value::Null,
        },
    );
    if !message.tool_calls.is_empty() {
        out.insert(
            "tool_calls".into(),
            Value::Array(message.tool_calls.iter().map(wire_tool_call).collect()),
        );
    }
    if let Some(id) = &message.tool_call_id {
        out.insert("tool_call_id".into(), Value::String(id.clone()));
    }
    if let Some(name) = &message.name {
        out.insert("name".into(), Value::String(name.clone()));
    }
    Value::Object(out)
}

fn wire_tool_call(call: &ToolCall) -> Value {
    // The wire format wants the arguments as a JSON *string*, not a JSON object.
    let arguments = match &call.arguments {
        Value::String(raw) => raw.clone(),
        other => other.to_string(),
    };
    json!({
        "id": call.id,
        "type": "function",
        "function": { "name": call.name, "arguments": arguments },
    })
}

fn wire_tool_spec(spec: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": spec.name,
            "description": spec.description,
            "parameters": spec.parameters,
        },
    })
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Reassembles an OpenAI SSE delta stream into events and a final turn.
///
/// Pure by construction: it never touches the network, so the streaming
/// protocol can be tested against canned chunks.
#[derive(Debug, Default)]
struct StreamAssembler {
    text: String,
    tool_calls: BTreeMap<usize, PartialToolCall>,
    usage: Option<TokenUsage>,
    finish_reason: Option<String>,
    done: bool,
}

impl StreamAssembler {
    /// Feeds one raw SSE line and returns the events that line produced.
    fn push_line(&mut self, line: &str) -> Vec<ProviderEvent> {
        let Some(payload) = line.trim_end_matches(['\r', '\n']).strip_prefix("data:") else {
            return Vec::new();
        };
        let payload = payload.trim();
        if payload.is_empty() {
            return Vec::new();
        }
        if payload == "[DONE]" {
            self.done = true;
            return Vec::new();
        }

        match serde_json::from_str::<Value>(payload) {
            Ok(chunk) => self.push_chunk(&chunk),
            Err(err) => {
                tracing::debug!(%err, "skipping unparsable SSE payload");
                Vec::new()
            }
        }
    }

    fn push_chunk(&mut self, chunk: &Value) -> Vec<ProviderEvent> {
        let mut events = Vec::new();

        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            let reported = TokenUsage::new(
                usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            self.usage = Some(reported);
            events.push(ProviderEvent::Usage(reported));
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return events;
        };

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_string());
        }

        let Some(delta) = choice.get("delta") else {
            return events;
        };

        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                self.text.push_str(text);
                events.push(ProviderEvent::TextDelta(text.to_string()));
            }
        }

        if let Some(thinking) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !thinking.is_empty() {
                events.push(ProviderEvent::ThinkingDelta(thinking.to_string()));
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (position, call) in calls.iter().enumerate() {
                self.absorb_tool_call(position, call);
            }
        }

        events
    }

    fn absorb_tool_call(&mut self, position: usize, call: &Value) {
        let index = call
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| index as usize)
            .unwrap_or(position);
        let entry = self.tool_calls.entry(index).or_default();

        if let Some(id) = call.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                entry.id = id.to_string();
            }
        }

        let Some(function) = call.get("function") else {
            return;
        };
        if let Some(name) = function.get("name").and_then(Value::as_str) {
            if !name.is_empty() {
                entry.name.push_str(name);
            }
        }
        match function.get("arguments") {
            Some(Value::String(fragment)) => entry.arguments.push_str(fragment),
            // Some OpenAI-compatible servers answer a streaming request with a
            // complete arguments object instead of string fragments.
            Some(Value::Null) | None => {}
            Some(other) => entry.arguments.push_str(&other.to_string()),
        }
    }

    /// Call after the stream ends: a tool call is only complete once every
    /// fragment has arrived.
    fn take_tool_calls(&mut self) -> Vec<ToolCall> {
        std::mem::take(&mut self.tool_calls)
            .into_iter()
            .filter_map(|(index, partial)| {
                if partial.name.is_empty() {
                    tracing::warn!(index, "dropping tool-call delta with no function name");
                    return None;
                }
                let id = if partial.id.is_empty() {
                    format!("call_{index}")
                } else {
                    partial.id
                };
                let arguments = serde_json::from_str::<Value>(&partial.arguments)
                    .unwrap_or(Value::String(partial.arguments));
                Some(ToolCall {
                    id,
                    name: partial.name,
                    arguments,
                })
            })
            .collect()
    }

    fn text(&self) -> &str {
        &self.text
    }

    fn usage(&self) -> Option<TokenUsage> {
        self.usage
    }

    fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::object_schema;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn collect(lines: &[&str]) -> (Vec<ProviderEvent>, StreamAssembler) {
        let mut assembler = StreamAssembler::default();
        let mut events = Vec::new();
        for line in lines {
            events.extend(assembler.push_line(line));
        }
        (events, assembler)
    }

    fn text_of(events: &[ProviderEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::TextDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn thinking_of(events: &[ProviderEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn text_deltas_arrive_in_order_and_assemble() {
        let (events, assembler) = collect(&[
            r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}"#,
            "",
            r#"data: {"choices":[{"index":0,"delta":{"content":"lo "}}]}"#,
            ": keep-alive",
            r#"data: {"choices":[{"index":0,"delta":{"content":"world"},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ]);

        assert_eq!(
            events,
            vec![
                ProviderEvent::TextDelta("Hel".into()),
                ProviderEvent::TextDelta("lo ".into()),
                ProviderEvent::TextDelta("world".into()),
            ]
        );
        assert_eq!(text_of(&events), "Hello world");
        assert_eq!(assembler.text(), "Hello world");
        assert_eq!(assembler.finish_reason(), Some("stop"));
        assert!(assembler.is_done());
    }

    #[test]
    fn reasoning_content_becomes_a_thinking_delta() {
        let (events, assembler) = collect(&[
            r#"data: {"choices":[{"index":0,"delta":{"reasoning_content":"weighing options"}}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"content":"the answer"}}]}"#,
            "data: [DONE]",
        ]);

        assert_eq!(
            events,
            vec![
                ProviderEvent::ThinkingDelta("weighing options".into()),
                ProviderEvent::TextDelta("the answer".into()),
            ]
        );
        assert_eq!(thinking_of(&events), "weighing options");
        assert_eq!(text_of(&events), "the answer");
        assert_eq!(assembler.text(), "the answer");
    }

    #[test]
    fn fragmented_tool_arguments_reassemble_into_json() {
        let (events, mut assembler) = collect(&[
            r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"src/"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"main.rs\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ]);

        // Tool-call fragments are buffered, never streamed as partial events.
        assert!(events.is_empty());

        let calls = assembler.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, json!({ "path": "src/main.rs" }));
        assert_eq!(assembler.finish_reason(), Some("tool_calls"));
        // A second call drains nothing.
        assert!(assembler.take_tool_calls().is_empty());
    }

    #[test]
    fn parallel_tool_calls_keep_their_indices() {
        let (_, mut assembler) = collect(&[
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"one","arguments":"{}"}},{"index":1,"id":"b","function":{"name":"two","arguments":"{}"}}]}}]}"#,
            "data: [DONE]",
        ]);

        let calls = assembler.take_tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "one");
        assert_eq!(calls[1].name, "two");
    }

    #[test]
    fn invalid_tool_arguments_fall_back_to_a_string() {
        let (_, mut assembler) = collect(&[
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_2","function":{"name":"shell","arguments":"not json at all"}}]}}]}"#,
            "data: [DONE]",
        ]);

        let calls = assembler.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, Value::String("not json at all".into()));
    }

    #[test]
    fn a_tool_call_without_a_name_is_dropped() {
        let (_, mut assembler) = collect(&[
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}"#,
            "data: [DONE]",
        ]);

        assert!(assembler.take_tool_calls().is_empty());
    }

    #[test]
    fn usage_chunk_reports_tokens() {
        let (events, assembler) = collect(&[
            r#"data: {"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18}}"#,
            "data: [DONE]",
        ]);

        assert_eq!(events, vec![ProviderEvent::Usage(TokenUsage::new(11, 7))]);
        assert_eq!(assembler.usage(), Some(TokenUsage::new(11, 7)));
    }

    #[test]
    fn unparsable_payloads_are_skipped() {
        let (events, assembler) = collect(&["data: {oops", "garbage", "data: [DONE]"]);
        assert!(events.is_empty());
        assert_eq!(assembler.text(), "");
        assert!(assembler.is_done());
    }

    #[test]
    fn joined_urls_never_double_or_drop_the_slash() {
        let expected = "https://api.openai.com/v1/chat/completions";
        assert_eq!(
            join_url("https://api.openai.com/v1", "/chat/completions"),
            expected
        );
        assert_eq!(
            join_url("https://api.openai.com/v1/", "/chat/completions"),
            expected
        );
        assert_eq!(
            join_url("https://api.openai.com/v1///", "chat/completions"),
            expected
        );
        assert_eq!(
            join_url("http://localhost:11434/v1/", "/chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(join_url("", "/chat/completions"), "chat/completions");
    }

    #[test]
    fn an_empty_tool_list_is_omitted_from_the_body() {
        let request = ChatRequest::new("gpt-4o-mini", vec![Message::user("hi")]);
        let body = build_body("gpt-4o-mini", &request);

        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert!(body.get("tools").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn a_reasoning_effort_is_sent_only_when_it_is_set() {
        let mut request = ChatRequest::new("m", vec![Message::user("hi")]);
        request.reasoning_effort = Some("max".to_string());

        let body = build_body("m", &request);
        assert_eq!(body["reasoning_effort"], "max");

        // An unset value omits the key entirely rather than sending `null` or an
        // empty string, so a model that does not reason is unaffected.
        request.reasoning_effort = None;
        let body = build_body("m", &request);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn tools_are_advertised_in_the_function_envelope() {
        let mut request = ChatRequest::new("m", vec![Message::user("hi")]);
        request.tools = vec![ToolSpec::new(
            "read_file",
            "reads a file",
            object_schema(json!({ "path": { "type": "string" } }), &["path"]),
        )];
        request.temperature = Some(0.2);
        request.max_tokens = Some(256);

        let body = build_body("m", &request);
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "read_file");
        assert_eq!(tool["function"]["description"], "reads a file");
        assert_eq!(tool["function"]["parameters"]["required"][0], "path");
        assert_eq!(body["max_tokens"], 256);
        assert!(body["temperature"].is_number());
    }

    #[test]
    fn tool_call_arguments_are_stringified_on_the_wire() {
        let message = Message::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: json!({ "path": "src/main.rs" }),
        }]);

        let encoded = wire_message(&message);
        assert_eq!(encoded["role"], "assistant");
        assert!(encoded["content"].is_null());
        assert_eq!(encoded["tool_calls"][0]["type"], "function");
        assert_eq!(encoded["tool_calls"][0]["function"]["name"], "read_file");
        assert_eq!(
            encoded["tool_calls"][0]["function"]["arguments"],
            serde_json::to_string(&json!({ "path": "src/main.rs" })).unwrap()
        );

        let result = wire_message(&Message::tool_result("call_1", "fn main() {}"));
        assert_eq!(result["role"], "tool");
        assert_eq!(result["tool_call_id"], "call_1");
    }

    #[test]
    fn backoff_grows_and_stops_growing() {
        assert_eq!(backoff_delay(0), Duration::from_millis(250));
        assert_eq!(backoff_delay(1), Duration::from_millis(500));
        assert_eq!(backoff_delay(2), Duration::from_millis(1000));
        assert_eq!(backoff_delay(5), Duration::from_millis(8000));
        assert_eq!(backoff_delay(50), Duration::from_millis(8000));
    }

    #[test]
    fn provider_rejects_an_empty_base_url() {
        let config = ProviderConfig {
            base_url: Some("   ".into()),
            ..Default::default()
        };
        let result = OpenAiProvider::new("local", &config, "m", None, Duration::from_secs(1), 0);
        let Err(err) = result else {
            panic!("an empty base_url must be rejected");
        };
        assert!(matches!(err, HarnessError::Config(_)));
    }

    #[test]
    fn retry_is_refused_once_output_has_been_emitted() {
        let clean = AttemptError::transport("connection reset");
        assert!(should_retry(&clean, 0, 3));
        assert!(should_retry(&clean, 2, 3));
        assert!(!should_retry(&clean, 3, 3));

        let mid_stream = AttemptError {
            emitted: true,
            ..AttemptError::transport("connection reset")
        };
        assert!(!should_retry(&mid_stream, 0, 3));

        assert!(!should_retry(&AttemptError::permanent("bad key"), 0, 3));
        assert!(!should_retry(&AttemptError::sink_closed(), 0, 3));
    }

    // ---- end-to-end over a loopback socket ----------------------------------

    struct LocalServer {
        base_url: String,
        requests: UnboundedReceiver<String>,
    }

    /// Serves one canned HTTP response per connection, reporting each request
    /// body back so a test can prove whether a retry happened.
    async fn serve(responses: Vec<String>) -> LocalServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, requests) = unbounded_channel();

        tokio::spawn(async move {
            for response in responses {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let request = read_request(&mut socket).await;
                if tx.send(request).is_err() {
                    return;
                }
                if socket.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
                let _ = socket.shutdown().await;
            }
        });

        LocalServer {
            base_url: format!("http://{addr}/v1"),
            requests,
        }
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        let header_end = loop {
            let read = socket.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                return String::from_utf8_lossy(&buffer).to_string();
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                break position + 4;
            }
        };

        let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let mut content_length = 0usize;
        for line in head.lines() {
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        while buffer.len() < header_end + content_length {
            let read = socket.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }

        String::from_utf8_lossy(&buffer).to_string()
    }

    fn sse_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Promises the full body but delivers only `delivered` bytes before closing,
    /// which is exactly what a dropped connection looks like mid-answer.
    fn truncated_response(body: &str, delivered: usize) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            &body[..delivered]
        )
    }

    fn error_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn header_of(request: &str, name: &str) -> Option<String> {
        request
            .lines()
            .take_while(|line| !line.is_empty())
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_string())
            })
    }

    fn body_of(request: &str) -> Value {
        let start = request
            .find("\r\n\r\n")
            .map(|position| position + 4)
            .unwrap_or(0);
        serde_json::from_str(&request[start..]).unwrap()
    }

    fn texts(events: &[ProviderEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::TextDelta(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    const TEXT_STREAM: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );

    fn provider_for(
        server: &LocalServer,
        api_key: Option<String>,
        max_retries: u32,
    ) -> OpenAiProvider {
        let config = ProviderConfig {
            base_url: Some(server.base_url.clone()),
            ..Default::default()
        };
        OpenAiProvider::new(
            "local",
            &config,
            "test-model",
            api_key,
            Duration::from_secs(5),
            max_retries,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn streams_a_turn_from_a_local_endpoint() {
        let mut server = serve(vec![sse_response(TEXT_STREAM)]).await;
        let provider = provider_for(&server, Some("sk-test".into()), 0);
        let (tx, mut rx) = unbounded_channel();

        let mut request = ChatRequest::new("test-model", vec![Message::user("hi")]);
        request.tools = vec![ToolSpec::new(
            "read_file",
            "reads",
            json!({ "type": "object" }),
        )];
        request.temperature = Some(0.5);

        let response = provider.stream(request, tx).await.unwrap();

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        assert_eq!(texts(&events), vec!["Hel", "lo"]);
        assert!(events.contains(&ProviderEvent::Usage(TokenUsage::new(5, 2))));

        assert_eq!(response.message.text(), "Hello");
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
        assert_eq!(response.usage, TokenUsage::new(5, 2));

        let sent = server.requests.try_recv().unwrap();
        assert!(
            sent.starts_with("POST /v1/chat/completions "),
            "unexpected request line: {sent}"
        );
        assert_eq!(
            header_of(&sent, "authorization").as_deref(),
            Some("Bearer sk-test")
        );
        assert_eq!(
            header_of(&sent, "accept").as_deref(),
            Some("text/event-stream")
        );

        let body = body_of(&sent);
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert!(body["temperature"].is_number());
        assert!(server.requests.try_recv().is_err(), "no retry expected");
    }

    #[tokio::test]
    async fn a_local_endpoint_works_without_an_api_key() {
        let mut server = serve(vec![sse_response(TEXT_STREAM)]).await;
        let provider = provider_for(&server, None, 0);
        let (tx, _rx) = unbounded_channel();

        provider
            .stream(
                ChatRequest::new("test-model", vec![Message::user("hi")]),
                tx,
            )
            .await
            .unwrap();

        let sent = server.requests.try_recv().unwrap();
        assert!(header_of(&sent, "authorization").is_none());
    }

    #[tokio::test]
    async fn a_failing_status_is_retried_before_any_output() {
        let mut server = serve(vec![
            error_response("500 Internal Server Error", "upstream is unwell"),
            sse_response(TEXT_STREAM),
        ])
        .await;
        let provider = provider_for(&server, None, 2);
        let (tx, _rx) = unbounded_channel();

        let response = provider
            .stream(
                ChatRequest::new("test-model", vec![Message::user("hi")]),
                tx,
            )
            .await
            .unwrap();

        assert_eq!(response.message.text(), "Hello");
        assert!(server.requests.try_recv().is_ok(), "first attempt");
        assert!(server.requests.try_recv().is_ok(), "retry attempt");
        assert!(server.requests.try_recv().is_err(), "no third attempt");
    }

    #[tokio::test]
    async fn a_status_error_is_reported_when_retries_are_exhausted() {
        let server = serve(vec![
            error_response("503 Service Unavailable", "try later"),
            error_response("503 Service Unavailable", "try later"),
        ])
        .await;
        let provider = provider_for(&server, None, 1);
        let (tx, _rx) = unbounded_channel();

        let err = provider
            .stream(
                ChatRequest::new("test-model", vec![Message::user("hi")]),
                tx,
            )
            .await
            .unwrap_err();

        match err {
            HarnessError::Provider { provider, message } => {
                assert_eq!(provider, "local");
                assert!(message.contains("503"), "{message}");
                assert!(message.contains("try later"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn a_dropped_connection_mid_stream_is_not_retried() {
        let full = TEXT_STREAM;
        let delivered = full.find("finish_reason").unwrap();
        let mut server = serve(vec![
            truncated_response(full, delivered),
            sse_response(TEXT_STREAM),
        ])
        .await;
        let provider = provider_for(&server, None, 3);
        let (tx, mut rx) = unbounded_channel();

        let result = provider
            .stream(
                ChatRequest::new("test-model", vec![Message::user("hi")]),
                tx,
            )
            .await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        assert_eq!(texts(&events), vec!["Hel", "lo"], "partial output was seen");
        assert!(matches!(result, Err(HarnessError::Provider { .. })));

        assert!(server.requests.try_recv().is_ok(), "first attempt");
        assert!(
            server.requests.try_recv().is_err(),
            "a partial turn must never be replayed"
        );
    }
}
