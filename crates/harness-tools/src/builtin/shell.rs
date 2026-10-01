//! `shell`: command execution with merged output, a timeout, and abort support.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use harness_core::{HarnessError, Result, ToolsConfig};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::builtin::truncate_payload;
use crate::{ToolContext, ToolOutput};

// The generated `Tool` impl names the trait by absolute path, so only the tests
// which call `.call()` on the tool need it in scope; the tests also assert on a
// null exit code through `Value::Null`.
#[cfg(test)]
use crate::Tool;
#[cfg(test)]
use serde_json::Value;

/// How often the abort flag is polled while a child runs.
const ABORT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long to keep draining pipes after the child is known to be gone.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

#[harness_macros::tool(
    name = "shell",
    description = "Run a shell command in the workspace with stdout and stderr merged. Prefer `read_file`, `grep` and `list_dir` for reading and searching; use this for builds, tests and other programs. The result states a non-zero exit code, timeout or abort, and truncated output says how to narrow it. Fails when the command cannot start; keep `cwd` inside the workspace and raise `timeout_secs` for long runs.",
    type_name = "Shell"
)]
#[derive(serde::Deserialize)]
pub struct ShellArgs {
    /// Command line to run through the shell.
    command: String,
    /// Directory relative to the workspace root. Defaults to the root.
    cwd: Option<String>,
    /// Wall-clock limit. Defaults to the configured shell timeout.
    timeout_secs: Option<u64>,
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let candidate = dir.join(format!("{name}.exe"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The program and switch the command is passed through, e.g. `sh -c <command>`.
fn shell_invocation(config: &ToolsConfig) -> (OsString, &'static str) {
    if let Some(program) = config
        .shell_program
        .as_deref()
        .map(str::trim)
        .filter(|program| !program.is_empty())
    {
        return (OsString::from(program), "-c");
    }
    if let Some(shell) = which("sh") {
        return (shell.into_os_string(), "-c");
    }
    #[cfg(windows)]
    let fallback = (OsString::from("cmd"), "/C");
    #[cfg(not(windows))]
    let fallback = (OsString::from("sh"), "-c");
    fallback
}

async fn pump<R>(mut stream: R, sender: UnboundedSender<String>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut buffer = [0u8; 8192];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let chunk = String::from_utf8_lossy(&buffer[..read]).into_owned();
                if sender.send(chunk).is_err() {
                    break;
                }
            }
        }
    }
}

