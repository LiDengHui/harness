//! End-to-end tests of the plan frame and the workflow queue over the bus.
//!
//! What is worth testing here only exists once a real socket is between the
//! client and the server: the plan frame a client draws a checklist from, the
//! per-node status frames that fill those rows in, and the workflow queue that
//! runs one job at a time while staying separate from the message queue.

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use harness_core::{
    ClientMessage, CompletionReason, Result as HarnessResult, ServerMessage, SubtaskStatus,
};
use harness_llm::ScriptedTurn;
use harness_orchestrator::{SelectedWorkflow, TaskGraph, TaskNode, WorkflowSelector};
use serde_json::json;

use support::{
    connect, kind, next_envelope, next_envelope_matching, scripting_factory, send,
    start_with_selector, user_message, workspace,
};

/// The four-field result the executor requires of every sub-agent. The objective
/// is deliberately generic: nodes in one layer run concurrently, so which script
/// turn serves which node is not deterministic, and the parse does not require
/// the objective to match the node's.
fn subtask_result() -> String {
    json!({
        "objective": "do the thing",
        "state": "done",
        "evidence": "ran the work",
        "boundary": "nothing else",
    })
    .to_string()
}

fn plan_of(nodes: serde_json::Value) -> String {
    json!({ "nodes": nodes }).to_string()
}

fn queue_workflow(task: &str) -> ClientMessage {
    ClientMessage::QueueWorkflow {
        task: task.to_string(),
        workflow: None,
        agent_id: None,
    }
}

/// A selector that always answers with the same workflow, so a test can tell the
/// selector path apart from the free-form planner: the planner is never called.
struct StubSelector {
    workflow: Option<SelectedWorkflow>,
}

#[async_trait]
impl WorkflowSelector for StubSelector {
    async fn select(
        &self,
        _task: &str,
        _provider: &dyn harness_llm::Provider,
        _model: &str,
        _requested: Option<&str>,
    ) -> HarnessResult<Option<SelectedWorkflow>> {
        Ok(self.workflow.clone())
    }
}

fn stub_graph() -> TaskGraph {
    TaskGraph {
        nodes: vec![
            TaskNode {
                id: "implement".into(),
                objective: "implement the change".into(),
                agent: None,
                depends_on: Vec::new(),
                files: Vec::new(),
                verify: Vec::new(),
            },
            TaskNode {
                id: "document".into(),
                objective: "document the change".into(),
                agent: None,
                depends_on: vec!["implement".into()],
                files: Vec::new(),
                verify: Vec::new(),
            },
        ],
        guidance: None,
    }
}

/// The plan a client receives carries every node, and every node's status frame
/// names the row it belongs to.
#[tokio::test]
async fn the_plan_reaches_the_client_with_every_node() {
    let fixture = workspace();
    let plan = plan_of(json!([
        { "id": "a", "objective": "do a" },
        { "id": "b", "objective": "do b", "depends_on": ["a"] },
        { "id": "c", "objective": "do c", "depends_on": ["a"] },
    ]));
    let server = support::start_with(
        &fixture,
        Some(scripting_factory(vec![
            ScriptedTurn::Text(plan),
            ScriptedTurn::Text(subtask_result()),
            ScriptedTurn::Text(subtask_result()),
            ScriptedTurn::Text(subtask_result()),
        ])),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, queue_workflow("build the thing")).await;

    let plan_frame =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "plan_created").await;
    match plan_frame.message {
        ServerMessage::PlanCreated { nodes, workflow_id } => {
            let ids: Vec<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
            assert_eq!(ids, vec!["a", "b", "c"], "every node must be listed");
            assert_eq!(nodes[0].objective, "do a");
            assert!(nodes[0].depends_on.is_empty());
            assert_eq!(nodes[1].depends_on, vec!["a".to_string()]);
            assert_eq!(nodes[2].depends_on, vec!["a".to_string()]);
            assert!(workflow_id.is_none(), "a free-form plan names no workflow");
        }
        other => panic!("expected a plan frame, got {other:?}"),
    }

    // Every status frame names the graph node it belongs to, and every node
    // reaches exactly one terminal state.
    let mut seen_nodes: Vec<String> = Vec::new();
    let mut terminals: Vec<(String, SubtaskStatus)> = Vec::new();
    loop {
        let envelope = next_envelope(&mut socket)
            .await
            .expect("the server closed before the plan finished");
        match &envelope.message {
            ServerMessage::SubtaskStart { node_id, .. } => seen_nodes.push(node_id.clone()),
            ServerMessage::SubtaskEnd {
                node_id, status, ..
            } => terminals.push((node_id.clone(), *status)),
            ServerMessage::Done { reason } => {
                assert_eq!(*reason, CompletionReason::EndTurn);
                break;
            }
            _ => {}
        }
    }

    seen_nodes.sort();
    assert_eq!(seen_nodes, vec!["a", "b", "c"]);
    assert_eq!(
        terminals.len(),
        3,
        "one terminal frame per node: {terminals:?}"
    );
    for node in ["a", "b", "c"] {
        assert!(
            terminals
                .iter()
                .any(|(id, status)| id == node && *status == SubtaskStatus::Succeeded),
            "node `{node}` has no succeeded frame: {terminals:?}"
        );
    }
}

