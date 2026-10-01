//! Deleting a session makes its history a 404 and drops it from the list.
//!
//! The memory layer proves the rows are gone
//! (`crates/harness-memory/tests/delete_session.rs`); this is the reader's side
//! of the same fact: the REST surface must treat a deleted session exactly like
//! one that never existed, rather than serving a half-empty history.

mod support;

use harness_core::Message;
use serde_json::Value;
use support::{seed_stored_session, start, workspace};

async fn get(server: &support::TestServer, path: &str) -> (reqwest::StatusCode, Value) {
    let response = reqwest::get(server.http_url(path))
        .await
        .expect("the request completes");
    let status = response.status();
    let body: Value = response.json().await.expect("the body is JSON");
    (status, body)
}

#[tokio::test]
async fn a_deleted_session_is_a_404_and_leaves_the_list() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let session = seed_stored_session(
        &server.state,
        "run:test",
        &[Message::user("a turn that will be deleted")],
    )
    .await;

    // Before: the history reads and the session is listed.
    let (status, body) = get(&server, &format!("/api/sessions/{session}/history")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["session"]["id"], session.to_string());

    let (status, body) = get(&server, "/api/sessions").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(
        body.as_array()
            .expect("an array")
            .iter()
            .any(|entry| entry["id"] == session.to_string()),
        "{body}"
    );

    let report = server
        .state
        .memory()
        .delete_session(session)
        .await
        .expect("the delete runs")
        .expect("the session existed");
    assert!(report.nodes > 0, "{report:?}");

    // After: the same request is the same answer as for a session that never
    // existed, and the list no longer carries it.
    let (status, body) = get(&server, &format!("/api/sessions/{session}/history")).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "no such session");

    let (status, body) = get(&server, "/api/sessions").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(
        !body
            .as_array()
            .expect("an array")
            .iter()
            .any(|entry| entry["id"] == session.to_string()),
        "the deleted session must not be listed: {body}"
    );
}
