//! `read_file`: windowed reading of a workspace file.
//!
//! A file is never refused for being large. The reader streams it, shows a
//! bounded window, and — when there is more — the returned text says how large
//! the file really is and which `offset` continues the read.

use std::io::{BufRead, BufReader};
use std::path::Path;

use async_trait::async_trait;
use harness_core::{object_schema, HarnessError, Result, ToolSpec};
use serde_json::{json, Value};

use crate::args::Args;
use crate::builtin::{io_error, relative_display, truncate_bytes, truncation_marker};
use crate::{Tool, ToolContext, ToolOutput};

/// What to do about a read failure. The path alone does not tell the model
/// whether to search for the file or create it.
const READ_ADVICE: &str = "check the path with list_dir, or create the file with write_file";

pub struct ReadFile;

/// The window a single read produced, before the marker is appended.
struct Window {
    body: String,
    lines: usize,
    shown_bytes: usize,
    truncated: bool,
    /// Line to pass as `offset` to see what comes after this window.
    next_offset: Option<usize>,
    /// The window stopped inside a single line that was longer than the budget.
    mid_line: bool,
    /// Line count, known only when the read reached the end of the file.
    total_lines: Option<usize>,
}

/// Streams the file, skipping `start - 1` lines and then collecting lines until
/// `limit` or `budget` bytes are reached. Never holds more than the window.
fn read_window(
    resolved: &Path,
    display: &str,
    start: usize,
    limit: Option<usize>,
    budget: usize,
) -> Result<Window> {
    let file = std::fs::File::open(resolved)
        .map_err(|err| io_error("read_file", "read", display, READ_ADVICE, err))?;
    let mut reader = BufReader::new(file);

    let mut selected: Vec<String> = Vec::new();
    let mut shown = 0usize;
    let mut line_no = 0usize;
    let mut truncated = false;
    let mut mid_line = false;
    let mut next_offset = None;
    let mut buffer = Vec::new();

    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|err| io_error("read_file", "read", display, READ_ADVICE, err))?;
        if read == 0 {
            break;
        }
        line_no += 1;
        if line_no < start {
            continue;
        }
        if let Some(limit) = limit {
            if selected.len() >= limit {
                // An explicit `limit` is a request the tool honoured, not a
                // silent cut, so it gets a continuation offset but no marker.
                next_offset = Some(line_no);
                break;
            }
        }

        let raw = String::from_utf8_lossy(&buffer);
        let line = raw.strip_suffix('\n').unwrap_or(&raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let added = if selected.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };

        if shown + added > budget {
            if selected.is_empty() {
                // A single line longer than the whole window: show its start so
                // the model still sees the file rather than an empty result.
                let (prefix, _) = truncate_bytes(line, budget);
                shown = prefix.len();
                selected.push(prefix);
                mid_line = true;
            }
            truncated = true;
            next_offset = Some(line_no);
            break;
        }

        shown += added;
        selected.push(line.to_string());
    }

    // `next_offset` is set exactly when the read stopped before the end, so it
    // is also how the end is detected.
    let reached_end = next_offset.is_none();
    Ok(Window {
        body: selected.join("\n"),
        lines: selected.len(),
        shown_bytes: shown,
        truncated,
        next_offset,
        mid_line,
        total_lines: reached_end.then_some(line_no),
    })
}

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "read_file",
            "Read a text file from the workspace. `offset` (1-based line) and `limit` page through large files; a truncated response reports the file's real size and the `offset` to continue from. Prefer this over `shell cat` for reading files. Fails when the path is missing, is a directory, or escapes the workspace; use `list_dir` to find the path and `grep` to jump to a section.",
            object_schema(
                json!({
                    "path": { "type": "string", "description": "Path relative to the workspace root." },
                    "offset": { "type": "integer", "description": "1-based line to start at. Defaults to 1." },
                    "limit": { "type": "integer", "description": "Maximum number of lines to return. Defaults to the end of the file." }
                }),
                &["path"],
            ),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let args = Args::new("read_file", args)?;
        let path = args.required_str("path")?;
        let offset = args.optional_usize("offset")?;
        let limit = args.optional_usize("limit")?;

        let resolved = ctx.resolve(Path::new(&path))?;
        let metadata = std::fs::metadata(&resolved)
            .map_err(|err| io_error("read_file", "read", &path, READ_ADVICE, err))?;
        if metadata.is_dir() {
            return Err(HarnessError::Tool(format!(
                "read_file: `{path}` is a directory, not a file; use list_dir to see its entries"
            )));
        }

        let total_bytes = metadata.len();
        // The window is what can be both read and returned: the read limit
        // bounds memory, the output limit bounds the conversation.
        let budget = ctx
            .config
            .max_read_bytes
            .min(ctx.config.max_output_bytes)
            .max(1);
        let start = offset.unwrap_or(1).max(1);
        let display = relative_display(&ctx.workspace_root, &resolved);

        // The common case: a whole file that fits, returned byte-for-byte.
        if offset.is_none() && limit.is_none() && total_bytes <= budget as u64 {
            let bytes = std::fs::read(&resolved)
                .map_err(|err| io_error("read_file", "read", &path, READ_ADVICE, err))?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return Ok(ToolOutput::text(text.clone()).with_metadata(json!({
                "path": display,
                "bytes": text.len(),
                "lines": text.lines().count(),
                "total_bytes": total_bytes,
                "truncated": false,
                "next_offset": Value::Null,
            })));
        }

        let window = read_window(&resolved, &path, start, limit, budget)?;

        let mut content = window.body.clone();
        if window.truncated {
            let next = window.next_offset.unwrap_or(start);
            let remedy = if window.mid_line {
                format!(
                    "the line at offset {next} is longer than the {budget}-byte read window; \
                     use grep to locate a section, then read it with a smaller `limit`."
                )
            } else {
                format!("continue with read_file `offset={next}`.")
            };
            content.push_str(&truncation_marker(
                window.shown_bytes,
                total_bytes as usize,
                "bytes",
                &remedy,
            ));
        } else if window.lines == 0 && total_bytes > 0 {
            if let Some(total_lines) = window.total_lines {
                content = format!(
                    "[read_file: `{path}` has {total_lines} lines; offset {start} is past the end, \
                     so nothing was returned. Use an offset between 1 and {total_lines}.]"
                );
            }
        }

        Ok(ToolOutput::text(content).with_metadata(json!({
            "path": display,
            "bytes": window.shown_bytes,
            "lines": window.lines,
            "total_bytes": total_bytes,
            "truncated": window.truncated,
            "next_offset": window.next_offset,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::test_support::context;

    #[tokio::test]
    async fn reads_content_and_applies_the_line_window() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let ctx = context(tmp.path());

        let full = ReadFile
            .call(json!({ "path": "notes.txt" }), &ctx)
            .await
            .unwrap();
        assert_eq!(full.content, "alpha\nbeta\ngamma\n");
        assert_eq!(full.metadata["lines"], 3);
        assert_eq!(full.metadata["bytes"], 17);
        assert_eq!(full.metadata["path"], "notes.txt");
        assert_eq!(full.metadata["truncated"], false);
        assert!(!full.is_error);

        let windowed = ReadFile
            .call(
                json!({ "path": "notes.txt", "offset": 2, "limit": 1 }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(windowed.content, "beta");
        assert_eq!(windowed.metadata["lines"], 1);
    }

    #[tokio::test]
    async fn missing_arguments_and_missing_files_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let err = ReadFile.call(json!({}), &ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("missing required argument `path`"),
            "{err}"
        );

        let err = ReadFile
            .call(json!({ "path": "nope.txt" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::Io(_)), "{err}");
        let message = err.to_string();
        assert!(message.contains("nope.txt"), "{message}");
        assert!(message.contains("list_dir"), "{message}");
    }

    #[tokio::test]
    async fn a_large_file_is_paged_with_a_marker_and_a_continuation() {
        let tmp = tempfile::tempdir().unwrap();
        let body: String = (0..20).map(|n| format!("line-{n:04}\n")).collect();
        std::fs::write(tmp.path().join("big.txt"), &body).unwrap();
        let mut ctx = context(tmp.path());
        ctx.config.max_read_bytes = 64;

        let first = ReadFile
            .call(json!({ "path": "big.txt" }), &ctx)
            .await
            .unwrap();
        assert!(!first.is_error);
        assert_eq!(first.metadata["truncated"], true);
        assert_eq!(first.metadata["total_bytes"], body.len());
        assert!(
            first.content.contains("[output truncated:"),
            "{}",
            first.content
        );
        assert!(
            first.content.contains(&format!("of {} bytes", body.len())),
            "{}",
            first.content
        );
        assert!(
            first.content.contains("continue with read_file `offset="),
            "{}",
            first.content
        );

        // Follow the marker's continuation until the whole file has been seen.
        let mut seen = String::new();
        let mut offset = 1usize;
        loop {
            let out = ReadFile
                .call(json!({ "path": "big.txt", "offset": offset }), &ctx)
                .await
                .unwrap();
            let shown = out
                .content
                .split("\n\n[output truncated")
                .next()
                .unwrap()
                .to_string();
            seen.push_str(&shown);
            if out.metadata["truncated"] == false {
                break;
            }
            let next = out.metadata["next_offset"].as_u64().unwrap() as usize;
            assert!(
                next > offset,
                "continuation must advance: {offset} -> {next}"
            );
            seen.push('\n');
            offset = next;
        }
        assert_eq!(format!("{seen}\n"), body);
    }

    #[tokio::test]
    async fn a_single_line_longer_than_the_window_is_still_shown() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("one-line.txt"), "x".repeat(100)).unwrap();
        let mut ctx = context(tmp.path());
        ctx.config.max_read_bytes = 16;

        let out = ReadFile
            .call(json!({ "path": "one-line.txt" }), &ctx)
            .await
            .unwrap();
        assert_eq!(out.metadata["truncated"], true);
        assert!(out.content.starts_with(&"x".repeat(16)), "{}", out.content);
        assert!(
            out.content.contains("longer than the 16-byte read window"),
            "{}",
            out.content
        );
        assert!(out.content.contains("grep"), "{}", out.content);
    }

    #[tokio::test]
    async fn an_offset_past_the_end_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "alpha\nbeta\n").unwrap();
        let ctx = context(tmp.path());

        let out = ReadFile
            .call(json!({ "path": "notes.txt", "offset": 9 }), &ctx)
            .await
            .unwrap();
        assert!(out.content.contains("has 2 lines"), "{}", out.content);
        assert!(
            out.content.contains("offset 9 is past the end"),
            "{}",
            out.content
        );
        assert_eq!(out.metadata["lines"], 0);
    }

    #[tokio::test]
    async fn paths_outside_the_workspace_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("outside.txt"), "secret").unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = context(&root);

        let err = ReadFile
            .call(json!({ "path": "../outside.txt" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::PathEscape(_)), "{err}");
    }
}
