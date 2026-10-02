//! End-to-end tests of the WebSocket bus against a real socket.
//!
//! Every test here binds a real listener on `127.0.0.1:0` and speaks the wire
//! protocol through `tokio_tungstenite`, because the parts worth testing — the
//! upgrade, the two loops ending together, the fan-out and its backpressure —
//! only exist once there is a socket between them.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use harness_core::{AgentId, ClientMessage, CompletionReason, Role, ServerMessage, SessionId};
use harness_llm::{MockProvider, ScriptedTurn};
use harness_server::BackpressurePolicy;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use support::{
    connect, error_code, kind, next_envelope, next_envelope_matching, scripting_factory, send,
    send_to, shared_factory, start, start_with, steer, tool_then_text, user_message,
    user_message_with_effort, wait_for_history, wait_until, workspace, Socket,
};

/// A shell that outlives any abort by orders of magnitude, so a test can be
/// certain the run was interrupted rather than merely fast.
fn long_shell() -> serde_json::Value {
    json!({ "command": "sleep 30" })
}

#[tokio::test]
async fn a_full_run_streams_the_expected_sequence() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("read the manifest")).await;

    let mut kinds: Vec<String> = Vec::new();
    let mut tools: Vec<String> = Vec::new();

    let (session_id, done) = loop {
        let envelope = next_envelope(&mut socket)
            .await
            .expect("the server closed before finishing the run");

        assert_eq!(envelope.v, 1, "every envelope declares protocol version 1");
        assert_eq!(
            envelope.agent_id.as_str(),
            "test",
            "the fixture agent is used"
        );

        if let ServerMessage::ToolCallStart { name, .. } = &envelope.message {
            tools.push(name.clone());
        }

        kinds.push(kind(&envelope));

        if let ServerMessage::Done { reason } = envelope.message {
            break (envelope.session_id, reason);
        }
    };

    assert_eq!(kinds.first().map(String::as_str), Some("session_started"));
    assert!(kinds.contains(&"assistant_chunk".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"tool_call_start".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"tool_call_end".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"token_usage".to_string()), "{kinds:?}");
    assert_eq!(kinds.last().map(String::as_str), Some("done"));

    let started = kinds
        .iter()
        .position(|kind| kind == "tool_call_start")
        .expect("a tool started");
    let ended = kinds
        .iter()
        .position(|kind| kind == "tool_call_end")
        .expect("a tool ended");
    assert!(
        started < ended,
        "tool events arrived out of order: {kinds:?}"
    );

    assert_eq!(tools, vec!["read_file".to_string()]);
    assert_eq!(done, CompletionReason::EndTurn);

    // The conversation is persisted, and the session is addressable afterwards.
    let history = wait_for_history(&server.state, session_id, "the persisted run", |history| {
        history.iter().any(|message| message.role == Role::Tool)
            && history
                .iter()
                .any(|message| message.role == Role::Assistant)
    })
    .await;

    assert_eq!(
        history.first().map(|m| m.text().to_string()),
        Some("read the manifest".to_string())
    );
    assert!(
        history
            .iter()
            .any(|message| message.text().contains("fixture")),
        "the tool output should carry the manifest: {history:?}"
    );
}

#[tokio::test]
async fn the_agent_named_on_a_user_message_is_the_one_that_runs() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    send(
        &mut socket,
        ClientMessage::UserMessage {
            text: "hello".to_string(),
            agent_id: Some(AgentId::new("test").expect("a valid id")),
            effort: None,
            permission_mode: None,
        },
    )
    .await;

    let started =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "session_started").await;
    assert!(matches!(
        started.message,
        ServerMessage::SessionStarted { ref agent_id, ref model, .. }
            if agent_id.as_str() == "test" && model == "mock-1"
    ));
}

#[tokio::test]
async fn an_unknown_agent_is_refused_before_a_session_is_created() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    send(
        &mut socket,
        ClientMessage::UserMessage {
            text: "hello".to_string(),
            agent_id: Some(AgentId::new("nobody").expect("a valid id")),
            effort: None,
            permission_mode: None,
        },
    )
    .await;

    let refusal = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(error_code(&refusal), Some("agent_not_found"));
    assert_eq!(server.state.session_count(), 0, "no session was registered");
}

