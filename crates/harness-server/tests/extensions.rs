//! `GET /api/extensions`: what the harness can reach for, as the UI shows it.

mod support;

use harness_core::McpServerConfig;
use serde_json::Value;
use support::{start, workspace, Fixture};

async fn extensions(server: &support::TestServer) -> Value {
    let response = reqwest::get(server.http_url("/api/extensions"))
        .await
        .expect("the request completes");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.expect("a JSON body")
}

/// A skill in the documented layout, so the registry has something to report.
fn write_skill(fixture: &Fixture, name: &str) {
    let dir = fixture.path().join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("the skills directory is writable");
    std::fs::write(
        dir.join("SKILL.md"),
        "---\ndescription: A fixture skill.\nmodel: mock/mock-1\nallowed-tools:\n  - read_file\n---\n\nDo the fixture thing.\n",
    )
    .expect("the skill file is writable");
}

fn write_plugin(fixture: &Fixture, name: &str) {
    let dir = fixture.path().join("plugins");
    std::fs::create_dir_all(&dir).expect("the plugins directory is writable");
    std::fs::write(
        dir.join(format!("{name}.plugin.toml")),
        format!("name = \"{name}\"\nversion = \"2.0.0\"\nentry = \"run\"\ncapabilities = [\"file_read\"]\n"),
    )
    .expect("the manifest is writable");
    std::fs::write(dir.join(format!("{name}.wat")), "(module)").expect("the module is writable");
}

#[tokio::test]
async fn extensions_report_skills_mcp_servers_and_plugins() {
    let mut fixture = workspace();
    write_skill(&fixture, "demo");
    write_plugin(&fixture, "demo");
    fixture.add_mcp_server(
        "remote",
        McpServerConfig {
            url: Some("https://example.invalid/mcp".to_string()),
            ..Default::default()
        },
    );
    let server = start(&fixture).await;

    let body = extensions(&server).await;

    let skills = body["skills"].as_array().expect("an array of skills");
    let skill = skills
        .iter()
        .find(|skill| skill["name"] == "demo")
        .unwrap_or_else(|| panic!("the fixture skill is missing: {body}"));
    assert_eq!(skill["description"], "A fixture skill.");
    assert_eq!(skill["model"], "mock/mock-1");
    assert_eq!(skill["allowed_tools"], serde_json::json!(["read_file"]));
    assert!(
        skill["metadata_tokens"].as_u64().expect("a count") > 0,
        "{skill}"
    );
    assert!(
        skill["body_tokens"].as_u64().expect("a count") > 0,
        "{skill}"
    );
    assert!(
        skill["source_path"]
            .as_str()
            .expect("a path")
            .ends_with("SKILL.md"),
        "{skill}"
    );

    let mcp = body["mcp"].as_array().expect("an array of servers");
    assert_eq!(mcp.len(), 1, "{body}");
    assert_eq!(mcp[0]["name"], "remote");
    assert_eq!(mcp[0]["kind"], "http");
    assert_eq!(mcp[0]["enabled"], true);
    assert_eq!(mcp[0]["lazy"], true);
    assert_eq!(mcp[0]["target"], "https://example.invalid/mcp");
    // Tool names are only knowable by connecting, which a page load must not do.
    assert_eq!(mcp[0]["tools"], serde_json::json!([]));

    let plugins = body["plugins"].as_array().expect("an array of plugins");
    assert_eq!(plugins.len(), 1, "{body}");
    assert_eq!(plugins[0]["name"], "demo");
    assert_eq!(plugins[0]["version"], "2.0.0");
    assert_eq!(plugins[0]["entry"], "run");
    assert_eq!(plugins[0]["capabilities"], serde_json::json!(["file_read"]));
    assert_eq!(plugins[0]["module"], "plugins/demo.wat");
}

#[tokio::test]
async fn a_stdio_server_reports_its_command_as_the_target() {
    let mut fixture = workspace();
    fixture.add_mcp_server(
        "fs",
        McpServerConfig {
            command: Some("mcp-fs".to_string()),
            args: vec!["--root".to_string(), ".".to_string()],
            lazy: false,
            ..Default::default()
        },
    );
    let server = start(&fixture).await;

    let body = extensions(&server).await;
    let mcp = body["mcp"].as_array().expect("an array of servers");
    assert_eq!(mcp.len(), 1, "{body}");
    assert_eq!(mcp[0]["kind"], "stdio");
    assert_eq!(mcp[0]["target"], "mcp-fs");
    assert_eq!(mcp[0]["lazy"], false);
}

#[tokio::test]
async fn a_server_with_no_transport_is_reported_as_invalid() {
    let mut fixture = workspace();
    // Neither `command` nor `url`: the config cannot be acted on, and saying so
    // beats picking one of the two transports arbitrarily.
    fixture.add_mcp_server("broken", McpServerConfig::default());
    let server = start(&fixture).await;

    let body = extensions(&server).await;
    let mcp = body["mcp"].as_array().expect("an array of servers");
    assert_eq!(mcp.len(), 1, "{body}");
    assert_eq!(mcp[0]["kind"], "invalid");
    assert_eq!(mcp[0]["target"], "");
}

#[tokio::test]
async fn an_empty_workspace_answers_with_empty_arrays() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let body = extensions(&server).await;
    assert_eq!(body["mcp"], serde_json::json!([]), "{body}");
    assert_eq!(body["plugins"], serde_json::json!([]), "{body}");
    assert!(body["skills"].is_array(), "{body}");
}
