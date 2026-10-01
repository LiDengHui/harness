//! End-to-end tests for the `Memory` contract.
//!
//! The helper modules (`trim`, `vector`, `index`, `recall`) have their own unit
//! tests; these exercise the SQLite store itself — the DAG semantics that the
//! agent kernel and the orchestrator depend on.

use harness_core::{
    HeuristicEstimator, Memory, Message, NodeKind, RecallQuery, Role, SessionId, TokenEstimator,
    ToolCall, TrimOptions,
};
use harness_memory::SqliteMemory;
use serde_json::json;

/// `seeded` writes: 2 framing turns + 4 x (tool-call, tool-result, answer) + 2 closing turns.
const SEEDED_NODES: usize = 2 + 4 * 3 + 2;
const SEEDED_TOOL_TURNS: usize = 4;

/// A distinctive marker at the head of each tool body, so a snippet is checkable.
fn tool_body(index: usize, repeat: usize) -> String {
    format!(
        "unique-marker-{index} {}",
        "the quick brown fox jumps over the lazy dog. ".repeat(repeat)
    )
}

/// Builds a session with conversation turns plus four oversized tool outputs.
///
/// `repeat` scales the tool bodies, which is the knob that decides how much trim
/// can save: the savings are a function of what share of the context is
/// mechanical rather than conversational.
async fn seeded_with(memory: &SqliteMemory, repeat: usize) -> (SessionId, Vec<String>) {
    let session = memory.create_session(Some("seeded")).await.unwrap();

    memory
        .append(session, &Message::user("please inspect the logs"))
        .await
        .unwrap();
    memory
        .append(session, &Message::assistant("Inspecting them now."))
        .await
        .unwrap();

    let mut bodies = Vec::new();
    for index in 0..SEEDED_TOOL_TURNS {
        let call = ToolCall {
            id: format!("call_{index}"),
            name: "read_file".into(),
            arguments: json!({ "path": format!("big_{index}.log") }),
        };
        memory
            .append(session, &Message::assistant_tool_calls(vec![call]))
            .await
            .unwrap();

        let body = tool_body(index, repeat);
        memory
            .append(
                session,
                &Message::tool_result(format!("call_{index}"), body.clone()),
            )
            .await
            .unwrap();
        bodies.push(body);

        memory
            .append(session, &Message::assistant(format!("Read log {index}.")))
            .await
            .unwrap();
    }

    memory
        .append(session, &Message::user("now summarise"))
        .await
        .unwrap();
    memory
        .append(session, &Message::assistant("Summary follows."))
        .await
        .unwrap();

    (session, bodies)
}

/// The default seed: large tool outputs, the case trim exists for.
async fn seeded(memory: &SqliteMemory) -> (SessionId, Vec<String>) {
    seeded_with(memory, 900).await
}

fn measured_tokens(history: &[Message]) -> usize {
    let estimator = HeuristicEstimator::default();
    history
        .iter()
        .map(|message| estimator.estimate(message.text()))
        .sum()
}

#[tokio::test]
async fn appended_turns_come_back_in_order() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let (session, _) = seeded(&memory).await;

    let history = memory.history(session, None).await.unwrap();

    assert_eq!(history.len(), SEEDED_NODES);
    assert_eq!(history[0].text(), "please inspect the logs");
    assert_eq!(history[0].role, Role::User);
    assert_eq!(history[1].text(), "Inspecting them now.");
    assert_eq!(history[2].role, Role::Assistant);
    assert_eq!(history[2].tool_calls.len(), 1);
    assert_eq!(history[3].role, Role::Tool);
    assert_eq!(history[3].tool_call_id.as_deref(), Some("call_0"));
    assert_eq!(history[14].text(), "now summarise");
    assert_eq!(history.last().unwrap().text(), "Summary follows.");
}