/// Starts a server whose single provider records what it was asked for, after
/// rewriting the fixture agent's frontmatter so a test can set the declared
/// reasoning effort.
async fn effort_recording_server(
    fixture: &support::Fixture,
    thinking_effort: Option<&str>,
) -> (support::TestServer, Arc<MockProvider>) {
    let frontmatter = match thinking_effort {
        Some(effort) => format!(
            "---\nname: Test Agent\nthinking_effort: {effort}\n---\nYou are a test agent.\n"
        ),
        None => "---\nname: Test Agent\n---\nYou are a test agent.\n".to_string(),
    };
    std::fs::write(fixture.path().join("agents/test.agent.md"), frontmatter)
        .expect("the agent file is writable");

    let provider = Arc::new(MockProvider::new(
        "mock",
        "mock-1",
        vec![ScriptedTurn::Text("done".to_string())],
    ));
    let shared: Arc<dyn harness_llm::Provider> = provider.clone();
    let server = start_with(fixture, Some(shared_factory(shared)), None).await;
    (server, provider)
}

/// The precedence rule, end to end: what the message asks for beats what the
/// agent declares, which beats the configured default.
#[tokio::test]
async fn a_messages_effort_outranks_the_agents_own() {
    let fixture = workspace();
    let (server, provider) = effort_recording_server(&fixture, Some("low")).await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message_with_effort("go", "max")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;

    assert_eq!(
        provider.recorded_efforts(),
        vec![Some("max".to_string())],
        "the message's level must reach the provider, not the agent's"
    );
}

#[tokio::test]
async fn the_agents_declared_effort_outranks_the_configured_default() {
    let fixture = workspace();
    let (server, provider) = effort_recording_server(&fixture, Some("low")).await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("go")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;

    assert_eq!(provider.recorded_efforts(), vec![Some("low".to_string())]);
}

#[tokio::test]
async fn the_configured_default_applies_when_nothing_else_declares_one() {
    let fixture = workspace();
    let (server, provider) = effort_recording_server(&fixture, None).await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("go")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;

    assert_eq!(
        provider.recorded_efforts(),
        vec![Some(harness_core::DEFAULT_THINKING_EFFORT.to_string())],
        "a run that names no level still gets a validated default"
    );
}

#[tokio::test]
async fn an_unsupported_effort_is_never_sent_to_the_provider() {
    let fixture = workspace();
    let (server, provider) = effort_recording_server(&fixture, Some("low")).await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message_with_effort("go", "bogus")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;

    assert_eq!(
        provider.recorded_efforts(),
        vec![Some("low".to_string())],
        "an unrecognised level must be skipped, not forwarded to the gateway"
    );
}

#[tokio::test]
async fn an_abort_interrupts_a_tool_that_is_still_running() {
    let fixture = workspace();
    let server = start_with(&fixture, Some(tool_then_text("shell", long_shell())), None).await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("go")).await;

    // Wait for the tool to be genuinely in flight: aborting before it starts
    // would test the model call, not the interruption.
    let started = Instant::now();
    let running =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "tool_call_start").await;
    assert!(
        matches!(running.message, ServerMessage::ToolCallStart { ref name, .. } if name == "shell")
    );

    send(
        &mut socket,
        ClientMessage::Abort {
            reason: Some("test".to_string()),
        },
    )
    .await;

    let done = next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;
    assert_eq!(
        done.message,
        ServerMessage::Done {
            reason: CompletionReason::Aborted
        }
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the abort took {:?}; it should beat the 30s sleep by a wide margin",
        started.elapsed()
    );
}

#[tokio::test]
async fn steering_reaches_the_running_loop_and_is_persisted() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(tool_then_text("shell", json!({ "command": "sleep 2" }))),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("go")).await;

    let running =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "tool_call_start").await;
    let session_id = running.session_id;

    send(&mut socket, steer("prefer the short path")).await;

    let mut refusals = Vec::new();
    loop {
        let envelope = next_envelope(&mut socket).await.expect("the run finished");
        if let Some(code) = error_code(&envelope) {
            refusals.push(code.to_string());
        }
        if kind(&envelope) == "done" {
            break;
        }
    }
    assert!(
        refusals.is_empty(),
        "the steering was refused: {refusals:?}"
    );

    let history = wait_for_history(&server.state, session_id, "the steering turn", |history| {
        history.iter().any(|message| {
            message
                .text()
                .contains("[steering/normal] prefer the short path")
        })
    })
    .await;

    let steering = history
        .iter()
        .position(|message| message.text().contains("[steering/normal]"))
        .expect("the steering turn is in the history");
    assert!(
        steering > 0,
        "the steering must be folded in after the run started: {history:?}"
    );
}