/// Kills the child and, on Windows, everything it spawned.
///
/// `sh -c "long_command"` forks, so killing only the shell leaves the real work
/// running with the output pipes still open — the readers would never see EOF.
/// On Unix a tree kill would need a process group, which std cannot create, so
/// there the child alone is killed.
async fn terminate(child: &mut tokio::process::Child) {
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

/// The one line that states what happened to the process, for every outcome
/// other than "ran and exited 0".
///
/// `metadata` carries the same facts for programmatic consumers, but the agent
/// loop forwards only `content` to the model, so the outcome has to be text.
fn shell_status(
    command: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    aborted: bool,
    timeout_secs: u64,
) -> Option<String> {
    if timed_out {
        return Some(format!(
            "[shell: `{command}` timed out after {timeout_secs}s and was killed. \
             Retry with a larger `timeout_secs`, or narrow the command.]"
        ));
    }
    if aborted {
        return Some(format!(
            "[shell: `{command}` was aborted before it completed.]"
        ));
    }
    match exit_code {
        Some(0) => None,
        Some(code) => Some(format!(
            "[shell: `{command}` exited with code {code}. The output below is what it printed; \
             check the command and its arguments.]"
        )),
        None => Some(format!(
            "[shell: `{command}` ended without reporting an exit code.]"
        )),
    }
}

fn shell_output(
    content: String,
    exit_code: Option<i32>,
    timed_out: bool,
    aborted: bool,
    truncated: bool,
    elapsed: Duration,
) -> ToolOutput {
    let is_error = timed_out || aborted || exit_code != Some(0);
    let metadata = json!({
        "exit_code": exit_code,
        "timed_out": timed_out,
        "aborted": aborted,
        "duration_ms": elapsed.as_millis() as u64,
        "truncated": truncated,
    });
    let output = if is_error {
        ToolOutput::error(content)
    } else {
        ToolOutput::text(content)
    };
    output.with_metadata(metadata)
}

async fn shell(args: ShellArgs, ctx: &ToolContext) -> Result<ToolOutput> {
    let command = args.command;
    let cwd = args.cwd;
    let timeout_secs = args.timeout_secs.unwrap_or(ctx.config.shell_timeout_secs);

    let started = Instant::now();
    let working_dir = match &cwd {
        Some(dir) => ctx.resolve(Path::new(dir))?,
        None => ctx.resolve(Path::new("."))?,
    };

    if ctx.abort.is_aborted() {
        return Ok(shell_output(
            "[shell: aborted before the command started; nothing ran.]".to_string(),
            None,
            false,
            true,
            false,
            started.elapsed(),
        ));
    }

    let (program, switch) = shell_invocation(&ctx.config);
    let mut invocation = Command::new(&program);
    invocation
        .arg(switch)
        .arg(&command)
        .current_dir(&working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    ctx.emit_progress(format!("running: {command}"));
    let mut child = invocation.spawn().map_err(|err| {
        HarnessError::Tool(format!(
            "shell: cannot start `{}` for command `{command}`: {err}. \
             Check that the program exists and is on PATH, or point `shell_program` at a shell that has it.",
            program.to_string_lossy()
        ))
    })?;

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(tokio::spawn(pump(stdout, sender.clone())));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(tokio::spawn(pump(stderr, sender.clone())));
    }
    drop(sender);

    let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs.max(1)));
    tokio::pin!(deadline);
    // The abort flag is a bare atomic with no notification attached, so it
    // has to be polled rather than awaited alongside the child.
    let mut poll = tokio::time::interval(ABORT_POLL_INTERVAL);

    let mut output = String::new();
    let mut readers_done = false;
    let mut exit_code = None;
    let mut timed_out = false;
    let mut aborted = false;

    // The child is polled with `try_wait` rather than awaited: a `wait()`
    // future would hold a mutable borrow of the child across the whole
    // `select!`, and the timeout/abort branches need `kill()` on it.
    loop {
        tokio::select! {
            chunk = receiver.recv(), if !readers_done => match chunk {
                Some(chunk) => output.push_str(&chunk),
                None => readers_done = true,
            },
            _ = &mut deadline, if !timed_out => {
                timed_out = true;
                terminate(&mut child).await;
            }
            _ = poll.tick() => {
                if ctx.abort.is_aborted() && !aborted {
                    aborted = true;
                    terminate(&mut child).await;
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !timed_out && !aborted {
                            exit_code = status.code();
                        }
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        }
    }

    // The child is gone by now, so a bounded drain is enough: waiting on a
    // survivor that inherited a pipe write end would hang the call.
    for reader in readers {
        let _ = tokio::time::timeout(DRAIN_GRACE, reader).await;
    }
    while let Ok(chunk) = receiver.try_recv() {
        output.push_str(&chunk);
    }

    let cut = truncate_payload(&output, ctx.config.max_output_bytes);
    let mut content = cut.content(
        "the command printed more than fits; re-run it with a filter (for example `| tail -n 100`) \
         or redirect the output to a file and read that with read_file.",
    );
    if let Some(status) = shell_status(&command, exit_code, timed_out, aborted, timeout_secs) {
        content = if content.is_empty() {
            status
        } else {
            format!("{status}\n{content}")
        };
    }

    Ok(shell_output(
        content,
        exit_code,
        timed_out,
        aborted,
        cut.truncated,
        started.elapsed(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::builtin::test_support::context;

    /// `echo` and `exit` behave the same in `sh` and `cmd`, but sleeping and
    /// file dumps do not.
    fn uses_cmd() -> bool {
        let (program, _) = shell_invocation(&ToolsConfig::default());
        program
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("cmd")
    }

    fn long_running_command() -> String {
        if uses_cmd() {
            "ping -n 20 127.0.0.1 >nul".to_string()
        } else {
            "sleep 20".to_string()
        }
    }

    #[tokio::test]
    async fn captures_stdout() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let out = Shell
            .call(json!({ "command": "echo hello-from-shell" }), &ctx)
            .await
            .unwrap();

        assert!(out.content.contains("hello-from-shell"), "{}", out.content);
        assert_eq!(out.metadata["exit_code"], 0);
        assert_eq!(out.metadata["timed_out"], false);
        assert_eq!(out.metadata["aborted"], false);
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn non_zero_exit_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let out = Shell
            .call(json!({ "command": "exit 3" }), &ctx)
            .await
            .unwrap();

        assert!(out.is_error);
        assert_eq!(out.metadata["exit_code"], 3);
        assert_eq!(out.metadata["timed_out"], false);
        assert!(
            out.content.contains("exited with code 3"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn truncated_output_says_so_and_how_to_narrow() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = context(tmp.path());
        ctx.config.max_output_bytes = 16;
        let payload = "y".repeat(200);

        let out = Shell
            .call(json!({ "command": format!("echo {payload}") }), &ctx)
            .await
            .unwrap();

        assert!(!out.is_error);
        assert_eq!(out.metadata["exit_code"], 0);
        assert_eq!(out.metadata["truncated"], true);
        assert!(
            out.content.contains("[output truncated:"),
            "{}",
            out.content
        );
        assert!(out.content.contains("bytes dropped"), "{}", out.content);
        assert!(out.content.contains("tail -n"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_long_command_is_killed_at_the_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());
        let start = Instant::now();

        let out = Shell
            .call(
                json!({ "command": long_running_command(), "timeout_secs": 1 }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert_eq!(out.metadata["timed_out"], true);
        assert_eq!(out.metadata["exit_code"], Value::Null);
        assert!(
            out.content.contains("timed out after 1s"),
            "{}",
            out.content
        );
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn an_aborted_context_skips_the_command() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());
        ctx.abort.abort();

        let out = Shell
            .call(json!({ "command": "echo never-ran" }), &ctx)
            .await
            .unwrap();

        assert!(out.is_error);
        assert_eq!(out.metadata["aborted"], true);
        assert!(!out.content.contains("never-ran"), "{}", out.content);
    }

    #[tokio::test]
    async fn runs_in_the_requested_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub/marker.txt"), "marker-content").unwrap();
        let ctx = context(tmp.path());

        let command = if uses_cmd() {
            "type marker.txt"
        } else {
            "cat marker.txt"
        };
        let out = Shell
            .call(json!({ "command": command, "cwd": "sub" }), &ctx)
            .await
            .unwrap();
        assert!(out.content.contains("marker-content"), "{}", out.content);

        let err = Shell
            .call(json!({ "command": "ls", "cwd": "../.." }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::PathEscape(_)), "{err}");
    }
}
