//! `grep`: regex search across workspace files.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use harness_core::{HarnessError, Result};
use regex::Regex;
use serde_json::json;
use walkdir::WalkDir;

use crate::builtin::{is_ignored, relative_display, truncate_payload, truncation_marker};
use crate::{ToolContext, ToolOutput};

// The generated `Tool` impl names the trait by absolute path, so only the tests
// — which call `.call()` on the tool — need it in scope.
#[cfg(test)]
use crate::Tool;

const DEFAULT_MAX_RESULTS: usize = 100;

#[harness_macros::tool(
    name = "grep",
    description = "Search file contents with a Rust regular expression; output is one `path:line: text` per match. Prefer this over `read_file` to find a symbol across many files and over `shell grep` for workspace-relative results. Results are capped and byte-limited, and the output says so and how to narrow. Fails on an invalid regex or glob, or a path outside the workspace.",
    type_name = "Grep"
)]
#[derive(serde::Deserialize)]
pub struct GrepArgs {
    /// Rust regex syntax.
    pattern: String,
    /// File or directory to search. Defaults to the workspace root.
    path: Option<String>,
    /// Only search files matching this glob, e.g. `*.rs`.
    glob: Option<String>,
    /// Maximum matches to return. Defaults to 100.
    max_results: Option<usize>,
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

fn compile_glob(pattern: &str) -> Result<GlobSet> {
    let advice = "Use a shell-style glob such as `*.rs` or `src/**/*.rs`.";
    let glob = Glob::new(pattern).map_err(|err| {
        HarnessError::Tool(format!("grep: invalid glob `{pattern}`: {err}. {advice}"))
    })?;
    let mut builder = GlobSetBuilder::new();
    builder.add(glob);
    builder.build().map_err(|err| {
        HarnessError::Tool(format!("grep: invalid glob `{pattern}`: {err}. {advice}"))
    })
}

async fn grep(args: GrepArgs, ctx: &ToolContext) -> Result<ToolOutput> {
    let pattern = args.pattern;
    let path = args.path.unwrap_or_else(|| ".".to_string());
    let glob = args.glob;
    let max_results = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);

    let regex = Regex::new(&pattern).map_err(|err| {
        HarnessError::Tool(format!(
            "grep: invalid regex `{pattern}`: {err}. Check the syntax or escape regex metacharacters."
        ))
    })?;
    let matcher = match &glob {
        Some(pattern) => Some(compile_glob(pattern)?),
        None => None,
    };

    let base = ctx.resolve(Path::new(&path))?;

    // A file target is searched as-is; a directory is walked. Sorted
    // traversal keeps the capped result set deterministic.
    let mut targets: Vec<(String, PathBuf)> = Vec::new();
    if base.is_file() {
        targets.push((file_name(&base), base.clone()));
    } else {
        let walker = WalkDir::new(&base)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_ignored(entry.path()));
        for entry in walker {
            let entry = entry.map_err(|err| {
                HarnessError::Tool(format!(
                    "grep: cannot walk `{path}`: {err}. Check that the path exists and is readable."
                ))
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = relative_display(&base, entry.path());
            if let Some(matcher) = &matcher {
                if !matcher.is_match(Path::new(&relative)) {
                    continue;
                }
            }
            targets.push((relative, entry.path().to_path_buf()));
        }
    }

    // Every match is counted so a capped result can say how many were dropped;
    // only the first `max_results` are formatted into the output.
    let mut results: Vec<String> = Vec::new();
    let mut total_matches = 0usize;
    let mut files_scanned = 0usize;

    for (relative, file) in &targets {
        // Binary or non-UTF-8 files are skipped rather than failing the search.
        let Ok(body) = std::fs::read_to_string(file) else {
            continue;
        };
        files_scanned += 1;
        for (index, line) in body.lines().enumerate() {
            if regex.is_match(line) {
                total_matches += 1;
                if results.len() < max_results {
                    results.push(format!("{relative}:{}: {line}", index + 1));
                }
            }
        }
    }

    let shown_matches = results.len();
    let capped = total_matches > shown_matches;
    let cut = truncate_payload(&results.join("\n"), ctx.config.max_output_bytes);

    let mut content = cut.body.clone();
    if cut.truncated {
        content.push_str(&truncation_marker(
            cut.shown_bytes,
            cut.total_bytes,
            "bytes",
            "narrow the search with a more specific `pattern`, or add `glob`/`path`.",
        ));
    }
    if capped {
        content.push_str(&truncation_marker(
            shown_matches,
            total_matches,
            "matches",
            "narrow the search with a more specific `pattern`, or add `glob`/`path`.",
        ));
    }

    Ok(ToolOutput::text(content).with_metadata(json!({
        "matches": shown_matches,
        "total_matches": total_matches,
        "files_scanned": files_scanned,
        "truncated": capped || cut.truncated,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::builtin::test_support::context;

    fn seed(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "// nothing\nfn hello() {}\n").unwrap();
        std::fs::write(root.join("src/notes.txt"), "hello world\n").unwrap();
        std::fs::write(root.join("target/build.rs"), "fn hello() {}\n").unwrap();
    }

    #[tokio::test]
    async fn reports_matches_as_path_line_text() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let ctx = context(tmp.path());

        let out = Grep
            .call(json!({ "pattern": "hello" }), &ctx)
            .await
            .unwrap();

        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(
            lines,
            vec![
                "src/lib.rs:2: fn hello() {}",
                "src/notes.txt:1: hello world"
            ]
        );
        assert_eq!(out.metadata["matches"], 2);
        assert_eq!(out.metadata["truncated"], false);
        assert!(!out.content.contains("target/"));
    }

    #[tokio::test]
    async fn filters_by_glob() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let ctx = context(tmp.path());

        let out = Grep
            .call(json!({ "pattern": "hello", "glob": "*.rs" }), &ctx)
            .await
            .unwrap();
        assert_eq!(out.content, "src/lib.rs:2: fn hello() {}");
    }

    #[tokio::test]
    async fn an_invalid_regex_is_a_tool_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let err = Grep
            .call(json!({ "pattern": "a(" }), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::Tool(_)), "{err}");
        assert!(err.to_string().contains("invalid regex"), "{err}");
    }

    #[tokio::test]
    async fn a_file_target_is_searched_directly() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let ctx = context(tmp.path());

        let out = Grep
            .call(json!({ "pattern": "hello", "path": "src/notes.txt" }), &ctx)
            .await
            .unwrap();
        assert_eq!(out.content, "notes.txt:1: hello world");
        assert_eq!(out.metadata["files_scanned"], 1);
    }

    #[tokio::test]
    async fn a_capped_search_reports_the_total_and_how_to_narrow() {
        let tmp = tempfile::tempdir().unwrap();
        let body: String = (0..7).map(|n| format!("hit {n}\n")).collect();
        std::fs::write(tmp.path().join("hits.txt"), body).unwrap();
        let ctx = context(tmp.path());

        let out = Grep
            .call(json!({ "pattern": "hit", "max_results": 3 }), &ctx)
            .await
            .unwrap();

        assert_eq!(out.metadata["matches"], 3);
        assert_eq!(out.metadata["total_matches"], 7);
        assert_eq!(out.metadata["truncated"], true);
        assert_eq!(
            out.content
                .lines()
                .filter(|line| line.starts_with("hits.txt:"))
                .count(),
            3
        );
        assert!(
            out.content
                .contains("[output truncated: showed 3 of 7 matches"),
            "{}",
            out.content
        );
        assert!(out.content.contains("narrow the search"), "{}", out.content);
    }
}
