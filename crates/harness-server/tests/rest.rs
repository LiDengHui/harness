//! The REST surface, exercised over a real loopback connection.

mod support;

use std::sync::Arc;

use harness_core::{Message, SessionId, ToolCall};
use harness_server::ServerState;
use serde_json::Value;
use support::{connect, send, start, user_message, workspace};

async fn get(server: &support::TestServer, path: &str) -> (reqwest::StatusCode, Value) {
    let response = reqwest::get(server.http_url(path))
        .await
        .expect("the request completes");
    let status = response.status();
    let body: Value = response.json().await.expect("the body is JSON");
    (status, body)
}

/// A server whose configured house language is `tag`.
///
/// Built here rather than through `support::start`, which always starts from the
/// fixture's default config; the language is the one field this endpoint reads.
async fn start_with_language(fixture: &support::Fixture, tag: &str) -> support::TestServer {
    let mut config = fixture.config();
    config.ui.language = tag.to_string();

    let state = Arc::new(
        ServerState::new(config, fixture.path().to_path_buf())
            .await
            .expect("the server state builds"),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available");
    let addr = listener.local_addr().expect("the socket is bound");
    let app = Arc::clone(&state).router();
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            eprintln!("test server stopped: {err}");
        }
    });

    support::TestServer { addr, state }
}

#[tokio::test]
async fn health_reports_the_protocol_and_version() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let (status, body) = get(&server, "/api/health").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["protocol"], harness_core::PROTOCOL_VERSION);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn ui_reports_the_configured_house_language() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let (status, body) = get(&server, "/api/ui").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    // The exact object, so a field the front end does not expect fails here.
    assert_eq!(
        body,
        serde_json::json!({
            "language": "zh-CN",
            "thinking": {
                "efforts": ["low", "high", "max"],
                "default": "high",
            },
        })
    );
}

#[tokio::test]
async fn ui_honours_a_supported_override_and_falls_back_on_anything_else() {
    let english = workspace();
    let server = start_with_language(&english, "EN").await;
    let (status, body) = get(&server, "/api/ui").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        body["language"], "en",
        "the tag is canonicalized, not echoed"
    );

    // A language the UI ships no strings for must not reach the browser.
    let french = workspace();
    let server = start_with_language(&french, "fr").await;
    let (status, body) = get(&server, "/api/ui").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["language"], "zh-CN");
}

#[tokio::test]
async fn agents_lists_what_the_registry_found() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let (status, body) = get(&server, "/api/agents").await;
    assert_eq!(status, reqwest::StatusCode::OK);

    let agents = body.as_array().expect("an array of agents");
    assert_eq!(agents.len(), 1, "{body}");

    let agent = &agents[0];
    assert_eq!(agent["id"], "test");
    assert_eq!(agent["name"], "Test Agent");
    assert_eq!(agent["description"], "A fixture agent.");
    assert_eq!(agent["model"], Value::Null);
    assert_eq!(agent["tools"], serde_json::json!([]));
    assert_eq!(agent["skills"], serde_json::json!([]));
    assert_eq!(agent["max_tokens"], Value::Null);
    assert_eq!(agent["thinking_effort"], Value::Null);
    assert_eq!(
        agent["source_path"],
        agent["source_path"]
            .as_str()
            .expect("a path string")
            .to_string()
    );
    assert!(agent["source_path"]
        .as_str()
        .expect("a path")
        .ends_with("test.agent.md"));
}

#[tokio::test]
async fn memory_stats_are_reported_and_grow_with_a_session() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let (status, body) = get(&server, "/api/memory/stats").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["sessions"], 0);
    assert_eq!(body["nodes"], 0);

    // Start a session so the counters have something to describe.
    let mut socket = connect(&server).await;
    send(&mut socket, user_message("read the manifest")).await;
    let session_id = support::next_envelope(&mut socket)
        .await
        .expect("a first message")
        .session_id;
    support::wait_for_history(&server.state, session_id, "the persisted run", |history| {
        !history.is_empty()
    })
    .await;

    let (status, body) = get(&server, "/api/memory/stats").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["sessions"], 1);
    assert!(body["nodes"].as_u64().expect("a count") > 0, "{body}");
}

#[tokio::test]
async fn sessions_are_listed_as_json() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let (status, body) = get(&server, "/api/sessions").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));

    let mut socket = connect(&server).await;
    send(&mut socket, user_message("hello")).await;
    let session_id = support::next_envelope(&mut socket)
        .await
        .expect("a first message")
        .session_id;

    support::wait_until("the session to be listed", || {
        server.state.session_count() == 1
    })
    .await;
    // The title is read from the persisted first user turn, so wait for the
    // turn itself rather than the in-memory session count.
    support::wait_for_history(&server.state, session_id, "the persisted turn", |history| {
        !history.is_empty()
    })
    .await;

    let (status, body) = get(&server, "/api/sessions").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let sessions = body.as_array().expect("an array of sessions");
    assert_eq!(sessions.len(), 1, "{body}");
    assert_eq!(sessions[0]["id"], session_id.to_string());
    assert_eq!(sessions[0]["label"], "serve:test");
    assert_eq!(sessions[0]["title"], "hello");
}