#[tokio::test]
async fn a_second_user_message_while_a_run_is_in_flight_is_queued_and_runs_after() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(scripting_factory(vec![
            ScriptedTurn::ToolCall {
                name: "shell".to_string(),
                arguments: json!({ "command": "sleep 2" }),
            },
            ScriptedTurn::Text("first".to_string()),
            ScriptedTurn::Text("second".to_string()),
        ])),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("first")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "tool_call_start").await;

    // Same connection, run still in flight: the message is accepted into the
    // queue rather than refused.
    send(&mut socket, user_message("second")).await;
    let queued =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "message_queued").await;
    assert!(matches!(
        queued.message,
        ServerMessage::MessageQueued { ref text, position }
            if text == "second" && position == 1
    ));

    // Both runs finish, in the order the messages were sent: the assistant
    // text proves which scripted turn ran, and the two `done`s prove the queue
    // was drained one run at a time.
    let mut chunks: Vec<String> = Vec::new();
    let mut dones: Vec<CompletionReason> = Vec::new();
    while dones.len() < 2 {
        let envelope = next_envelope(&mut socket)
            .await
            .expect("the server closed before both runs finished");
        match &envelope.message {
            ServerMessage::AssistantChunk { text } => chunks.push(text.clone()),
            ServerMessage::Done { reason } => dones.push(*reason),
            _ => {}
        }
    }

    assert_eq!(chunks, vec!["first".to_string(), "second".to_string()]);
    assert_eq!(
        dones,
        vec![CompletionReason::EndTurn, CompletionReason::EndTurn]
    );
}

#[tokio::test]
async fn an_abort_cancels_the_run_but_keeps_the_queue() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(scripting_factory(vec![
            ScriptedTurn::ToolCall {
                name: "shell".to_string(),
                arguments: json!({ "command": "sleep 30" }),
            },
            ScriptedTurn::Text("queued-ok".to_string()),
        ])),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("first")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "tool_call_start").await;

    send(&mut socket, user_message("second")).await;
    let queued =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "message_queued").await;
    assert!(matches!(
        queued.message,
        ServerMessage::MessageQueued { position: 1, .. }
    ));

    send(
        &mut socket,
        ClientMessage::Abort {
            reason: Some("test".to_string()),
        },
    )
    .await;

    let aborted = next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;
    assert!(matches!(
        aborted.message,
        ServerMessage::Done {
            reason: CompletionReason::Aborted
        }
    ));

    // The queued message survived the abort and ran next.
    let mut chunks: Vec<String> = Vec::new();
    loop {
        let envelope = next_envelope(&mut socket)
            .await
            .expect("the queued run never finished");
        if let ServerMessage::AssistantChunk { text } = &envelope.message {
            chunks.push(text.clone());
        }
        if kind(&envelope) == "done" {
            break;
        }
    }
    assert_eq!(chunks, vec!["queued-ok".to_string()]);
}

#[tokio::test]
async fn the_queue_refuses_more_than_its_limit_with_a_clear_error() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(tool_then_text("shell", json!({ "command": "sleep 30" }))),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("run")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "tool_call_start").await;

    let limit = harness_server::state::MESSAGE_QUEUE_LIMIT;
    for index in 0..limit {
        send(&mut socket, user_message(&format!("queued {index}"))).await;
        let ack =
            next_envelope_matching(&mut socket, |envelope| kind(envelope) == "message_queued")
                .await;
        assert!(
            matches!(ack.message, ServerMessage::MessageQueued { position, .. } if position == index + 1),
            "queue positions should count up from 1"
        );
    }

    send(&mut socket, user_message("one too many")).await;
    let refusal =
        next_envelope_matching(&mut socket, |envelope| error_code(envelope).is_some()).await;
    assert_eq!(error_code(&refusal), Some("queue_full"));
    let ServerMessage::Error { message, .. } = &refusal.message else {
        unreachable!("the matcher selected an error frame");
    };
    assert!(
        message.contains(&limit.to_string()),
        "the refusal should name the limit: {message}"
    );
}

