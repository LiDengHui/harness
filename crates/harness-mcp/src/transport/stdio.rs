//! stdio transport: newline-delimited JSON-RPC over a child process's pipes.

use std::collections::BTreeMap;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use harness_core::{HarnessError, Result};

use super::MAX_MESSAGE_BYTES;
use crate::protocol::{response_id, take_result};

/// What the stdout reader hands to whoever is waiting for a response.
#[derive(Debug)]
enum Incoming {
    Frame(Vec<u8>),
    Failed(String),
}

#[derive(Debug)]
pub(crate) struct StdioTransport {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    incoming: Option<UnboundedReceiver<Incoming>>,
    closed: bool,
}

impl StdioTransport {
    pub(crate) fn new(command: String, args: Vec<String>, env: BTreeMap<String, String>) -> Self {
        Self {
            command,
            args,
            env,
            child: None,
            stdin: None,
            incoming: None,
            closed: false,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        !self.closed
    }

    pub(crate) async fn request(&mut self, message: &Value, id: u64) -> Result<Value> {
        self.write(message).await?;
        self.await_response(id).await
    }

    pub(crate) async fn notify(&mut self, message: &Value) -> Result<()> {
        self.write(message).await
    }

    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        self.closed = true;
        // Closing stdin is the polite half-close; the kill below is what stops
        // a server that ignores EOF.
        self.stdin = None;
        self.incoming = None;
        if let Some(mut child) = self.child.take() {
            terminate(&mut child).await;
        }
        Ok(())
    }

    /// Launches the process on the first message rather than at construction, so
    /// a configured-but-unused server costs nothing at startup.
    fn ensure_started(&mut self) -> Result<()> {
        if self.child.is_some() {
            return Ok(());
        }
        if self.closed {
            return Err(HarnessError::Mcp(format!(
                "stdio server `{}` has already been shut down",
                self.command
            )));
        }

        let mut command = Command::new(&self.command);
        command
            .args(&self.args)
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A panic that unwinds past this transport never runs `Drop`, and
            // this is the only thing that reclaims the process on that path.
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|err| {
            HarnessError::Mcp(format!(
                "failed to start stdio server `{}`: {err}",
                self.command
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            HarnessError::Mcp(format!("stdio server `{}` exposed no stdin", self.command))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            HarnessError::Mcp(format!("stdio server `{}` exposed no stdout", self.command))
        })?;

        // stderr is drained rather than captured: a server that logs verbosely
        // would otherwise fill the pipe and block forever on a full buffer.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(drain_stderr(stderr));
        }

        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(read_stdout(stdout, sender));

        self.stdin = Some(stdin);
        self.incoming = Some(receiver);
        self.child = Some(child);
        Ok(())
    }

    async fn write(&mut self, message: &Value) -> Result<()> {
        self.ensure_started()?;
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');

        let stdin = self.stdin.as_mut().ok_or_else(|| {
            HarnessError::Mcp(format!("stdio server `{}` is not writable", self.command))
        })?;
        stdin.write_all(&line).await.map_err(|err| {
            HarnessError::Mcp(format!(
                "stdio server `{}` closed its input: {err}",
                self.command
            ))
        })?;
        stdin.flush().await.map_err(|err| {
            HarnessError::Mcp(format!(
                "stdio server `{}` could not be flushed: {err}",
                self.command
            ))
        })
    }

    async fn await_response(&mut self, id: u64) -> Result<Value> {
        loop {
            let incoming = match self.incoming.as_mut() {
                Some(receiver) => receiver.recv().await,
                None => None,
            };

            let frame = match incoming {
                Some(Incoming::Frame(frame)) => frame,
                Some(Incoming::Failed(message)) => return Err(HarnessError::Mcp(message)),
                None => {
                    return Err(HarnessError::Mcp(format!(
                        "stdio server `{}` stopped before answering request {id}",
                        self.command
                    )))
                }
            };

            let text = String::from_utf8_lossy(&frame);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let message: Value = match serde_json::from_str(text) {
                Ok(message) => message,
                Err(err) => {
                    // A server that logs to stdout must not desynchronise us.
                    tracing::debug!("ignoring a non-JSON line from `{}`: {err}", self.command);
                    continue;
                }
            };

            match response_id(&message) {
                Some(answered) if answered == id => return take_result(&message),
                Some(other) => {
                    tracing::debug!("ignoring a response for the unused request {other}");
                }
                None => tracing::debug!("ignoring a server notification"),
            }
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Dropping a `Child` on Windows does not terminate the process, so the
        // kill is explicit here. It is fire-and-forget because `Drop` cannot
        // await; the OS reclaims the exit status afterwards.
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

/// Kills the child and, on Windows, everything it spawned.
///
/// MCP servers are routinely wrappers (`npx`, a shell, a launcher) whose real
/// work is a grandchild; killing only the direct child would leave that behind
/// holding the pipes open.
async fn terminate(child: &mut Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    let _ = child.kill().await;
}

async fn read_stdout(stdout: ChildStdout, sender: UnboundedSender<Incoming>) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_frame(&mut reader, MAX_MESSAGE_BYTES).await {
            Ok(Some(frame)) => {
                if sender.send(Incoming::Frame(frame)).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(err) => {
                // A waiter blocked on the channel has to learn the reader died,
                // otherwise it waits for a message that can never arrive.
                let _ = sender.send(Incoming::Failed(err.to_string()));
                break;
            }
        }
    }
}

async fn drain_stderr(stderr: ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let text = String::from_utf8_lossy(&buffer[..read]);
                tracing::debug!("server stderr: {}", text.trim_end());
            }
        }
    }
}

/// Reads one newline-delimited frame.
///
/// The size limit is applied to the bytes buffered so far, before they are
/// committed, so an endless line fails loudly instead of exhausting memory.
async fn read_frame<R>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|err| {
            HarnessError::Mcp(format!("reading the server's stdout failed: {err}"))
        })?;

        if available.is_empty() {
            // EOF. A partial trailing line is handed on so the caller reports it
            // as malformed JSON rather than as silence.
            return Ok(if frame.is_empty() { None } else { Some(frame) });
        }

        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len() + position > limit {
                return Err(too_large(limit));
            }
            frame.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            return Ok(Some(frame));
        }

        if frame.len() + available.len() > limit {
            return Err(too_large(limit));
        }
        let consumed = available.len();
        frame.extend_from_slice(available);
        reader.consume(consumed);
    }
}

fn too_large(limit: usize) -> HarnessError {
    HarnessError::Mcp(format!("server message exceeds the {limit} byte limit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn frames(input: &str, limit: usize) -> Result<Vec<String>> {
        let mut reader = BufReader::new(std::io::Cursor::new(input.as_bytes().to_vec()));
        let mut frames = Vec::new();
        while let Some(frame) = read_frame(&mut reader, limit).await? {
            frames.push(String::from_utf8_lossy(&frame).to_string());
        }
        Ok(frames)
    }

    #[tokio::test]
    async fn frames_are_split_on_newlines() {
        let parsed = frames("one\ntwo\n\nthree", 1024).await.unwrap();
        assert_eq!(parsed, vec!["one", "two", "", "three"]);
    }

    #[tokio::test]
    async fn an_oversized_frame_is_rejected() {
        let err = frames("0123456789\n", 4).await.unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");

        let err = frames("0123456789", 4).await.unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    #[tokio::test]
    async fn an_empty_stream_ends_immediately() {
        assert!(frames("", 1024).await.unwrap().is_empty());
    }
}