#[tokio::test]
async fn trim_savings_are_measured_across_two_context_shapes() {
    // The documented saving is a function of how much of the context is
    // mechanical, so measure both a tool-heavy session and a chatty one rather
    // than quoting a single number.
    for (label, repeat) in [("tool-heavy", 900usize), ("mostly-conversational", 12)] {
        let memory = SqliteMemory::in_memory().await.unwrap();
        let (session, _) = seeded_with(&memory, repeat).await;

        let before = measured_tokens(&memory.history(session, None).await.unwrap());
        let report = memory
            .trim(
                session,
                TrimOptions {
                    min_tool_output_chars: 100,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let after = measured_tokens(&memory.history(session, None).await.unwrap());

        println!(
            "{label:>22}: {before:>6} -> {after:>5} tokens, saved {:>5.1}% \
             ({}/{} nodes elided)",
            report.saved_percent(),
            report.nodes_trimmed,
            report.nodes_examined,
        );

        assert_eq!(report.nodes_examined, SEEDED_NODES);
        assert_eq!(report.nodes_trimmed, SEEDED_TOOL_TURNS);
        assert!(
            report.saved_percent() > 20.0,
            "{label} saved only {:.1}%",
            report.saved_percent()
        );
        assert!(after < before);
    }
}

#[tokio::test]
async fn trim_leaves_every_conversational_turn_byte_identical() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let (session, bodies) = seeded(&memory).await;
    let before = memory.history(session, None).await.unwrap();

    let options = TrimOptions {
        min_tool_output_chars: 100,
        ..Default::default()
    };
    memory.trim(session, options).await.unwrap();
    let after = memory.history(session, None).await.unwrap();

    assert_eq!(before.len(), after.len());
    for (original, trimmed) in before.iter().zip(after.iter()) {
        if original.role.is_conversational() {
            assert_eq!(
                original.content,
                trimmed.content,
                "a {} turn was rewritten by trim",
                original.role.as_str()
            );
            assert_eq!(original.tool_calls, trimmed.tool_calls);
        }
    }

    // Nothing was actually lost: every elided body is still retrievable.
    let head = memory.head(session).await.unwrap().unwrap();
    let nodes = memory.path(head).await.unwrap();
    let elided: Vec<_> = nodes
        .iter()
        .filter(|node| node.trimmed && node.role == Some(Role::Tool))
        .collect();
    assert_eq!(elided.len(), SEEDED_TOOL_TURNS);

    for node in elided {
        let body = memory.raw_output(node.id).await.unwrap();
        assert!(
            bodies
                .iter()
                .any(|expected| Some(expected) == body.as_ref()),
            "the raw body behind {} was not recoverable",
            node.id
        );
        assert!(
            node.content.contains("elided"),
            "the reference should say what it replaced: {}",
            node.content
        );
    }
}

#[tokio::test]
async fn keep_recent_protects_the_active_context() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let (session, _) = seeded(&memory).await;

    let untouched = memory
        .trim(
            session,
            TrimOptions {
                keep_recent: 100,
                min_tool_output_chars: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(untouched.nodes_trimmed, 0);

    let everything = memory
        .trim(
            session,
            TrimOptions {
                keep_recent: 0,
                min_tool_output_chars: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(everything.nodes_trimmed, SEEDED_TOOL_TURNS);
}

#[tokio::test]
async fn short_tool_output_is_not_worth_eliding() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("small")).await.unwrap();
    memory
        .append(session, &Message::user("check the version"))
        .await
        .unwrap();
    memory
        .append(session, &Message::tool_result("call_1", "1.2.3"))
        .await
        .unwrap();

    let report = memory.trim(session, TrimOptions::default()).await.unwrap();

    assert_eq!(report.nodes_trimmed, 0);
    let history = memory.history(session, None).await.unwrap();
    assert_eq!(history[1].text(), "1.2.3");
}

#[tokio::test]
async fn a_snapshot_pins_a_stable_fork_point() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("snapshots")).await.unwrap();
    memory
        .append(session, &Message::user("first"))
        .await
        .unwrap();

    let mark = memory.snapshot(session, "before-experiment").await.unwrap();
    memory
        .append(session, &Message::user("second"))
        .await
        .unwrap();

    let node = memory.node(mark).await.unwrap().unwrap();
    assert_eq!(node.kind, NodeKind::Snapshot);
    assert_eq!(node.label.as_deref(), Some("before-experiment"));

    // The marker itself is not a conversational turn.
    assert_eq!(memory.history(session, None).await.unwrap().len(), 2);

    let forked = memory
        .branch(session, mark, Some("experiment"))
        .await
        .unwrap();
    let inherited = memory.history(forked, None).await.unwrap();
    assert_eq!(inherited.len(), 1);
    assert_eq!(inherited[0].text(), "first");
}

