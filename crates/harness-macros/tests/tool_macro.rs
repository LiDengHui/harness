//! End-to-end checks of what `#[harness::tool]` generates.
//!
//! Integration tests rather than unit tests: a proc-macro crate cannot expand
//! its own macros, and the generated code names `::harness_core`,
//! `::harness_tools` and `::serde_json` by absolute path, so the honest test is
//! one that consumes the macro exactly the way a real crate does.

use std::sync::Arc;

use harness_core::{HarnessError, ToolsConfig};
use harness_macros::tool;
use harness_tools::{Tool, ToolContext, ToolRegistry};
use serde_json::json;

// `#[harness::tool]` above the derive is the documented order; `CountArgs`
// below exercises the opposite order to prove neither one is required.
#[tool(
    name = "echo",
    description = "Echo the text back, with a few typed arguments"
)]
#[derive(serde::Deserialize)]
struct EchoArgs {
    /// The text to echo.
    text: String,
    /// Where to write it, if anywhere.
    target: Option<std::path::PathBuf>,
    /// How many times to repeat.
    times: Option<u32>,
    /// Tags to attach, in order.
    tags: Vec<String>,
    scale: f64,
    /// Whether to shout.
    loud: bool,
}

async fn echo(
    args: EchoArgs,
    _ctx: &harness_tools::ToolContext,
) -> harness_core::Result<harness_tools::ToolOutput> {
    let text = if args.loud {
        args.text.to_uppercase()
    } else {
        args.text.clone()
    };
    let times = args.times.unwrap_or(1).max(1);

    Ok(
        harness_tools::ToolOutput::text(text.repeat(times as usize)).with_metadata(json!({
            "tags": args.tags,
            "scale": args.scale,
            "target": args.target.map(|path| path.display().to_string()),
        })),
    )
}

#[derive(serde::Deserialize)]
#[tool(
    name = "count-lines",
    description = "Count the lines in a snippet",
    handler = "count_lines"
)]
struct CountArgs {
    /// The text to count.
    text: String,
}

async fn count_lines(
    args: CountArgs,
    _ctx: &harness_tools::ToolContext,
) -> harness_core::Result<harness_tools::ToolOutput> {
    Ok(harness_tools::ToolOutput::text(
        args.text.lines().count().to_string(),
    ))
}

#[tool(name = "shout", description = "Shout a word back", type_name = "Shout")]
#[derive(serde::Deserialize)]
struct ShoutArgs {
    /// The word to shout.
    word: String,
}

async fn shout(
    args: ShoutArgs,
    _ctx: &harness_tools::ToolContext,
) -> harness_core::Result<harness_tools::ToolOutput> {
    Ok(harness_tools::ToolOutput::text(args.word.to_uppercase()))
}

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(root, ToolsConfig::default())
}

#[test]
fn the_schema_comes_from_the_argument_struct() {
    let spec = EchoTool.spec();
    assert_eq!(spec.name, "echo");
    assert_eq!(
        spec.description,
        "Echo the text back, with a few typed arguments"
    );

    let parameters = &spec.parameters;
    assert_eq!(parameters["type"], "object");
    assert_eq!(parameters["additionalProperties"], false);

    let properties = &parameters["properties"];
    assert_eq!(properties["text"]["type"], "string");
    assert_eq!(properties["text"]["description"], "The text to echo.");
    assert_eq!(properties["target"]["type"], "string");
    assert_eq!(properties["times"]["type"], "integer");
    assert_eq!(properties["tags"]["type"], "array");
    assert_eq!(properties["tags"]["items"]["type"], "string");
    assert_eq!(properties["scale"]["type"], "number");
    assert_eq!(properties["loud"]["type"], "boolean");
    assert_eq!(properties["loud"]["description"], "Whether to shout.");

    assert!(
        properties["scale"].get("description").is_none(),
        "a field without a doc comment carries no description"
    );
}

#[test]
fn optional_fields_are_absent_from_required() {
    let parameters = EchoTool.spec().parameters;
    let required: Vec<&str> = parameters["required"]
        .as_array()
        .expect("required is an array")
        .iter()
        .map(|name| name.as_str().expect("required holds strings"))
        .collect();

    assert_eq!(required, vec!["text", "tags", "scale", "loud"]);
    assert!(!required.contains(&"times"), "Option<T> is not required");
    assert!(!required.contains(&"target"), "Option<T> is not required");
}

#[test]
fn the_handler_can_be_named_explicitly() {
    let spec = CountLinesTool.spec();
    assert_eq!(spec.name, "count-lines");
    assert_eq!(spec.parameters["required"][0], "text");
    assert!(spec.parameters["properties"]["text"]["description"]
        .as_str()
        .is_some_and(|text| text.contains("count")));
}

#[test]
fn type_name_replaces_the_derived_struct_name() {
    let spec = Shout.spec();
    assert_eq!(spec.name, "shout");
    assert_eq!(spec.parameters["required"], json!(["word"]));
    assert_eq!(
        spec.parameters["properties"]["word"]["description"],
        "The word to shout."
    );
}

#[tokio::test]
async fn call_round_trips_arguments_through_a_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    registry.register(Arc::new(CountLinesTool));
    assert_eq!(registry.names(), vec!["count-lines", "echo"]);
    assert_eq!(registry.specs()[1], EchoTool.spec());

    let echo = registry.get("echo").expect("registered");
    let output = echo
        .call(
            json!({
                "text": "hi",
                "tags": ["a", "b"],
                "scale": 1.5,
                "loud": true,
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert_eq!(output.content, "HI");
    assert!(!output.is_error);
    assert_eq!(output.metadata["tags"], json!(["a", "b"]));
    assert_eq!(output.metadata["scale"], 1.5);
    assert_eq!(
        output.metadata["target"],
        serde_json::Value::Null,
        "an absent Option<T> deserializes to None"
    );

    let output = echo
        .call(
            json!({
                "text": "ha",
                "target": "notes.txt",
                "times": 3,
                "tags": [],
                "scale": 0.0,
                "loud": false,
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(output.content, "hahaha");
    assert_eq!(output.metadata["target"], "notes.txt");

    let counted = registry.get("count-lines").expect("registered");
    let output = counted
        .call(json!({ "text": "one\ntwo\nthree" }), &ctx)
        .await
        .unwrap();
    assert_eq!(output.content, "3");
}

#[tokio::test]
async fn arguments_are_deserialized_into_the_struct() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());

    let err = EchoTool.call(json!({}), &ctx).await.unwrap_err();
    assert!(matches!(err, HarnessError::Tool(_)), "{err}");
    assert!(err.to_string().contains("echo: invalid arguments"), "{err}");
    assert!(err.to_string().contains("text"), "{err}");

    let err = EchoTool
        .call(
            json!({ "text": "hi", "tags": "not a list", "scale": 1, "loud": false }),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, HarnessError::Tool(_)), "{err}");
    assert!(err.to_string().contains("echo: invalid arguments"), "{err}");
}