/// A node that never runs still ends as `skipped`, so its row does not stay
/// pending forever.
#[tokio::test]
async fn a_node_that_never_runs_ends_as_skipped() {
    let fixture = workspace();
    let plan = plan_of(json!([
        { "id": "root", "objective": "do the root" },
        { "id": "left", "objective": "do left", "depends_on": ["root"] },
        { "id": "right", "objective": "do right", "depends_on": ["root"] },
    ]));
    // `root` never returns a four-field result, so it fails after its one retry
    // and its dependents are never scheduled.
    let server = support::start_with(
        &fixture,
        Some(scripting_factory(vec![
            ScriptedTurn::Text(plan),
            ScriptedTurn::Text("I could not finish this one.".to_string()),
            ScriptedTurn::Text("I could not finish this one.".to_string()),
        ])),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, queue_workflow("build the thing")).await;

    let mut terminals: Vec<(String, SubtaskStatus)> = Vec::new();
    loop {
        let envelope = next_envelope(&mut socket)
            .await
            .expect("the server closed before the plan finished");
        if let ServerMessage::SubtaskEnd {
            node_id, status, ..
        } = &envelope.message
        {
            terminals.push((node_id.clone(), *status));
        }
        if kind(&envelope) == "done" {
            break;
        }
    }

    assert!(
        terminals
            .iter()
            .any(|(id, status)| id == "root" && *status == SubtaskStatus::Failed),
        "the failing root must report failed: {terminals:?}"
    );
    for node in ["left", "right"] {
        assert!(
            terminals
                .iter()
                .any(|(id, status)| id == node && *status == SubtaskStatus::Skipped),
            "node `{node}` must report skipped: {terminals:?}"
        );
    }
}

/// Two jobs queue behind each other and run in order.
#[tokio::test]
async fn two_jobs_queue_and_run_in_order() {
    let fixture = workspace();
    let first = plan_of(json!([{ "id": "only-first", "objective": "do the first" }]));
    let second = plan_of(json!([{ "id": "only-second", "objective": "do the second" }]));
    let server = support::start_with(
        &fixture,
        Some(scripting_factory(vec![
            // Job 1: planner, then a node that stays in flight long enough for
            // job 2 to be queued, then its result.
            ScriptedTurn::Text(first),
            ScriptedTurn::ToolCall {
                name: "shell".to_string(),
                arguments: json!({ "command": "sleep 2" }),
            },
            ScriptedTurn::Text(subtask_result()),
            // Job 2: planner, then its node.
            ScriptedTurn::Text(second),
            ScriptedTurn::Text(subtask_result()),
        ])),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, queue_workflow("first job")).await;
    let started =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "workflow_started").await;
    assert!(
        matches!(
            started.message,
            ServerMessage::WorkflowStarted { job_id: 0, ref task, .. } if task == "first job"
        ),
        "{:?}",
        started.message
    );

    // The first job's node is genuinely in flight before the second is sent.
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "tool_call_start").await;
    send(&mut socket, queue_workflow("second job")).await;

    let queued =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "workflow_queued").await;
    assert!(
        matches!(
            queued.message,
            ServerMessage::WorkflowQueued { job_id: 1, position: 1, ref task, .. }
                if task == "second job"
        ),
        "{:?}",
        queued.message
    );

    // The first finishes, then the second starts — in that order.
    let finished_first = next_envelope_matching(&mut socket, |envelope| {
        kind(envelope) == "workflow_finished"
    })
    .await;
    assert!(
        matches!(
            finished_first.message,
            ServerMessage::WorkflowFinished {
                job_id: 0,
                status: SubtaskStatus::Succeeded,
                ..
            }
        ),
        "{:?}",
        finished_first.message
    );

    let started_second =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "workflow_started").await;
    assert!(
        matches!(
            started_second.message,
            ServerMessage::WorkflowStarted { job_id: 1, ref task, .. } if task == "second job"
        ),
        "{:?}",
        started_second.message
    );

    let finished_second = next_envelope_matching(&mut socket, |envelope| {
        kind(envelope) == "workflow_finished"
    })
    .await;
    assert!(
        matches!(
            finished_second.message,
            ServerMessage::WorkflowFinished {
                job_id: 1,
                status: SubtaskStatus::Succeeded,
                ..
            }
        ),
        "{:?}",
        finished_second.message
    );
}