/// One connection can address a session another connection opened, and two
/// sessions run at the same time on their own histories.
#[tokio::test]
async fn two_sessions_run_concurrently_and_one_connection_can_drive_both() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(scripting_factory(vec![
            ScriptedTurn::ToolCall {
                name: "shell".to_string(),
                arguments: json!({ "command": "sleep 30" }),
            },
            ScriptedTurn::Text("second-session".to_string()),
        ])),
        None,
    )
    .await;

    let mut first = connect(&server).await;
    send(&mut first, user_message("first session")).await;
    let a_running =
        next_envelope_matching(&mut first, |envelope| kind(envelope) == "tool_call_start").await;
    let session_a = a_running.session_id;

    let mut second = connect(&server).await;
    send(&mut second, user_message("second session")).await;
    let b_running =
        next_envelope_matching(&mut second, |envelope| kind(envelope) == "tool_call_start").await;
    let session_b = b_running.session_id;
    assert_ne!(session_a, session_b);

    // Both are mid-run: each has produced a tool event and neither has a `done`,
    // so the two sessions are in flight at the same time.

    // The first connection addresses the second session: the message is queued
    // on it, and the acknowledgement carries that session's routing context.
    send_to(&mut first, session_b, user_message("queued for B")).await;
    let queued =
        next_envelope_matching(&mut first, |envelope| kind(envelope) == "message_queued").await;
    assert_eq!(queued.session_id, session_b);
    assert!(matches!(
        queued.message,
        ServerMessage::MessageQueued { position: 1, .. }
    ));

    // Abort B's running turn through the first connection; B's queued message
    // then runs to completion while A is still in its 30s sleep.
    send_to(
        &mut first,
        session_b,
        ClientMessage::Abort {
            reason: Some("test".to_string()),
        },
    )
    .await;

    let mut chunks: Vec<String> = Vec::new();
    let mut dones: Vec<CompletionReason> = Vec::new();
    while dones.len() < 2 {
        let envelope = next_envelope(&mut first)
            .await
            .expect("session B never finished its queued run");
        if envelope.session_id != session_b {
            continue;
        }
        if let ServerMessage::AssistantChunk { text } = &envelope.message {
            chunks.push(text.clone());
        }
        if let ServerMessage::Done { reason } = &envelope.message {
            dones.push(*reason);
        }
    }
    assert_eq!(
        dones,
        vec![CompletionReason::Aborted, CompletionReason::EndTurn]
    );
    assert_eq!(chunks.concat(), "second-session");

    // Both sockets watch session B, and A is still running.
    let record_b = server
        .state
        .session(session_b)
        .expect("session B is registered");
    assert_eq!(record_b.subscriber_count(), 2);
    let record_a = server
        .state
        .session(session_a)
        .expect("session A is registered");
    assert!(record_a.is_running(), "session A is still mid-run");

    // Abort A so the test does not leave a 30s tool running: the harness would
    // otherwise wait for it at shutdown.
    send(
        &mut first,
        ClientMessage::Abort {
            reason: Some("cleanup".to_string()),
        },
    )
    .await;
    let a_done = next_envelope_matching(&mut first, |envelope| {
        envelope.session_id == session_a && kind(envelope) == "done"
    })
    .await;
    assert!(matches!(
        a_done.message,
        ServerMessage::Done {
            reason: CompletionReason::Aborted
        }
    ));
}

/// Each session's events reach its own subscribers and no one else's.
#[tokio::test]
async fn a_connection_sees_only_the_sessions_it_is_attached_to() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(tool_then_text("shell", json!({ "command": "sleep 2" }))),
        None,
    )
    .await;

    let mut first = connect(&server).await;
    send(&mut first, user_message("first")).await;
    let first_started =
        next_envelope_matching(&mut first, |envelope| kind(envelope) == "session_started").await;
    let session_a = first_started.session_id;

    let mut second = connect(&server).await;
    send(&mut second, user_message("second")).await;
    let second_started =
        next_envelope_matching(&mut second, |envelope| kind(envelope) == "session_started").await;
    let session_b = second_started.session_id;
    assert_ne!(session_a, session_b);

    // Drain each socket to its own `done`; every frame it reads must belong to
    // the session it opened, even though both sessions ran concurrently.
    for (socket, session_id) in [(&mut first, session_a), (&mut second, session_b)] {
        loop {
            let envelope = next_envelope(socket)
                .await
                .expect("the server closed before the run finished");
            assert_eq!(
                envelope.session_id, session_id,
                "a frame for another session leaked onto this connection"
            );
            if kind(&envelope) == "done" {
                break;
            }
        }
    }
}

#[tokio::test]
async fn control_messages_after_the_run_finished_report_not_running() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("hello")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;

    send(&mut socket, steer("too late")).await;
    let refusal = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(error_code(&refusal), Some("not_running"));

    send(&mut socket, ClientMessage::Abort { reason: None }).await;
    let refusal = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(error_code(&refusal), Some("not_running"));
}

