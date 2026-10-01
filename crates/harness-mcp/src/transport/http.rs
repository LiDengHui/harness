//! Streamable HTTP transport: one JSON-RPC message per POST.

use std::time::Duration;

use serde_json::Value;

use harness_core::{HarnessError, Result};

use super::MAX_MESSAGE_BYTES;
use crate::protocol::{response_id, take_result};

/// A tool call can legitimately be slow — a build, a long query — so this is
/// generous. It exists to bound a hung server, not to hurry a working one.
const REQUEST_TIMEOUT_SECS: u64 = 300;

#[derive(Debug)]
pub(crate) struct HttpTransport {
    /// One client per server, so successive calls reuse the connection.
    client: reqwest::Client,
    url: String,
    closed: bool,
}

impl HttpTransport {
    /// Builds the client. No request is sent, so an offline server only shows up
    /// when something is first asked of it.
    pub(crate) fn new(url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .map_err(|err| {
                HarnessError::Mcp(format!("could not build an HTTP client for `{url}`: {err}"))
            })?;
        Ok(Self {
            client,
            url: url.to_string(),
            closed: false,
        })
    }

    pub(crate) fn is_open(&self) -> bool {
        !self.closed
    }

    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        self.closed = true;
        Ok(())
    }

    pub(crate) async fn request(&mut self, message: &Value, id: u64) -> Result<Value> {
        let response = self.post(message).await?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if content_type.contains("text/event-stream") {
            return self.read_stream(response, id).await;
        }

        let body = response
            .text()
            .await
            .map_err(|err| self.failure("reading the response body", &err))?;
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(HarnessError::Mcp(format!(
                "mcp server `{}` answered with more than {MAX_MESSAGE_BYTES} bytes",
                self.url
            )));
        }

        let value: Value = serde_json::from_str(&body).map_err(|err| {
            HarnessError::Mcp(format!(
                "mcp server `{}` returned a body that is not JSON: {err}",
                self.url
            ))
        })?;

        match response_id(&value) {
            Some(answered) if answered != id => Err(HarnessError::Mcp(format!(
                "mcp server `{}` answered request {answered} instead of {id}",
                self.url
            ))),
            _ => take_result(&value),
        }
    }

    /// A notification is a POST whose body no one reads.
    pub(crate) async fn notify(&mut self, message: &Value) -> Result<()> {
        let response = self.post(message).await?;
        // Draining the body returns the connection to the pool; a response
        // dropped mid-body would be closed instead. A stream is left alone
        // because draining it would wait for the server to finish sending.
        let is_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("text/event-stream");
        if !is_stream {
            let _ = response.bytes().await;
        }
        Ok(())
    }

    async fn post(&self, message: &Value) -> Result<reqwest::Response> {
        let response = self
            .client
            .post(&self.url)
            // Some servers only open an event stream for a client that says it
            // accepts one.
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .json(message)
            .send()
            .await
            .map_err(|err| self.failure("sending the request", &err))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(HarnessError::Mcp(format!(
                "mcp server `{}` returned {status}: {}",
                self.url,
                body.trim()
            )));
        }
        Ok(response)
    }

    /// Reads an SSE answer, matching frames by `id`.
    ///
    /// The stream may carry notifications and other requests' replies before
    /// ours, and it is read incrementally so the connection can be dropped as
    /// soon as the answer arrives rather than when the server closes it.
    async fn read_stream(&mut self, mut response: reqwest::Response, id: u64) -> Result<Value> {
        let mut decoder = SseDecoder::default();
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(err) => return Err(self.failure("reading the event stream", &err)),
            };
            for payload in decoder.push(&chunk)? {
                if let Some(result) = matching_frame(&payload, id) {
                    return result;
                }
            }
        }
        for payload in decoder.finish() {
            if let Some(result) = matching_frame(&payload, id) {
                return result;
            }
        }
        Err(HarnessError::Mcp(format!(
            "mcp server `{}` closed the event stream without answering request {id}",
            self.url
        )))
    }

    /// `reqwest::Error`'s own message hides the cause; "connection refused" is
    /// what an operator needs to read.
    fn failure(&self, doing: &str, err: &reqwest::Error) -> HarnessError {
        let mut message = err.to_string();
        let mut source = std::error::Error::source(err);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        HarnessError::Mcp(format!(
            "mcp server `{}`: {doing} failed: {message}",
            self.url
        ))
    }
}

