//! Stage boundaries: a mid-run snapshot is a real fork point, not just the end.

use harness_core::{Memory, Message, NodeKind};
use harness_memory::{
    fork_from_stage, is_stage_label, node_stage, pin_stage, stage_label, stages, SqliteMemory,
};

#[tokio::test]
async fn stage_labels_are_recognisable_and_name_their_node() {
    assert_eq!(stage_label("plan"), "stage:plan");
    assert!(is_stage_label(&stage_label("plan")));
    // A branch's fork marker is labelled with a node id, not a stage.
    assert!(!is_stage_label("01H-node-id"));

    let label = node_stage("node-7", "start");
    assert_eq!(label, "node:node-7:start");
    assert!(is_stage_label(&stage_label(&label)));
}

#[tokio::test]
async fn a_branch_from_a_mid_run_stage_sees_the_stage_state_not_the_end() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("run")).await.unwrap();

    memory
        .append(session, &Message::user("stage one output"))
        .await
        .unwrap();
    let first = pin_stage(&memory, session, &node_stage("node-7", "start"))
        .await
        .unwrap();

    memory
        .append(session, &Message::assistant("stage two output"))
        .await
        .unwrap();
    let second = pin_stage(&memory, session, &node_stage("node-7", "end"))
        .await
        .unwrap();

    // The run continues past both boundaries; the final turn must not be visible
    // from either stage.
    memory
        .append(session, &Message::user("the final turn"))
        .await
        .unwrap();

    assert_eq!(first.label, "stage:node:node-7:start");
    assert_eq!(second.label, "stage:node:node-7:end");

    let listed = stages(&memory, session).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|marker| &marker.label)
            .collect::<Vec<_>>(),
        vec!["stage:node:node-7:start", "stage:node:node-7:end",],
        "the session's stages come back in the order they were pinned"
    );

    let from_start = first.fork(&memory, Some("redo")).await.unwrap();
    let start_history = memory.history(from_start, None).await.unwrap();
    assert_eq!(
        start_history
            .iter()
            .map(|message| message.text())
            .collect::<Vec<_>>(),
        vec!["stage one output"],
        "a branch from the start stage must not see later stages"
    );

    let from_end = fork_from_stage(&memory, &second, Some("continue"))
        .await
        .unwrap();
    let end_history = memory.history(from_end, None).await.unwrap();
    assert_eq!(
        end_history
            .iter()
            .map(|message| message.text())
            .collect::<Vec<_>>(),
        vec!["stage one output", "stage two output"],
        "a branch from the end stage sees both stages but not the final turn"
    );

    // The source run is untouched by either fork.
    let source = memory.history(session, None).await.unwrap();
    assert_eq!(source.len(), 3);
    assert_eq!(source[2].text(), "the final turn");
}

#[tokio::test]
async fn a_branch_reports_no_stages_of_its_own() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("origin")).await.unwrap();
    memory
        .append(session, &Message::user("before the fork"))
        .await
        .unwrap();
    let stage = pin_stage(&memory, session, "checkpoint").await.unwrap();

    let branch = stage.fork(&memory, Some("branch")).await.unwrap();

    assert!(
        stages(&memory, branch).await.unwrap().is_empty(),
        "the fork marker is not a stage; the origin's stages are not the branch's"
    );
    assert_eq!(stages(&memory, session).await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_empty_session_has_no_stages() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("empty")).await.unwrap();
    assert!(stages(&memory, session).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_stage_marker_is_a_snapshot_node() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("kinds")).await.unwrap();
    memory
        .append(session, &Message::user("work"))
        .await
        .unwrap();

    let marker = pin_stage(&memory, session, "checkpoint").await.unwrap();
    let node = memory.node(marker.node_id).await.unwrap().unwrap();

    assert_eq!(node.kind, NodeKind::Snapshot);
    assert_eq!(node.session_id, session);
    assert_eq!(node.label.as_deref(), Some("stage:checkpoint"));
    // The marker is not a conversational turn, so history is unchanged.
    assert_eq!(memory.history(session, None).await.unwrap().len(), 1);
}
