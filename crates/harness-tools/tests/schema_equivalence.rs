//! The `#[harness_macros::tool]`-derived schemas must match the hand-written
//! ones they replaced, field for field.
//!
//! `grep`, `list_dir`, `edit_file` and `shell` derive their `ToolSpec` from an
//! argument struct. The literals below mirror the schema and description those
//! tools ship; keeping them in the test is what stops the derived spec and the
//! documented one from drifting apart.

use harness_core::{object_schema, ToolSpec};
use harness_tools::builtin::{edit_file::EditFile, grep::Grep, list_dir::ListDir, shell::Shell};
use harness_tools::Tool;
use serde_json::json;

fn assert_same_spec(derived: ToolSpec, hand_written: ToolSpec) {
    assert_eq!(derived.name, hand_written.name, "tool name drifted");
    assert_eq!(
        derived.description, hand_written.description,
        "{} description drifted",
        hand_written.name
    );
    assert_eq!(
        derived.parameters, hand_written.parameters,
        "{} schema drifted",
        hand_written.name
    );
}

#[test]
fn grep_schema_matches_the_hand_written_one() {
    assert_same_spec(
        Grep.spec(),
        ToolSpec::new(
            "grep",
            "Search file contents with a Rust regular expression; output is one `path:line: text` per match. Prefer this over `read_file` to find a symbol across many files and over `shell grep` for workspace-relative results. Results are capped and byte-limited, and the output says so and how to narrow. Fails on an invalid regex or glob, or a path outside the workspace.",
            object_schema(
                json!({
                    "pattern": { "type": "string", "description": "Rust regex syntax." },
                    "path": { "type": "string", "description": "File or directory to search. Defaults to the workspace root." },
                    "glob": { "type": "string", "description": "Only search files matching this glob, e.g. `*.rs`." },
                    "max_results": { "type": "integer", "description": "Maximum matches to return. Defaults to 100." }
                }),
                &["pattern"],
            ),
        ),
    );
}

#[test]
fn list_dir_schema_matches_the_hand_written_one() {
    assert_same_spec(
        ListDir.spec(),
        ToolSpec::new(
            "list_dir",
            "List directory entries, directories first with a trailing `/`; non-recursive by default, so set `recursive` and `max_depth` to descend. Prefer this over `shell ls` to explore the workspace and over `grep` when you do not yet know the file name. `.git`, `target`, `node_modules`, `.venv` and `__pycache__` are always skipped. Fails when the path is missing or outside the workspace.",
            object_schema(
                json!({
                    "path": { "type": "string", "description": "Directory relative to the workspace root. Defaults to the root." },
                    "recursive": { "type": "boolean", "description": "Descend into subdirectories. Defaults to false." },
                    "max_depth": { "type": "integer", "description": "How deep to descend when recursive. Defaults to the whole tree." }
                }),
                &[],
            ),
        ),
    );
}

#[test]
fn edit_file_schema_matches_the_hand_written_one() {
    assert_same_spec(
        EditFile.spec(),
        ToolSpec::new(
            "edit_file",
            "Replace an exact string in an existing file. `old_string` must be unique unless `replace_all` is true, so include surrounding context. Prefer this over `write_file` for targeted changes. Fails when `old_string` is empty, is not found, occurs more than once without `replace_all`, or equals `new_string`; read the file first to copy the exact text.",
            object_schema(
                json!({
                    "path": { "type": "string", "description": "Path relative to the workspace root." },
                    "old_string": { "type": "string", "description": "Exact text to replace, including indentation." },
                    "new_string": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence. Defaults to false." }
                }),
                &["path", "old_string", "new_string"],
            ),
        ),
    );
}

#[test]
fn shell_schema_matches_the_hand_written_one() {
    assert_same_spec(
        Shell.spec(),
        ToolSpec::new(
            "shell",
            "Run a shell command in the workspace with stdout and stderr merged. Prefer `read_file`, `grep` and `list_dir` for reading and searching; use this for builds, tests and other programs. The result states a non-zero exit code, timeout or abort, and truncated output says how to narrow it. Fails when the command cannot start; keep `cwd` inside the workspace and raise `timeout_secs` for long runs.",
            object_schema(
                json!({
                    "command": { "type": "string", "description": "Command line to run through the shell." },
                    "cwd": { "type": "string", "description": "Directory relative to the workspace root. Defaults to the root." },
                    "timeout_secs": { "type": "integer", "description": "Wall-clock limit. Defaults to the configured shell timeout." }
                }),
                &["command"],
            ),
        ),
    );
}