#[tokio::test]
async fn a_client_ping_is_answered_with_pong() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    send(&mut socket, ClientMessage::Ping).await;

    let pong = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(pong.message, ServerMessage::Pong);
    // No session is running, so the reply is attributed to the server itself.
    assert_eq!(pong.agent_id.as_str(), "server");
}

#[tokio::test]
async fn a_subscriber_can_join_a_session_another_connection_started() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(tool_then_text("shell", json!({ "command": "sleep 2" }))),
        None,
    )
    .await;

    let mut first = connect(&server).await;
    send(&mut first, user_message("go")).await;
    let running =
        next_envelope_matching(&mut first, |envelope| kind(envelope) == "tool_call_start").await;

    let mut second = connect(&server).await;
    send(
        &mut second,
        ClientMessage::Subscribe {
            session_id: running.session_id,
        },
    )
    .await;

    // The subscriber sees the rest of the run, including its end.
    let done = next_envelope_matching(&mut second, |envelope| kind(envelope) == "done").await;
    assert_eq!(done.session_id, running.session_id);
    assert!(matches!(
        done.message,
        ServerMessage::Done {
            reason: CompletionReason::EndTurn
        }
    ));

    // Both connections are attached to the same session.
    let record = server
        .state
        .session(running.session_id)
        .expect("the session is registered");
    assert_eq!(record.subscriber_count(), 2);
}

#[tokio::test]
async fn an_unknown_subscription_is_refused() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    send(
        &mut socket,
        ClientMessage::Subscribe {
            session_id: SessionId::new(),
        },
    )
    .await;

    let refusal = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(error_code(&refusal), Some("unknown_session"));
}

#[tokio::test]
async fn garbage_is_refused_without_closing_the_connection() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    socket
        .send(WsMessage::text("{\"this is\":\"not a client message\"}"))
        .await
        .expect("the frame is written");

    let refusal = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(error_code(&refusal), Some("bad_message"));

    // The connection is still usable.
    send(&mut socket, ClientMessage::Ping).await;
    let pong = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(pong.message, ServerMessage::Pong);
}

/// A client that stops reading is disconnected rather than allowed to grow the
/// server's memory.
///
/// The policy is also unit-tested in `harness_server::backpressure`; this test
/// exists to prove the wiring, which is why it needs a run big enough to
/// actually fill the socket buffers — a few small frames would sit comfortably
/// in the kernel's buffers and the policy would never be reached.
#[tokio::test]
async fn a_client_that_stops_reading_is_disconnected() {
    let fixture = workspace();
    let policy = BackpressurePolicy {
        capacity: 1,
        max_consecutive_full: 2,
        wait: Duration::from_millis(10),
    };
    // One enormous turn: tens of thousands of deltas, far more than the socket
    // buffers can absorb while nobody is reading them.
    let factory = scripting_factory(vec![ScriptedTurn::Text("x".repeat(500_000))]);
    let server = start_with(&fixture, Some(factory), Some(policy)).await;

    let mut socket = connect(&server).await;
    send(&mut socket, user_message("flood")).await;

    let started =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "session_started").await;
    assert!(matches!(
        started.message,
        ServerMessage::SessionStarted { .. }
    ));

    // Read nothing for a while, so the server's writes have nowhere to go.
    tokio::time::sleep(Duration::from_millis(750)).await;

    assert!(
        read_until_closed(&mut socket, Duration::from_secs(20)).await,
        "the server kept streaming to a client that stopped reading"
    );

    let record = server
        .state
        .session(started.session_id)
        .expect("the session is registered");
    wait_until("the slow subscriber to be detached", || {
        record.subscriber_count() == 0
    })
    .await;
}

/// The counterpart of the test above: with a policy that never gives up, the
/// same flood leaves the connection open. The pair is what shows the disconnect
/// is the policy's doing rather than something that happens on its own.
#[tokio::test]
async fn a_client_that_stops_reading_survives_a_policy_that_never_gives_up() {
    let fixture = workspace();
    let policy = BackpressurePolicy {
        capacity: 4096,
        max_consecutive_full: u32::MAX,
        wait: Duration::from_millis(10),
    };
    let factory = scripting_factory(vec![ScriptedTurn::Text("x".repeat(500_000))]);
    let server = start_with(&fixture, Some(factory), Some(policy)).await;

    let mut socket = connect(&server).await;
    send(&mut socket, user_message("flood")).await;
    let started =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "session_started").await;

    tokio::time::sleep(Duration::from_millis(750)).await;

    assert!(
        !read_until_closed(&mut socket, Duration::from_secs(3)).await,
        "the connection closed even though the policy disconnects nobody"
    );

    let record = server
        .state
        .session(started.session_id)
        .expect("the session is registered");
    assert_eq!(
        record.subscriber_count(),
        1,
        "the subscriber is still attached"
    );
}