#[tokio::test]
async fn history_replays_the_full_conversation_in_order() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let memory = server.state.memory();

    let session = memory
        .create_session(Some("replay"))
        .await
        .expect("a session");
    memory
        .append(session, &Message::user("what is in the manifest?"))
        .await
        .expect("the first turn");
    memory
        .append(
            session,
            &Message::assistant_tool_calls(vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({ "path": "Cargo.toml" }),
            }]),
        )
        .await
        .expect("the tool call");
    memory
        .append(
            session,
            &Message::tool_result("call_1", "[package]\nname = \"fixture\""),
        )
        .await
        .expect("the tool result");
    memory
        .append(session, &Message::assistant("A fixture package."))
        .await
        .expect("the answer");

    let (status, body) = get(&server, &format!("/api/sessions/{session}/history")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["session"]["id"], session.to_string());
    assert_eq!(body["session"]["title"], "what is in the manifest?");

    let messages = body["messages"].as_array().expect("an array of messages");
    assert_eq!(messages.len(), 4, "{body}");

    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "what is in the manifest?");

    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["tool_calls"][0]["name"], "read_file");
    assert_eq!(
        messages[1]["tool_calls"][0]["arguments"]["path"],
        "Cargo.toml"
    );

    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "call_1");
    assert_eq!(messages[2]["content"], "[package]\nname = \"fixture\"");

    assert_eq!(messages[3]["role"], "assistant");
    assert_eq!(messages[3]["content"], "A fixture package.");
}

#[tokio::test]
async fn an_empty_session_replays_as_no_messages() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let session = server
        .state
        .memory()
        .create_session(Some("empty"))
        .await
        .expect("a session");

    let (status, body) = get(&server, &format!("/api/sessions/{session}/history")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["messages"], serde_json::json!([]));
    assert_eq!(body["session"]["id"], session.to_string());
}

#[tokio::test]
async fn an_unknown_session_history_is_a_json_404() {
    let fixture = workspace();
    let server = start(&fixture).await;

    // Malformed: not a ULID at all.
    let (status, body) = get(&server, "/api/sessions/nope/history").await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "no such session");

    // Well formed but never created.
    let missing = SessionId::new();
    let (status, body) = get(&server, &format!("/api/sessions/{missing}/history")).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert!(body["error"].is_string(), "{body}");
}

/// The rail filters sessions by their parent, so the distinction has to survive
/// serialisation: a sub-agent's session points at the session it forked from,
/// and the session that started the plan points at nothing.
#[tokio::test]
async fn sessions_expose_the_parent_a_sub_agent_forked_from() {
    let fixture = workspace();
    let plan = serde_json::json!({
        "nodes": [{
            "id": "read-manifest",
            "objective": "read the manifest and report what it declares",
        }]
    })
    .to_string();
    let result = serde_json::json!({
        "objective": "read the manifest and report what it declares",
        "state": "done",
        "evidence": "read Cargo.toml",
        "boundary": "nothing else was touched",
    })
    .to_string();
    let server = support::start_with(
        &fixture,
        Some(support::scripting_factory(vec![
            harness_llm::ScriptedTurn::Text(plan),
            harness_llm::ScriptedTurn::Text(result),
        ])),
        None,
    )
    .await;

    let mut socket = connect(&server).await;
    send(
        &mut socket,
        harness_core::ClientMessage::PlanTask {
            task: "read the manifest".to_string(),
            agent_id: None,
        },
    )
    .await;

    let start = support::next_envelope_matching(&mut socket, |envelope| {
        support::kind(envelope) == "subtask_start"
    })
    .await;
    let parent = start.session_id;
    let child = start
        .subagent_session_id
        .expect("the worker's frame names the session it was stored under");
    support::next_envelope_matching(&mut socket, |envelope| support::kind(envelope) == "done")
        .await;

    let (status, body) = get(&server, "/api/sessions").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let sessions = body.as_array().expect("an array of sessions");
    let listed = |id: SessionId| {
        sessions
            .iter()
            .find(|session| session["id"] == id.to_string())
            .unwrap_or_else(|| panic!("no session `{id}` in {body}"))
    };

    assert_eq!(
        listed(child)["forked_from_session"],
        parent.to_string(),
        "the worker's session names the plan's session"
    );
    assert_eq!(
        listed(parent)["forked_from_session"],
        Value::Null,
        "the plan's session is a root, so the rail keeps it"
    );
}

#[tokio::test]
async fn an_unknown_path_is_a_json_404() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let (status, body) = get(&server, "/api/nope").await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert!(
        body["error"]
            .as_str()
            .expect("an error string")
            .contains("/api/nope"),
        "{body}"
    );
}
