//! `write_file`: whole-file writes, creating parent directories as needed.

use std::path::Path;

use async_trait::async_trait;
use harness_core::{object_schema, Result, ToolSpec};
use serde_json::{json, Value};

use crate::args::Args;
use crate::builtin::{io_error, relative_display};
use crate::{Tool, ToolContext, ToolOutput};

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "write_file",
            "Write a file, replacing it if it exists and creating missing parent directories. Prefer this for new files or a full rewrite; use `edit_file` to change part of an existing file so the rest is preserved. Fails when the path escapes the workspace or a parent is an existing file. Read the file first if its current content must survive.",
            object_schema(
                json!({
                    "path": { "type": "string", "description": "Path relative to the workspace root." },
                    "content": { "type": "string", "description": "Full file contents." }
                }),
                &["path", "content"],
            ),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let args = Args::new("write_file", args)?;
        let path = args.required_str("path")?;
        let content = args.required_str("content")?;

        let resolved = ctx.resolve(Path::new(&path))?;
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                io_error(
                    "write_file",
                    "create the parent directory of",
                    &path,
                    "check that no parent is an existing file and that the workspace is writable",
                    err,
                )
            })?;
        }
        std::fs::write(&resolved, &content).map_err(|err| {
            io_error(
                "write_file",
                "write",
                &path,
                "check that the path is a file and is writable",
                err,
            )
        })?;
        ctx.emit_progress(format!("wrote {path}"));

        Ok(
            ToolOutput::text(format!("wrote {path}")).with_metadata(json!({
                "path": relative_display(&ctx.workspace_root, &resolved),
                "bytes_written": content.len(),
            })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::builtin::test_support::context;
    use harness_core::HarnessError;

    #[tokio::test]
    async fn creates_parent_directories_and_round_trips_content() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let out = WriteFile
            .call(
                json!({ "path": "deep/nested/dir/file.txt", "content": "hello\nworld" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!out.is_error);
        assert_eq!(out.metadata["path"], "deep/nested/dir/file.txt");
        assert_eq!(out.metadata["bytes_written"], 11);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("deep/nested/dir/file.txt")).unwrap(),
            "hello\nworld"
        );
    }

    #[tokio::test]
    async fn paths_outside_the_workspace_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = context(&root);

        let err = WriteFile
            .call(json!({ "path": "../outside.txt", "content": "nope" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::PathEscape(_)), "{err}");
        assert!(!tmp.path().join("outside.txt").exists());
    }

    #[tokio::test]
    async fn content_is_required() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let err = WriteFile
            .call(json!({ "path": "a.txt" }), &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("missing required argument `content`"),
            "{err}"
        );
    }
}