#[tokio::test]
async fn branching_inherits_the_chain_and_isolates_new_turns() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let (session, _) = seeded(&memory).await;
    let head = memory.head(session).await.unwrap().unwrap();
    let original = memory.history(session, None).await.unwrap();

    let forked = memory
        .branch(session, head, Some("alternative"))
        .await
        .unwrap();
    assert_ne!(forked, session);

    let inherited = memory.history(forked, None).await.unwrap();
    assert_eq!(
        inherited, original,
        "the fork reproduces the original's prefix"
    );

    let forked_head = memory.head(forked).await.unwrap().unwrap();
    assert_ne!(forked_head, head, "the fork marker is its own head");

    // The fork shows up as a child of the node it was taken at.
    let children = memory.children(head).await.unwrap();
    assert_eq!(children.len(), 1, "the fork marker is head's only child");

    memory
        .append(forked, &Message::user("try a different approach"))
        .await
        .unwrap();

    assert_eq!(
        memory.history(forked, None).await.unwrap().len(),
        original.len() + 1
    );
    assert_eq!(
        memory.history(session, None).await.unwrap().len(),
        original.len(),
        "the source session must not absorb the branch's turns"
    );

    let fork_info = memory.session(forked).await.unwrap().unwrap();
    assert_eq!(fork_info.forked_from, Some(head));
    assert_eq!(fork_info.label.as_deref(), Some("alternative"));
    // `nodes` counts owned rows: the fork marker plus the appended turn. The
    // inherited chain is shared, so it is not counted here.
    assert_eq!(fork_info.nodes, 2);

    let sessions = memory.sessions().await.unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|info| info.nodes > 0));
}

#[tokio::test]
async fn a_fork_reports_the_session_it_was_taken_from() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let origin = memory.create_session(Some("origin")).await.unwrap();
    memory
        .append(origin, &Message::user("start here"))
        .await
        .unwrap();
    let head = memory.head(origin).await.unwrap().unwrap();

    let forked = memory.branch(origin, head, Some("fork")).await.unwrap();

    let fork_info = memory.session(forked).await.unwrap().unwrap();
    assert_eq!(fork_info.forked_from, Some(head));
    assert_eq!(
        fork_info.forked_from_session,
        Some(origin),
        "a fork names the session that owns the node it was taken from"
    );

    let origin_info = memory.session(origin).await.unwrap().unwrap();
    assert_eq!(origin_info.forked_from, None);
    assert_eq!(
        origin_info.forked_from_session, None,
        "a session that is not a fork has no parent session"
    );

    // The listing carries the same link, which is what the tree is drawn from.
    let listed = memory.sessions().await.unwrap();
    let listed_fork = listed
        .iter()
        .find(|info| info.id == forked)
        .expect("the fork is listed");
    assert_eq!(listed_fork.forked_from_session, Some(origin));
    let listed_origin = listed
        .iter()
        .find(|info| info.id == origin)
        .expect("the origin is listed");
    assert_eq!(listed_origin.forked_from_session, None);
}

#[tokio::test]
async fn a_reopened_database_returns_the_same_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("harness.db");

    let (session, before) = {
        let memory = SqliteMemory::open(&path).await.unwrap();
        let (session, _) = seeded(&memory).await;
        let before = memory.history(session, None).await.unwrap();
        (session, before)
    };

    assert!(
        path.exists(),
        "opening should create the file and its parents"
    );

    let reopened = SqliteMemory::open(&path).await.unwrap();
    let after = reopened.history(session, None).await.unwrap();
    assert_eq!(before, after);

    // And the DAG is still usable: trim survives a restart.
    let report = reopened
        .trim(session, TrimOptions::default())
        .await
        .unwrap();
    assert_eq!(report.nodes_trimmed, SEEDED_TOOL_TURNS);
}

#[tokio::test]
async fn recall_finds_a_verbatim_phrase_by_keyword() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("recall")).await.unwrap();
    memory
        .append(
            session,
            &Message::user("remember the codeword zanzibar-lantern for later"),
        )
        .await
        .unwrap();
    memory
        .append(session, &Message::assistant("noted"))
        .await
        .unwrap();
    for index in 0..12 {
        memory
            .append(
                session,
                &Message::assistant(format!("filler turn {index} about ordinary subjects")),
            )
            .await
            .unwrap();
    }

    let mut query = RecallQuery::new("zanzibar-lantern");
    query.session_id = Some(session);
    let hits = memory.recall(query).await.unwrap();

    assert!(!hits.is_empty(), "the codeword should be retrievable");
    assert!(
        hits[0].snippet.contains("zanzibar-lantern"),
        "top hit was {:?}",
        hits[0].snippet
    );
    assert_eq!(hits[0].session_id, session);
}

#[tokio::test]
async fn recall_never_exceeds_its_token_budget() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("budget")).await.unwrap();
    for index in 0..20 {
        memory
            .append(
                session,
                &Message::assistant(format!(
                    "point {index}: {}",
                    "the shared vocabulary appears repeatedly here. ".repeat(20)
                )),
            )
            .await
            .unwrap();
    }

    let mut query = RecallQuery::new("shared vocabulary appears repeatedly");
    query.session_id = Some(session);
    query.limit = 20;
    query.max_tokens = 60;

    let hits = memory.recall(query).await.unwrap();
    let used: usize = hits.iter().map(|hit| hit.tokens).sum();

    assert!(used <= 60, "budget 60 produced {used} tokens");
    assert!(
        !hits.is_empty(),
        "a smaller budget must still return something"
    );
}

