//! Addressing a session that already exists in the store.
//!
//! A session id outlives the process that created it: the conversation is in
//! SQLite, not in the server's memory. These tests seed the store directly, so
//! the server has never opened the session, and then speak to it over a socket —
//! which is exactly the case that used to be refused as `unknown_session`.

mod support;

use futures::SinkExt;
use harness_core::{ClientMessage, CompletionReason, Message, ServerMessage, SessionId};
use harness_llm::ScriptedTurn;
use serde_json::Value;
use support::{
    connect, error_code, kind, next_envelope, next_envelope_matching, scripting_factory,
    seed_stored_session, send_to, start, start_with, user_message, wait_for_history, wait_until,
    workspace,
};
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[tokio::test]
async fn a_message_to_a_stored_session_is_accepted_and_appends_to_it() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(scripting_factory(vec![ScriptedTurn::Text(
            "continued".to_string(),
        )])),
        None,
    )
    .await;

    let stored = seed_stored_session(
        &server.state,
        "run:test",
        &[
            Message::user("earlier question"),
            Message::assistant("earlier answer"),
        ],
    )
    .await;
    assert!(
        server.state.session(stored).is_none(),
        "the server has not opened this session"
    );

    let mut socket = connect(&server).await;
    send_to(&mut socket, stored, user_message("follow up")).await;

    let started =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "session_started").await;
    assert_eq!(
        started.session_id, stored,
        "the stored session is continued under its own id, not forked"
    );
    assert!(matches!(
        started.message,
        ServerMessage::SessionStarted { ref agent_id, .. } if agent_id.as_str() == "test"
    ));

    let done = next_envelope_matching(&mut socket, |envelope| {
        envelope.session_id == stored && kind(envelope) == "done"
    })
    .await;
    assert_eq!(
        done.message,
        ServerMessage::Done {
            reason: CompletionReason::EndTurn
        }
    );

    // The conversation grew in place: the earlier turns are still there and the
    // new exchange was appended to the same session.
    let history = wait_for_history(&server.state, stored, "the appended turn", |history| {
        history.len() >= 4 && history.iter().any(|message| message.text() == "continued")
    })
    .await;
    assert_eq!(history[0].text(), "earlier question");
    assert_eq!(history[1].text(), "earlier answer");
    assert_eq!(history[2].text(), "follow up");
    assert_eq!(history[3].text(), "continued");

    // The REST replay of the same session shows the reply, so a client reading
    // the conversation back sees the turn it just sent.
    let body: Value = reqwest::get(server.http_url(&format!("/api/sessions/{stored}/history")))
        .await
        .expect("the request completes")
        .json()
        .await
        .expect("the body is JSON");
    let texts: Vec<&str> = body["messages"]
        .as_array()
        .expect("a message list")
        .iter()
        .map(|message| message["content"].as_str().unwrap_or_default())
        .collect();
    assert!(texts.contains(&"earlier answer"), "{body}");
    assert!(texts.contains(&"continued"), "{body}");
}

/// The label is the only record of the agent, and it can outlive the agent file.
#[tokio::test]
async fn a_stored_session_labelled_with_a_missing_agent_runs_on_the_default() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(scripting_factory(vec![ScriptedTurn::Text(
            "ok".to_string(),
        )])),
        None,
    )
    .await;

    let stored = seed_stored_session(&server.state, "run:ghost", &[Message::user("hi")]).await;

    let mut socket = connect(&server).await;
    send_to(&mut socket, stored, user_message("still there?")).await;

    let started =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "session_started").await;
    assert!(
        matches!(
            started.message,
            ServerMessage::SessionStarted { ref agent_id, .. } if agent_id.as_str() == "test"
        ),
        "the fixture's default agent should run a label whose agent is gone"
    );
}

#[tokio::test]
async fn a_malformed_session_id_is_refused_naming_it() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    socket
        .send(WsMessage::text(
            "{\"v\":1,\"session_id\":\"not-a-ulid\",\"type\":\"user_message\",\"text\":\"hi\"}",
        ))
        .await
        .expect("the frame is written");

    let refusal = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(error_code(&refusal), Some("bad_message"));
    let ServerMessage::Error { message, .. } = &refusal.message else {
        panic!("expected an error frame, got {:?}", refusal.message);
    };
    assert!(
        message.contains("not-a-ulid"),
        "the refusal should name the id the client sent: {message}"
    );
}

#[tokio::test]
async fn a_well_formed_id_that_names_nothing_is_refused_under_that_id() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    // Well formed but never created: neither in this process's map nor in the
    // store.
    let missing = SessionId::new();
    send_to(&mut socket, missing, user_message("hello")).await;

    let refusal = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(error_code(&refusal), Some("unknown_session"));
    assert_eq!(
        refusal.session_id, missing,
        "the refusal must carry the requested id, not a freshly minted one"
    );
    let ServerMessage::Error { message, .. } = &refusal.message else {
        panic!("expected an error frame, got {:?}", refusal.message);
    };
    assert!(
        message.contains(&missing.to_string()),
        "the refusal should name the id the client sent: {message}"
    );
}

#[tokio::test]
async fn a_connection_can_subscribe_to_a_stored_session() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(scripting_factory(vec![ScriptedTurn::Text(
            "hello".to_string(),
        )])),
        None,
    )
    .await;

    // `serve:` is the other label this server writes, and it resolves the same
    // way as `run:`.
    let stored =
        seed_stored_session(&server.state, "serve:test", &[Message::user("earlier")]).await;

    let mut watcher = connect(&server).await;
    send_to(
        &mut watcher,
        stored,
        ClientMessage::Subscribe { session_id: stored },
    )
    .await;

    // Subscribing to a stored session opens it on demand rather than refusing.
    wait_until("the stored session to be registered", || {
        server.state.session(stored).is_some()
    })
    .await;

    // A different connection drives the session, and the subscriber sees its
    // events — which is the point of subscribing.
    let mut driver = connect(&server).await;
    send_to(&mut driver, stored, user_message("go")).await;

    let started = next_envelope_matching(&mut watcher, |envelope| {
        envelope.session_id == stored && kind(envelope) == "session_started"
    })
    .await;
    assert!(matches!(
        started.message,
        ServerMessage::SessionStarted { ref agent_id, .. } if agent_id.as_str() == "test"
    ));

    let done = next_envelope_matching(&mut watcher, |envelope| {
        envelope.session_id == stored && kind(envelope) == "done"
    })
    .await;
    assert_eq!(
        done.message,
        ServerMessage::Done {
            reason: CompletionReason::EndTurn
        }
    );
}