/// Drains a socket until the server closes it, returning `false` if it never does.
async fn read_until_closed(socket: &mut Socket, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), socket.next()).await {
            Ok(Some(Ok(WsMessage::Close(_)))) | Ok(None) => return true,
            Ok(Some(Ok(_))) => continue,
            // An abrupt reset is still the server letting go of this subscriber.
            Ok(Some(Err(_))) => return true,
            Err(_) => continue,
        }
    }
    false
}

/// A guardrail that refuses a call is reported on the wire, and the refused call
/// is never executed.
///
/// The scripted provider asks for the one tool call the default policy denies,
/// so this is the blocking half of the pipeline end to end: the model's request,
/// the refusal, and the frame the client sees.
#[tokio::test]
async fn a_blocked_tool_call_is_reported_on_the_bus() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(tool_then_text("shell", json!({ "command": "rm -rf /" }))),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("clean the disk")).await;

    let frame = next_envelope_matching(&mut socket, |envelope| kind(envelope) == "guardrail").await;
    match frame.message {
        ServerMessage::Guardrail {
            name,
            blocked,
            detail,
        } => {
            assert_eq!(name, "tool_policy");
            assert!(blocked, "the destructive call should be blocked: {detail}");
        }
        other => panic!("expected a guardrail frame, got {other:?}"),
    }

    // The refusal is a `ToolCallEnd` marked rejected rather than a tool run, and
    // the run still reaches its own end.
    let rejected = next_envelope_matching(&mut socket, |envelope| {
        matches!(
            envelope.message,
            ServerMessage::ToolCallEnd {
                status: harness_core::ToolCallStatus::Rejected,
                ..
            }
        )
    })
    .await;
    assert!(matches!(
        rejected.message,
        ServerMessage::ToolCallEnd { ref name, .. } if name == "shell"
    ));

    let done = next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;
    assert!(matches!(
        done.message,
        ServerMessage::Done {
            reason: CompletionReason::EndTurn
        }
    ));
}

/// A secret-shaped string in a tool result is caught before it joins the
/// conversation, and the redaction is reported on the wire.
///
/// The secret scanner rewrites rather than blocks, so the frame carries
/// `blocked: false`; what matters is that the key never reaches the model's
/// history, which the persisted conversation proves.
#[tokio::test]
async fn a_secret_in_a_tool_result_is_redacted_before_it_enters_history() {
    let fixture = workspace();
    std::fs::write(
        fixture.path().join("note.txt"),
        "AWS_SECRET_ACCESS_KEY=AKIAIOSFODNN7EXAMPLE\n",
    )
    .expect("the note is writable");
    let server = start_with(
        &fixture,
        Some(tool_then_text("read_file", json!({ "path": "note.txt" }))),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("read note.txt")).await;

    let frame = next_envelope_matching(&mut socket, |envelope| kind(envelope) == "guardrail").await;
    let session_id = frame.session_id;
    match frame.message {
        ServerMessage::Guardrail {
            name,
            blocked,
            detail,
        } => {
            assert_eq!(name, "secret_scanner");
            assert!(
                !blocked,
                "the secret scanner redacts rather than blocks: {detail}"
            );
        }
        other => panic!("expected a guardrail frame, got {other:?}"),
    }

    let done = next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;
    assert!(matches!(
        done.message,
        ServerMessage::Done {
            reason: CompletionReason::EndTurn
        }
    ));

    let history = wait_for_history(
        &server.state,
        session_id,
        "the redacted tool result",
        |history| history.iter().any(|message| message.role == Role::Tool),
    )
    .await;
    let tool = history
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("the read produced a tool result");
    assert!(
        !tool.text().contains("AKIAIOSFODNN7EXAMPLE"),
        "the key reached the history: {}",
        tool.text()
    );
    assert!(
        tool.text().contains("[redacted:aws_key]"),
        "the key was not replaced by its placeholder: {}",
        tool.text()
    );
}