/// Parses one SSE payload and keeps it only when it answers `id`.
fn matching_frame(payload: &str, id: u64) -> Option<Result<Value>> {
    let value: Value = serde_json::from_str(payload).ok()?;
    if response_id(&value) != Some(id) {
        return None;
    }
    Some(take_result(&value))
}

/// Incremental `text/event-stream` reader.
///
/// Frames are cut on the blank line that terminates an event, never on a
/// transport chunk boundary: the two have nothing to do with each other.
#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>> {
        // A carriage return is never meaningful inside a `data:` payload (JSON
        // escapes it), so normalising here lets the frame split look for `\n\n`
        // alone.
        self.buffer
            .extend(chunk.iter().copied().filter(|byte| *byte != b'\r'));
        if self.buffer.len() > MAX_MESSAGE_BYTES {
            return Err(HarnessError::Mcp(format!(
                "event stream exceeded {MAX_MESSAGE_BYTES} bytes"
            )));
        }

        let mut payloads = Vec::new();
        while let Some(position) = blank_line(&self.buffer) {
            let block: Vec<u8> = self.buffer.drain(..position + 2).collect();
            payloads.extend(data_of(&block));
        }
        Ok(payloads)
    }

    /// Whatever is left when the stream ends without a trailing blank line.
    fn finish(&mut self) -> Vec<String> {
        let block = std::mem::take(&mut self.buffer);
        data_of(&block)
    }
}

fn blank_line(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|pair| pair == b"\n\n")
}

/// The spec defines an event's `data:` lines as one payload joined by newlines.
fn data_of(block: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(block);
    let mut data: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if data.is_empty() {
        Vec::new()
    } else {
        vec![data.join("\n")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(chunks: &[&str]) -> Vec<String> {
        let mut decoder = SseDecoder::default();
        let mut payloads = Vec::new();
        for chunk in chunks {
            payloads.extend(decoder.push(chunk.as_bytes()).unwrap());
        }
        payloads.extend(decoder.finish());
        payloads
    }

    #[test]
    fn an_event_is_emitted_only_once_it_is_complete() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: {\"id\":1}").unwrap().is_empty());
        assert!(decoder.push(b"\n").unwrap().is_empty());
        assert_eq!(decoder.push(b"\n").unwrap(), vec!["{\"id\":1}".to_string()]);
    }

    #[test]
    fn frames_survive_arbitrary_chunk_boundaries() {
        assert_eq!(
            decoded(&[
                "data: {\"a\":",
                "1}\n\n",
                ":keep-alive\n\n",
                "data: {\"b\":2}\n\n"
            ]),
            vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]
        );
        assert_eq!(decoded(&["data: {\"a\":1}\ndata: {\"b\":2}\n\n"]).len(), 1);
        assert_eq!(
            decoded(&["data: {\"a\":1}\ndata: {\"b\":2}\n\n"])[0],
            "{\"a\":1}\n{\"b\":2}"
        );
    }

    #[test]
    fn carriage_returns_do_not_break_the_frame_split() {
        assert_eq!(
            decoded(&["event: message\r\ndata: {\"id\":1}\r\n\r\n"]),
            vec!["{\"id\":1}".to_string()]
        );
    }

    #[test]
    fn a_trailing_frame_without_a_blank_line_is_still_returned() {
        assert_eq!(
            decoded(&["data: {\"id\":1}"]),
            vec!["{\"id\":1}".to_string()]
        );
    }

    #[test]
    fn only_the_matching_id_is_accepted() {
        let ok = matching_frame("{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"n\":1}}", 4).unwrap();
        assert_eq!(ok.unwrap()["n"], 1);

        assert!(matching_frame("{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{}}", 4).is_none());
        assert!(matching_frame("not json", 4).is_none());

        let failure = matching_frame(
            "{\"id\":4,\"error\":{\"code\":-32601,\"message\":\"nope\"}}",
            4,
        )
        .unwrap()
        .unwrap_err();
        assert!(failure.to_string().contains("-32601"), "{failure}");
    }
}
