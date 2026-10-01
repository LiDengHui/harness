//! `edit_file`: exact-string replacement inside an existing file.

use std::path::Path;

use harness_core::{HarnessError, Result};
use serde_json::json;

use crate::builtin::{io_error, relative_display};
use crate::{ToolContext, ToolOutput};

// The generated `Tool` impl names the trait by absolute path, so only the tests
// which call `.call()` on the tool need it in scope.
#[cfg(test)]
use crate::Tool;

#[harness_macros::tool(
    name = "edit_file",
    description = "Replace an exact string in an existing file. `old_string` must be unique unless `replace_all` is true, so include surrounding context. Prefer this over `write_file` for targeted changes. Fails when `old_string` is empty, is not found, occurs more than once without `replace_all`, or equals `new_string`; read the file first to copy the exact text.",
    type_name = "EditFile"
)]
#[derive(serde::Deserialize)]
pub struct EditFileArgs {
    /// Path relative to the workspace root.
    path: String,
    /// Exact text to replace, including indentation.
    old_string: String,
    /// Replacement text.
    new_string: String,
    /// Replace every occurrence. Defaults to false.
    replace_all: Option<bool>,
}

async fn edit_file(args: EditFileArgs, ctx: &ToolContext) -> Result<ToolOutput> {
    let path = args.path;
    let old_string = args.old_string;
    let new_string = args.new_string;
    let replace_all = args.replace_all.unwrap_or(false);

    let resolved = ctx.resolve(Path::new(&path))?;
    let content = std::fs::read_to_string(&resolved).map_err(|err| {
        io_error(
            "edit_file",
            "read",
            &path,
            "check the path with list_dir, or create the file with write_file",
            err,
        )
    })?;

    if old_string.is_empty() {
        return Err(HarnessError::Tool(format!(
            "edit_file: `old_string` must not be empty ({path})"
        )));
    }
    if old_string == new_string {
        return Err(HarnessError::Tool(format!(
            "edit_file: `old_string` and `new_string` are identical, so {path} would not change"
        )));
    }

    let matches = content.matches(&old_string).count();
    if matches == 0 {
        return Err(HarnessError::Tool(format!(
            "edit_file: `old_string` was not found in {path}"
        )));
    }
    let replacements = if replace_all { matches } else { 1 };
    if matches > 1 && !replace_all {
        return Err(HarnessError::Tool(format!(
            "edit_file: `old_string` occurs {matches} times in {path}; \
             add more surrounding context to make it unique, or set `replace_all` to true"
        )));
    }

    let updated = if replace_all {
        content.replace(&old_string, &new_string)
    } else {
        content.replacen(&old_string, &new_string, 1)
    };
    std::fs::write(&resolved, updated).map_err(|err| {
        io_error(
            "edit_file",
            "write",
            &path,
            "check that the file is writable and not open in another program",
            err,
        )
    })?;
    ctx.emit_progress(format!("edited {path}"));

    Ok(
        ToolOutput::text(format!("{path}: {replacements} replacement(s)")).with_metadata(json!({
            "path": relative_display(&ctx.workspace_root, &resolved),
            "replacements": replacements,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::builtin::test_support::context;

    fn seed(root: &Path, body: &str) {
        std::fs::write(root.join("file.txt"), body).unwrap();
    }

    #[tokio::test]
    async fn applies_a_unique_replacement() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "one\ntwo\nthree\n");
        let ctx = context(tmp.path());

        let out = EditFile
            .call(
                json!({ "path": "file.txt", "old_string": "two", "new_string": "TWO" }),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out.metadata["replacements"], 1);
        assert_eq!(out.metadata["path"], "file.txt");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("file.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
    }

    #[tokio::test]
    async fn repeated_needles_are_ambiguous_unless_replace_all() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "dup\ndup\n");
        let ctx = context(tmp.path());

        let err = EditFile
            .call(
                json!({ "path": "file.txt", "old_string": "dup", "new_string": "x" }),
                &ctx,
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("occurs 2 times"), "{message}");
        assert!(message.contains("replace_all"), "{message}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("file.txt")).unwrap(),
            "dup\ndup\n"
        );

        let out = EditFile
            .call(
                json!({
                    "path": "file.txt",
                    "old_string": "dup",
                    "new_string": "x",
                    "replace_all": true
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.metadata["replacements"], 2);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("file.txt")).unwrap(),
            "x\nx\n"
        );
    }

    #[tokio::test]
    async fn missing_needles_and_no_op_edits_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "hello\n");
        let ctx = context(tmp.path());

        let err = EditFile
            .call(
                json!({ "path": "file.txt", "old_string": "absent", "new_string": "x" }),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("was not found in file.txt"),
            "{err}"
        );

        let err = EditFile
            .call(
                json!({ "path": "file.txt", "old_string": "hello", "new_string": "hello" }),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("identical"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_file_error_names_the_path_and_the_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let err = EditFile
            .call(
                json!({ "path": "gone.txt", "old_string": "a", "new_string": "b" }),
                &ctx,
            )
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("gone.txt"), "{message}");
        assert!(message.contains("write_file"), "{message}");
    }
}
