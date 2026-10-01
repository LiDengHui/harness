//! The workflow create route checks a stage's `agent:` before it saves.
//!
//! This is the end of the chain that starts in `harness-cli`'s `load_registry`:
//! the registry installed on the server carries the workspace's agent ids, and
//! `WorkflowRegistry::save_new` refuses a spec whose stage names one nobody
//! defines. Without that, `POST /api/workflows` could persist a procedure that
//! looks runnable and fails only when a user picks it.

mod support;

use std::sync::Arc;

use harness_server::ServerState;
use harness_workflows::WorkflowRegistry;
use serde_json::{json, Value};
use support::{scripting_factory, workspace, Fixture, TestServer};

/// A server whose workflow registry carries the fixture's agent ids, and whose
/// only provider answers the authoring call with `reply`.
async fn start_with_agents(fixture: &Fixture, reply: &str) -> TestServer {
    let registry = WorkflowRegistry::load_with_agents(fixture.path(), ["test".to_string()])
        .expect("the fixture's workflows load");

    let state = ServerState::new(fixture.config(), fixture.path().to_path_buf())
        .await
        .expect("the state builds")
        .with_provider_factory(scripting_factory(vec![harness_llm::ScriptedTurn::Text(
            reply.to_string(),
        )]))
        .with_workflow_registry(Arc::new(registry));

    let state = Arc::new(state);
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

    TestServer { addr, state }
}

fn authored(id: &str, agent: &str) -> String {
    json!({
        "id": id,
        "name": "Authored",
        "description": "A workflow the tests authored.",
        "when": "When a test needs one.",
        "guidance": "Run the stage.",
        "stages": [{ "id": "go", "objective": "Do the thing.", "agent": agent }],
    })
    .to_string()
}

async fn post_workflow(server: &TestServer, description: &str) -> (reqwest::StatusCode, Value) {
    let response = reqwest::Client::new()
        .post(server.http_url("/api/workflows"))
        .json(&json!({ "description": description }))
        .send()
        .await
        .expect("the request completes");
    let status = response.status();
    let body: Value = response.json().await.expect("the body is JSON");
    (status, body)
}

#[tokio::test]
async fn a_workflow_naming_an_undefined_agent_is_refused_and_nothing_is_written() {
    let fixture = workspace();
    // The fixture declares exactly one agent, `test`; `ghost` is the missing one.
    let server = start_with_agents(&fixture, &authored("ghost-runner", "ghost")).await;

    let (status, body) = post_workflow(&server, "run it").await;

    assert_eq!(
        status,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "a workflow that cannot run must not be saved: {body}"
    );
    assert_eq!(body["error"], "the workflow could not be saved");
    assert!(
        !fixture.path().join("workflows").exists(),
        "no directory or file may be created for a refused workflow"
    );
}

#[tokio::test]
async fn a_workflow_naming_a_defined_agent_is_saved() {
    let fixture = workspace();
    let server = start_with_agents(&fixture, &authored("real-runner", "test")).await;

    let (status, body) = post_workflow(&server, "run it").await;

    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["id"], "real-runner");
    assert!(
        fixture
            .path()
            .join("workflows/real-runner.workflow.md")
            .is_file(),
        "the accepted workflow must reach disk"
    );
}