/// A job that names no workflow falls back to the free-form plan.
#[tokio::test]
async fn a_job_with_no_workflow_named_falls_back_to_a_free_form_plan() {
    let fixture = workspace();
    let server = support::start_with(
        &fixture,
        Some(scripting_factory(vec![
            ScriptedTurn::Text(plan_of(json!([
                { "id": "planned", "objective": "do the planned work" }
            ]))),
            ScriptedTurn::Text(subtask_result()),
        ])),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, queue_workflow("build the thing")).await;

    let plan =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "plan_created").await;
    assert!(
        matches!(
            plan.message,
            ServerMessage::PlanCreated { ref nodes, workflow_id: None }
                if nodes.len() == 1 && nodes[0].id == "planned"
        ),
        "{:?}",
        plan.message
    );

    let finished = next_envelope_matching(&mut socket, |envelope| {
        kind(envelope) == "workflow_finished"
    })
    .await;
    assert!(
        matches!(
            finished.message,
            ServerMessage::WorkflowFinished {
                status: SubtaskStatus::Succeeded,
                ..
            }
        ),
        "{:?}",
        finished.message
    );
}

/// With a selector installed, a job that names no workflow runs the chosen one —
/// the planner is not consulted at all.
#[tokio::test]
async fn a_job_with_no_workflow_named_runs_the_selected_workflow() {
    let fixture = workspace();
    // Only the two node results are scripted: if the planner ran, it would eat
    // the first one and fail to parse a plan.
    let factory = scripting_factory(vec![
        ScriptedTurn::Text(subtask_result()),
        ScriptedTurn::Text(subtask_result()),
    ]);
    let selector: Arc<dyn WorkflowSelector> = Arc::new(StubSelector {
        workflow: Some(SelectedWorkflow {
            id: "feature-build".into(),
            graph: stub_graph(),
        }),
    });
    let server = start_with_selector(&fixture, factory, selector).await;
    let mut socket = connect(&server).await;

    send(&mut socket, queue_workflow("ship the feature")).await;

    let plan =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "plan_created").await;
    match plan.message {
        ServerMessage::PlanCreated { nodes, workflow_id } => {
            assert_eq!(workflow_id.as_deref(), Some("feature-build"));
            let ids: Vec<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
            assert_eq!(ids, vec!["implement", "document"]);
            assert_eq!(nodes[1].depends_on, vec!["implement".to_string()]);
        }
        other => panic!("expected a plan frame, got {other:?}"),
    }

    let finished = next_envelope_matching(&mut socket, |envelope| {
        kind(envelope) == "workflow_finished"
    })
    .await;
    assert!(
        matches!(
            finished.message,
            ServerMessage::WorkflowFinished {
                status: SubtaskStatus::Succeeded,
                ..
            }
        ),
        "{:?}",
        finished.message
    );
}

/// The workflow queue and the user-message queue are separate lists: each
/// numbers itself, and neither counts the other's entries.
#[tokio::test]
async fn the_workflow_queue_and_the_message_queue_do_not_interfere() {
    let fixture = workspace();
    let plan = plan_of(json!([{ "id": "slow", "objective": "do the slow work" }]));
    let server = support::start_with(
        &fixture,
        Some(scripting_factory(vec![
            ScriptedTurn::Text(plan),
            ScriptedTurn::ToolCall {
                name: "shell".to_string(),
                arguments: json!({ "command": "sleep 2" }),
            },
        ])),
        None,
    )
    .await;
    let mut socket = connect(&server).await;

    send(&mut socket, queue_workflow("running job")).await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "workflow_started").await;
    next_envelope_matching(&mut socket, |envelope| kind(envelope) == "tool_call_start").await;

    // A message goes to the message queue...
    send(&mut socket, user_message("a waiting message")).await;
    let message_queued =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "message_queued").await;
    assert!(
        matches!(
            message_queued.message,
            ServerMessage::MessageQueued { position: 1, .. }
        ),
        "{:?}",
        message_queued.message
    );

    // ...and a job goes to the workflow queue, at its own position 1, not
    // behind the message.
    send(&mut socket, queue_workflow("waiting job")).await;
    let job_queued =
        next_envelope_matching(&mut socket, |envelope| kind(envelope) == "workflow_queued").await;
    assert!(
        matches!(
            job_queued.message,
            ServerMessage::WorkflowQueued { position: 1, ref task, .. } if task == "waiting job"
        ),
        "the workflow queue must number itself independently: {:?}",
        job_queued.message
    );

    // The connection going away clears both queues; dropping the socket is the
    // client vanishing, which is what the test is simulating.
    drop(socket);
}
