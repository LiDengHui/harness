//! The builtin tool set: seven tools that need nothing but the workspace root.

pub mod edit_file;
pub mod grep;
pub mod list_dir;
pub mod read_file;
pub mod shell;
pub mod web_fetch;
pub mod write_file;

use std::path::Path;
use std::sync::Arc;

use harness_core::HarnessError;

use crate::Tool;

/// Directories no listing or search descends into: version control state, build
/// output, and dependency trees. Walking them costs context and never helps.
pub(crate) const IGNORED_DIRS: &[&str] =
    &[".git", "target", "node_modules", ".venv", "__pycache__"];

pub(crate) fn is_ignored(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| IGNORED_DIRS.contains(&name))
}

/// Path as reported to the model: relative to `root` and always slash-separated,
/// so output looks the same on Windows as everywhere else.
pub(crate) fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Truncates on a character boundary and reports whether anything was dropped.
pub(crate) fn truncate_bytes(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// The one truncation marker every builtin appends.
///
/// The marker is part of the tool's `content`, not its `metadata`, on purpose:
/// the agent loop forwards only `content` back to the model, so a fact that
/// lives only in `metadata` is a fact the model never sees. It names what was
/// shown, what was dropped, and the concrete way to get the rest.
///
/// `unit` is the thing being counted (`bytes`, `matches`, `entries`), so a
/// capped search reports matches rather than pretending to measure bytes.
pub(crate) fn truncation_marker(shown: usize, total: usize, unit: &str, remedy: &str) -> String {
    let dropped = total.saturating_sub(shown);
    format!(
        "\n\n[output truncated: showed {shown} of {total} {unit}; {dropped} {unit} dropped. {remedy}]"
    )
}

/// A payload cut down to a byte budget, carrying the numbers a marker needs.
pub(crate) struct Truncated {
    /// The payload after cutting, before any marker is appended.
    pub body: String,
    /// Bytes of `body`.
    pub shown_bytes: usize,
    /// Bytes the payload had before cutting.
    pub total_bytes: usize,
    pub truncated: bool,
}

impl Truncated {
    /// `body`, plus a [`truncation_marker`] when bytes were dropped.
    pub(crate) fn content(&self, remedy: &str) -> String {
        let mut out = self.body.clone();
        if self.truncated {
            out.push_str(&truncation_marker(
                self.shown_bytes,
                self.total_bytes,
                "bytes",
                remedy,
            ));
        }
        out
    }
}

/// Cuts `text` to `max_bytes` on a character boundary.
pub(crate) fn truncate_payload(text: &str, max_bytes: usize) -> Truncated {
    let (body, truncated) = truncate_bytes(text, max_bytes);
    Truncated {
        shown_bytes: body.len(),
        total_bytes: text.len(),
        truncated,
        body,
    }
}

/// An I/O failure that names the tool, the input, and the way out.
///
/// The bare OS error ("The system cannot find the file specified") names
/// neither the file nor a recovery, so a model reading it cannot tell what to
/// change. The variant stays [`HarnessError::Io`] so callers that match on it
/// still can; only the message gains the context.
pub(crate) fn io_error(
    tool: &str,
    action: &str,
    input: &str,
    advice: &str,
    err: std::io::Error,
) -> HarnessError {
    HarnessError::Io(std::io::Error::new(
        err.kind(),
        format!("{tool}: cannot {action} `{input}`: {err}. {advice}"),
    ))
}

pub(crate) fn all() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read_file::ReadFile),
        Arc::new(write_file::WriteFile),
        Arc::new(edit_file::EditFile),
        Arc::new(list_dir::ListDir),
        Arc::new(grep::Grep),
        Arc::new(shell::Shell),
        Arc::new(web_fetch::WebFetch),
    ]
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    use harness_core::ToolsConfig;

    use crate::ToolContext;

    pub(crate) fn context(root: &Path) -> ToolContext {
        ToolContext::new(root.to_path_buf(), ToolsConfig::default())
    }
}
