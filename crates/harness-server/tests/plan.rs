//! End-to-end tests of `plan_task` over the bus.
//!
//! The orchestrator is reached through a real socket, because what is worth
//! testing is the wiring: the planning call, the sub-agents the executor runs,
//! and the single `done` the client sees at the end of the DAG.

mod support;

use std::time::{Duration, Instant};

use harness_core::{AgentId, ClientMessage, CompletionReason, ServerMessage, SubtaskStatus};
use harness_llm::ScriptedTurn;
use serde_json::json;

use support::{
    connect, error_code, kind, next_envelope, next_envelope_matching, scripting_factory, send,
    start, start_with, tool_then_text, user_message, workspace, Fixture,
};

/// The plan the model reads first: one node, so the script's turns are consumed
/// in a deterministic order — the planner, then that node.
fn one_node_plan(agent: Option<&str>) -> String {
    json!({
        "nodes": [{
            "id": "read-manifest",
            "objective": "read the manifest and report what it declares",
            "agent": agent,
            "files": ["Cargo.toml"],
        }]
    })
    .to_string()
}

/// The four-field result the executor requires of every sub-agent.
fn subtask_result() -> String {
    json!({
        "objective": "read the manifest and report what it declares",
        "state": "done",
        "evidence": "read Cargo.toml, which declares a package named fixture",
        "boundary": "nothing else was touched",
    })
    .to_string()
}

/// A provider scripted with a plan and the node's answer to it.
fn planning_factory(plan: String) -> harness_server::ProviderFactory {
    scripting_factory(vec![
        ScriptedTurn::Text(plan),
        ScriptedTurn::Text(subtask_result()),
    ])
}

fn plan_task(task: &str) -> ClientMessage {
    ClientMessage::PlanTask {
        task: task.to_string(),
        agent_id: None,
    }
}

#[tokio::test]
async fn a_plan_task_streams_subtask_events_and_a_single_done() {
    let fixture = workspace();
    let server = start_with(&fixture, Some(planning_factory(one_node_plan(None))), None).await;
    let mut socket = connect(&server).await;

    send(&mut socket, plan_task("read the manifest")).await;

    let mut kinds: Vec<String> = Vec::new();
    let mut subtask_status = None;
    let (session_id, done) = loop {
        let envelope = next_envelope(&mut socket)
            .await
            .expect("the server closed before the plan finished");

        assert_eq!(envelope.v, 1, "every envelope declares protocol version 1");
        assert_eq!(
            envelope.agent_id.as_str(),
            "test",
            "the fixture agent is used"
        );

        if let ServerMessage::SubtaskEnd { status, .. } = &envelope.message {
            subtask_status = Some(*status);
        }
        kinds.push(kind(&envelope));

        if let ServerMessage::Done { reason } = envelope.message {
            break (envelope.session_id, reason);
        }
    };

    assert_eq!(kinds.first().map(String::as_str), Some("session_started"));
    assert!(kinds.contains(&"subtask_start".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"subtask_end".to_string()), "{kinds:?}");
    // The sub-agents emit a `Done` each; only the plan's own may reach the client.
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "done").count(),
        1,
        "exactly one done closes the stream: {kinds:?}"
    );
    assert_eq!(kinds.last().map(String::as_str), Some("done"));

    let started = kinds
        .iter()
        .position(|kind| kind == "subtask_start")
        .expect("a subtask started");
    let ended = kinds
        .iter()
        .position(|kind| kind == "subtask_end")
        .expect("a subtask ended");
    assert!(
        started < ended,
        "subtask events arrived out of order: {kinds:?}"
    );

    assert_eq!(subtask_status, Some(SubtaskStatus::Succeeded));
    assert_eq!(done, CompletionReason::EndTurn);
    assert!(
        server.state.session(session_id).is_some(),
        "the plan's session is registered"
    );
}

#[tokio::test]
async fn a_plan_task_while_a_run_is_in_flight_is_refused() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(tool_then_text("shell", json!({ "command": "sleep 2" }))),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, user_message("first")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "tool_call_start").await;

    // Same connection, run still in flight: the plan is refused, not queued.
    send(&mut socket, plan_task("plan something else")).await;

    let refusal =
        next_envelope_matching(&mut socket, |envelope| error_code(envelope).is_some()).await;
    assert_eq!(error_code(&refusal), Some("busy"));

    // The run in flight is untouched by the refused plan.
    let done = next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;
    assert!(matches!(
        done.message,
        ServerMessage::Done {
            reason: CompletionReason::EndTurn
        }
    ));
}

