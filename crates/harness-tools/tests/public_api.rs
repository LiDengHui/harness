//! Exercises the crate the way another crate will: through the public API only.

use harness_core::{HarnessError, ToolsConfig};
use harness_tools::{AbortFlag, ProgressSender, ToolContext, ToolOutput, ToolRegistry};
use serde_json::json;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(root.to_path_buf(), ToolsConfig::default())
}

#[test]
fn output_helpers_and_abort_flag_are_public() {
    let output = ToolOutput::text("hi").with_metadata(json!({ "n": 1 }));
    assert!(!output.is_error);
    assert_eq!(output.metadata["n"], 1);

    let failure = ToolOutput::error("bad");
    assert!(failure.is_error);

    let abort = AbortFlag::new();
    assert!(!abort.is_aborted());
    abort.abort();
    assert!(abort.is_aborted());
}

#[test]
fn the_registry_advertises_its_tools() {
    let registry = ToolRegistry::with_builtins();
    assert_eq!(registry.len(), 7);
    assert!(!registry.is_empty());
    assert_eq!(
        registry.names(),
        vec![
            "edit_file",
            "grep",
            "list_dir",
            "read_file",
            "shell",
            "web_fetch",
            "write_file"
        ]
    );

    let specs = registry.specs();
    assert!(specs.iter().any(|spec| spec.name == "read_file"));
    assert!(specs.iter().all(|spec| spec.parameters["type"] == "object"));

    assert!(registry.get("read_file").is_some());
    assert!(registry.get("nope").is_none());
    assert_eq!(registry.filter(&["grep".to_string()]).names(), vec!["grep"]);
    assert!(ToolRegistry::new().is_empty());
    assert_eq!(ToolRegistry::default().len(), 0);
}

#[tokio::test]
async fn a_full_edit_session_works_through_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let ctx = context(root);
    let registry = ToolRegistry::with_builtins();

    let write = registry.get("write_file").expect("write_file");
    let read = registry.get("read_file").expect("read_file");
    let edit = registry.get("edit_file").expect("edit_file");
    let list = registry.get("list_dir").expect("list_dir");
    let grep = registry.get("grep").expect("grep");

    write
        .call(
            json!({ "path": "src/app.rs", "content": "fn main() {}\n" }),
            &ctx,
        )
        .await
        .expect("write");

    let read_out = read
        .call(json!({ "path": "src/app.rs" }), &ctx)
        .await
        .expect("read");
    assert_eq!(read_out.content, "fn main() {}\n");

    let edit_out = edit
        .call(
            json!({
                "path": "src/app.rs",
                "old_string": "fn main() {}",
                "new_string": "fn main() { greet(); }"
            }),
            &ctx,
        )
        .await
        .expect("edit");
    assert_eq!(edit_out.metadata["replacements"], 1);

    let listing = list
        .call(json!({ "recursive": true }), &ctx)
        .await
        .expect("list");
    assert_eq!(listing.content, "src/\nsrc/app.rs");

    let hits = grep
        .call(json!({ "pattern": "greet" }), &ctx)
        .await
        .expect("grep");
    assert_eq!(hits.content, "src/app.rs:1: fn main() { greet(); }");
    assert_eq!(hits.metadata["matches"], 1);
}

#[tokio::test]
async fn progress_messages_reach_the_contexts_sender() {
    let tmp = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut ctx = context(tmp.path());
    ctx.progress = Some(ProgressSender::from(sender));

    let write = ToolRegistry::with_builtins()
        .get("write_file")
        .expect("write_file");
    write
        .call(json!({ "path": "notes.txt", "content": "x" }), &ctx)
        .await
        .expect("write");

    let message = receiver.try_recv().expect("a progress message");
    assert!(message.contains("notes.txt"), "{message}");
}

#[tokio::test]
async fn escape_attempts_and_read_limits_hold_across_the_api() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(tmp.path().join("outside.txt"), "secret").unwrap();

    let mut ctx = context(&root);
    let registry = ToolRegistry::with_builtins();

    let err = registry
        .get("read_file")
        .expect("read_file")
        .call(json!({ "path": "../outside.txt" }), &ctx)
        .await
        .expect_err("path escape");
    assert!(matches!(err, HarnessError::PathEscape(_)), "{err}");

    // A file larger than the read limit is paged, not refused: the response
    // states the real size and the offset that continues the read.
    std::fs::write(root.join("big.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    ctx.config.max_read_bytes = 10;
    let read = registry.get("read_file").expect("read_file");

    let first = read
        .call(json!({ "path": "big.txt" }), &ctx)
        .await
        .expect("a large file is read in a window, not refused");
    assert_eq!(first.metadata["total_bytes"], 19);
    assert_eq!(first.metadata["truncated"], true);
    assert!(
        first
            .content
            .contains("[output truncated: showed 7 of 19 bytes"),
        "{}",
        first.content
    );

    let next = first.metadata["next_offset"]
        .as_u64()
        .expect("a continuation");
    let rest = read
        .call(json!({ "path": "big.txt", "offset": next }), &ctx)
        .await
        .expect("continuation");
    assert_eq!(rest.content, "three\nfour");
    assert_eq!(rest.metadata["truncated"], false);
}