#[tokio::test]
async fn recall_can_be_restricted_to_one_role() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("roles")).await.unwrap();
    memory
        .append(session, &Message::user("deployment checklist needs review"))
        .await
        .unwrap();
    memory
        .append(
            session,
            &Message::assistant("deployment checklist acknowledged"),
        )
        .await
        .unwrap();

    let mut query = RecallQuery::new("deployment checklist");
    query.session_id = Some(session);
    query.roles = vec![Role::Assistant];
    let hits = memory.recall(query).await.unwrap();

    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|hit| hit.role == Some(Role::Assistant)),
        "role filter leaked: {:?}",
        hits.iter().map(|hit| hit.role).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn stats_reflect_what_was_written() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let empty = memory.stats().await.unwrap();
    assert_eq!(empty.sessions, 0);

    let (session, _) = seeded(&memory).await;
    let stats = memory.stats().await.unwrap();

    assert_eq!(stats.sessions, 1);
    assert_eq!(stats.nodes, SEEDED_NODES);
    assert_eq!(
        stats.blobs, SEEDED_TOOL_TURNS,
        "each oversized tool body is parked in blobs"
    );
    assert!(stats.blob_bytes > 100_000);
    assert!(stats.total_tokens > 0);

    let info = memory.session(session).await.unwrap().unwrap();
    assert_eq!(info.nodes, SEEDED_NODES);
    assert!(info.head.is_some());
}

#[tokio::test]
async fn a_user_turn_becomes_the_session_title() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("titled")).await.unwrap();
    memory
        .append(session, &Message::user("deploy the staging cluster"))
        .await
        .unwrap();

    let info = memory.session(session).await.unwrap().unwrap();
    assert_eq!(info.title.as_deref(), Some("deploy the staging cluster"));

    let listed = memory.sessions().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].title.as_deref(),
        Some("deploy the staging cluster")
    );
}

#[tokio::test]
async fn the_title_is_the_first_user_turn_even_after_more_turns() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("first")).await.unwrap();
    memory
        .append(session, &Message::user("first question about auth"))
        .await
        .unwrap();
    memory
        .append(session, &Message::assistant("Answering."))
        .await
        .unwrap();
    memory
        .append(session, &Message::user("second question, unrelated"))
        .await
        .unwrap();

    let title = memory.session(session).await.unwrap().unwrap().title;
    assert_eq!(
        title.as_deref(),
        Some("first question about auth"),
        "the title must not follow the latest turn"
    );
}

#[tokio::test]
async fn a_session_without_a_user_turn_has_no_title() {
    let memory = SqliteMemory::in_memory().await.unwrap();

    let fresh = memory.create_session(Some("fresh")).await.unwrap();
    assert_eq!(memory.session(fresh).await.unwrap().unwrap().title, None);

    // A head exists, but nothing the user actually said is on it.
    let assistant_only = memory.create_session(Some("assistant-only")).await.unwrap();
    memory
        .append(assistant_only, &Message::assistant("ready when you are"))
        .await
        .unwrap();
    assert_eq!(
        memory.session(assistant_only).await.unwrap().unwrap().title,
        None
    );
}

#[tokio::test]
async fn a_long_first_message_is_truncated_to_a_preview() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("long")).await.unwrap();
    let prompt = format!(
        "audit the release pipeline {}",
        "and every downstream job ".repeat(50)
    );
    memory
        .append(session, &Message::user(&prompt))
        .await
        .unwrap();

    let title = memory
        .session(session)
        .await
        .unwrap()
        .unwrap()
        .title
        .expect("a title from a long first turn");

    assert!(title.ends_with('…'), "expected an ellipsis: {title}");
    assert!(
        prompt.starts_with(title.trim_end_matches('…')),
        "the preview must be a prefix of the message: {title}"
    );
    assert_eq!(title.chars().count(), 61, "60 characters plus the ellipsis");
}

#[tokio::test]
async fn a_branched_session_reports_the_inherited_first_turn() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("origin")).await.unwrap();
    memory
        .append(session, &Message::user("design the schema"))
        .await
        .unwrap();
    memory
        .append(session, &Message::assistant("Drafting it."))
        .await
        .unwrap();
    let head = memory.head(session).await.unwrap().unwrap();

    let forked = memory.branch(session, head, Some("fork")).await.unwrap();
    memory
        .append(forked, &Message::user("try the other design"))
        .await
        .unwrap();

    let title = memory.session(forked).await.unwrap().unwrap().title;
    assert_eq!(
        title.as_deref(),
        Some("design the schema"),
        "a fork reports the turn it inherited, not its own later turn"
    );
}
