//! `list_dir`: a sorted directory listing that skips noise directories.

use std::path::Path;

use harness_core::{HarnessError, Result};
use serde_json::json;
use walkdir::WalkDir;

use crate::builtin::{is_ignored, relative_display, truncation_marker};
use crate::{ToolContext, ToolOutput};

// The generated `Tool` impl names the trait by absolute path, so only the tests
// — which call `.call()` on the tool — need it in scope.
#[cfg(test)]
use crate::Tool;

#[harness_macros::tool(
    name = "list_dir",
    description = "List directory entries, directories first with a trailing `/`; non-recursive by default, so set `recursive` and `max_depth` to descend. Prefer this over `shell ls` to explore the workspace and over `grep` when you do not yet know the file name. `.git`, `target`, `node_modules`, `.venv` and `__pycache__` are always skipped. Fails when the path is missing or outside the workspace.",
    type_name = "ListDir"
)]
#[derive(serde::Deserialize)]
pub struct ListDirArgs {
    /// Directory relative to the workspace root. Defaults to the root.
    path: Option<String>,
    /// Descend into subdirectories. Defaults to false.
    recursive: Option<bool>,
    /// How deep to descend when recursive. Defaults to the whole tree.
    max_depth: Option<usize>,
}

async fn list_dir(args: ListDirArgs, ctx: &ToolContext) -> Result<ToolOutput> {
    let path = args.path.unwrap_or_else(|| ".".to_string());
    let recursive = args.recursive.unwrap_or(false);
    let max_depth = args.max_depth;

    let base = ctx.resolve(Path::new(&path))?;
    // A non-recursive listing is exactly a depth-1 walk.
    let depth = if recursive {
        max_depth.unwrap_or(usize::MAX)
    } else {
        1
    };

    let walker = WalkDir::new(&base)
        .min_depth(1)
        .max_depth(depth)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !is_ignored(entry.path()));

    let mut entries: Vec<(bool, String)> = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|err| HarnessError::Tool(format!("list_dir: {err}")))?;
        let is_dir = entry.file_type().is_dir();
        entries.push((is_dir, relative_display(&base, entry.path())));
    }
    entries.sort_by(|a, b| (!a.0, &a.1).cmp(&(!b.0, &b.1)));

    let total_entries = entries.len();
    let mut lines: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    for (is_dir, relative) in &entries {
        let line = if *is_dir {
            format!("{relative}/")
        } else {
            relative.clone()
        };
        bytes += line.len() + 1;
        if bytes > ctx.config.max_output_bytes && !lines.is_empty() {
            break;
        }
        lines.push(line);
    }

    let shown = lines.len();
    let capped = shown < total_entries;
    let mut content = lines.join("\n");
    if capped {
        content.push_str(&truncation_marker(
            shown,
            total_entries,
            "entries",
            "narrow with `path`, or set `recursive=false` and lower `max_depth`.",
        ));
    }

    Ok(ToolOutput::text(content).with_metadata(json!({
        "entries": shown,
        "total_entries": total_entries,
        "truncated": capped,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::builtin::test_support::context;

    fn seed(root: &Path) {
        for dir in ["beta", "alpha", "target", ".git", "node_modules"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in ["zebra.txt", "apple.txt", "beta/inner.txt"] {
            std::fs::write(root.join(file), "x").unwrap();
        }
    }

    #[tokio::test]
    async fn sorts_directories_first_and_skips_ignored_ones() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let ctx = context(tmp.path());

        let out = ListDir.call(json!({}), &ctx).await.unwrap();
        let lines: Vec<&str> = out.content.lines().collect();

        assert_eq!(lines, vec!["alpha/", "beta/", "apple.txt", "zebra.txt"]);
        assert_eq!(out.metadata["entries"], 4);
        assert_eq!(out.metadata["truncated"], false);

        let recursive = ListDir
            .call(json!({ "recursive": true }), &ctx)
            .await
            .unwrap();
        assert!(
            recursive.content.contains("beta/inner.txt"),
            "{}",
            recursive.content
        );
        assert!(
            !recursive
                .content
                .lines()
                .any(|line| line.starts_with("target")),
            "{}",
            recursive.content
        );
    }

    #[tokio::test]
    async fn honours_max_depth_and_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        std::fs::write(tmp.path().join("a/b/c/deep.txt"), "x").unwrap();
        let ctx = context(tmp.path());

        let shallow = ListDir
            .call(
                json!({ "path": "a", "recursive": true, "max_depth": 1 }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(shallow.content.lines().collect::<Vec<_>>(), vec!["b/"]);

        let deep = ListDir
            .call(json!({ "path": "a", "recursive": true }), &ctx)
            .await
            .unwrap();
        assert_eq!(deep.content, "b/\nb/c/\nb/c/deep.txt");
    }

    #[tokio::test]
    async fn paths_outside_the_workspace_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let ctx = context(&root);

        let err = ListDir
            .call(json!({ "path": "../outside.txt" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::PathEscape(_)), "{err}");
    }

    #[tokio::test]
    async fn a_truncated_listing_reports_the_total_and_how_to_narrow() {
        let tmp = tempfile::tempdir().unwrap();
        for n in 0..6 {
            std::fs::write(tmp.path().join(format!("file-{n}.txt")), "x").unwrap();
        }
        let mut ctx = context(tmp.path());
        ctx.config.max_output_bytes = 20;

        let out = ListDir.call(json!({}), &ctx).await.unwrap();
        assert_eq!(out.metadata["total_entries"], 6);
        assert_eq!(out.metadata["truncated"], true);
        assert!(out.metadata["entries"].as_u64().unwrap() < 6);
        assert!(
            out.content.contains("[output truncated:"),
            "{}",
            out.content
        );
        assert!(out.content.contains("of 6 entries"), "{}", out.content);
        assert!(
            out.content.contains("narrow with `path`"),
            "{}",
            out.content
        );
    }
}