/// An abort reaches the executor and stops a sub-agent that is still working.
#[tokio::test]
async fn an_abort_stops_a_plan_and_is_reported_as_aborted() {
    let fixture = workspace();
    let server = start_with(
        &fixture,
        Some(scripting_factory(vec![
            ScriptedTurn::Text(one_node_plan(None)),
            // A shell that outlives any abort by orders of magnitude, so the plan
            // was certainly interrupted rather than merely fast.
            ScriptedTurn::ToolCall {
                name: "shell".to_string(),
                arguments: json!({ "command": "sleep 30" }),
            },
        ])),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, plan_task("read the manifest")).await;

    // Wait for the sub-agent's tool to be genuinely in flight: aborting before
    // it starts would test the model call, not the interruption.
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
async fn a_plan_task_for_an_unknown_agent_is_refused_before_a_session_is_created() {
    let fixture = workspace();
    let server = start(&fixture).await;
    let mut socket = connect(&server).await;

    send(
        &mut socket,
        ClientMessage::PlanTask {
            task: "read the manifest".to_string(),
            agent_id: Some(AgentId::new("nobody").expect("a valid id")),
        },
    )
    .await;

    let refusal = next_envelope(&mut socket).await.expect("an answer");
    assert_eq!(error_code(&refusal), Some("agent_not_found"));
    assert_eq!(server.state.session_count(), 0, "no session was registered");
}

/// A node's run is a session of its own, linked to the plan's session, and the
/// frame that opens the worker's lane names it so a client can open it.
#[tokio::test]
async fn a_subagent_frame_names_the_workers_session_and_that_session_replays() {
    let fixture = workspace();
    let server = start_with(&fixture, Some(planning_factory(one_node_plan(None))), None).await;
    let mut socket = connect(&server).await;

    send(&mut socket, plan_task("read the manifest")).await;

    let start =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "subtask_start").await;
    assert_eq!(
        start.subagent_id.as_ref().map(AgentId::as_str),
        Some("test"),
        "the frame names the worker's agent"
    );
    let worker_session = start
        .subagent_session_id
        .expect("the worker's frame names the session its turns are stored under");
    assert_ne!(
        worker_session, start.session_id,
        "the worker's session is not the plan's session"
    );

    // Let the plan finish before reading the store it wrote to.
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;

    // The id the frame named opens the worker's own conversation.
    let history = support::wait_for_history(
        &server.state,
        worker_session,
        "the worker's stored turns",
        |history| !history.is_empty(),
    )
    .await;
    assert!(
        history
            .iter()
            .any(|message| message.text().contains("read the manifest")),
        "the worker's session must hold its own prompt: {history:?}"
    );

    let sessions = server
        .state
        .memory()
        .sessions()
        .await
        .expect("the session list");
    let child = sessions
        .iter()
        .find(|session| session.id == worker_session)
        .expect("the worker's session is listed");
    assert_eq!(
        child.forked_from_session,
        Some(start.session_id),
        "the worker's session must point at the plan's session"
    );
    assert_eq!(
        child.label.as_deref(),
        Some("read-manifest"),
        "the child is labelled with its node"
    );

    // The parent stays the main conversation: none of the worker's turns is in it.
    let parent_history = server
        .state
        .memory()
        .history(start.session_id, None)
        .await
        .expect("the parent is readable");
    assert!(
        parent_history.is_empty(),
        "the parent must hold no worker turns: {parent_history:?}"
    );
}

/// A workspace with an agent literally named `default`, which is the entry agent
/// the executor names in a handoff.
fn workspace_with_a_default_agent() -> Fixture {
    let fixture = workspace();
    std::fs::write(
        fixture.path().join("agents/default.agent.md"),
        "---\nname: Default Agent\ndescription: The entry agent.\n---\nYou are the entry agent.\n",
    )
    .expect("the agent file is writable");
    fixture
}

#[tokio::test]
async fn a_node_run_by_another_agent_reaches_the_client_as_a_handoff() {
    let fixture = workspace_with_a_default_agent();
    let server = start_with(
        &fixture,
        Some(planning_factory(one_node_plan(Some("test")))),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, plan_task("read the manifest")).await;

    let handoff =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "agent_handoff").await;
    assert!(matches!(
        handoff.message,
        ServerMessage::AgentHandoff { ref from, ref to, .. }
            if from.as_str() == "default" && to.as_str() == "test"
    ));

    let done = next_envelope_matching(&mut socket, |envelope| kind(envelope) == "done").await;
    assert!(matches!(
        done.message,
        ServerMessage::Done {
            reason: CompletionReason::EndTurn
        }
    ));
}
