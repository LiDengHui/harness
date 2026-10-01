//! `Memory::delete_session`: what goes, what stays, and what a fork keeps.
//!
//! The test opens a file-backed store rather than an in-memory one so it can
//! read the `nodes`, `blobs`, `embeddings` and `nodes_fts` tables directly: the
//! public API proves a session is gone, and the SQL proves *how* gone — a
//! delete that left blob or embedding rows behind would still pass a
//! `sessions()` assertion.

use harness_core::{Memory, Message, SessionId};
use harness_memory::{SqliteMemory, BLOB_THRESHOLD_CHARS};
use rusqlite::Connection;

/// Row counts for the tables a delete must empty.
fn table_counts(path: &std::path::Path) -> (i64, i64, i64, i64) {
    let conn = Connection::open(path).expect("the store is readable");
    let count = |table: &str| -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|err| panic!("count {table}: {err}"))
    };
    (
        count("nodes"),
        count("blobs"),
        count("embeddings"),
        count("nodes_fts"),
    )
}

#[tokio::test]
async fn deleting_a_session_removes_its_rows_and_leaves_a_fork_in_place() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let db = dir.path().join("memory.db");
    let memory = SqliteMemory::open(&db).await.expect("the store opens");

    let origin = memory
        .create_session(Some("origin"))
        .await
        .expect("a session");
    memory
        .append(origin, &Message::user("hello from the origin"))
        .await
        .expect("a first turn");
    // Over the blob threshold, so the origin owns a blob row as well.
    let big = "x".repeat(BLOB_THRESHOLD_CHARS + 64);
    memory
        .append(origin, &Message::tool_result("call_1", big))
        .await
        .expect("a large tool result");
    let head = memory.head(origin).await.expect("a head").expect("a node");

    // A fork inherits the origin's chain; it owns only its own marker and turn.
    let fork = memory
        .branch(origin, head, Some("fork"))
        .await
        .expect("a fork");
    memory
        .append(fork, &Message::user("this turn belongs to the fork"))
        .await
        .expect("a fork turn");

    let (nodes_before, blobs_before, embeddings_before, fts_before) = table_counts(&db);
    assert!(nodes_before > 0, "the origin wrote nodes");
    assert!(blobs_before > 0, "the large result was parked as a blob");
    assert!(embeddings_before > 0, "the turns were embedded");
    assert!(fts_before > 0, "the nodes were indexed for keyword search");

    let report = memory
        .delete_session(origin)
        .await
        .expect("the delete runs")
        .expect("the session existed");

    assert!(report.nodes > 0, "{report:?}");
    assert!(report.blobs > 0, "{report:?}");
    assert!(report.embeddings > 0, "{report:?}");
    assert_eq!(
        report.forks, 1,
        "the fork was counted, not cascaded: {report:?}"
    );

    // Gone from the list, and `session()` — the value the server's history
    // route 404s on — is `None`.
    assert!(
        memory
            .sessions()
            .await
            .expect("the list reads")
            .iter()
            .all(|info| info.id != origin),
        "the deleted session must not be listed"
    );
    assert!(memory
        .session(origin)
        .await
        .expect("the lookup runs")
        .is_none());

    // Nothing left to replay, and the node is unreachable through every reader.
    assert!(memory
        .history(origin, None)
        .await
        .expect("history reads")
        .is_empty());
    assert!(memory.node(head).await.expect("a lookup").is_none());
    assert!(memory.raw_output(head).await.expect("a lookup").is_none());

    // The origin's own blob and embedding rows are gone; the fork's remain.
    let (nodes_after, blobs_after, embeddings_after, fts_after) = table_counts(&db);
    assert_eq!(
        blobs_after, 0,
        "no blob may outlive the nodes that owned it"
    );
    assert!(
        embeddings_after < embeddings_before,
        "the origin's embeddings must be gone ({embeddings_before} -> {embeddings_after})"
    );
    assert!(
        nodes_after > 0 && nodes_after < nodes_before,
        "the fork's own nodes stay ({nodes_before} -> {nodes_after})"
    );
    assert!(
        fts_after < fts_before,
        "the FTS index follows the deleted nodes ({fts_before} -> {fts_after})"
    );

    // The fork is a conversation of its own, so it survives — with its own turn,
    // and without the prefix it inherited from the deleted origin.
    let fork_info = memory
        .session(fork)
        .await
        .expect("the lookup runs")
        .expect("the fork survives");
    assert!(fork_info.forked_from.is_some());
    assert_eq!(
        fork_info.forked_from_session, None,
        "the node it forked from is gone, so the reference cannot resolve"
    );
    let fork_history = memory.history(fork, None).await.expect("history reads");
    assert_eq!(fork_history.len(), 1, "{fork_history:?}");
    assert_eq!(fork_history[0].text(), "this turn belongs to the fork");
}

#[tokio::test]
async fn deleting_a_session_that_does_not_exist_reports_none() {
    let memory = SqliteMemory::in_memory().await.expect("the store opens");
    let missing = SessionId::new();

    assert!(memory
        .delete_session(missing)
        .await
        .expect("the delete runs")
        .is_none());
    assert_eq!(memory.stats().await.expect("stats read").sessions, 0);
}

#[tokio::test]
async fn deleting_a_session_twice_is_not_an_error_the_second_time() {
    let memory = SqliteMemory::in_memory().await.expect("the store opens");
    let session = memory
        .create_session(Some("once"))
        .await
        .expect("a session");
    memory
        .append(session, &Message::user("only turn"))
        .await
        .expect("a turn");

    assert!(memory
        .delete_session(session)
        .await
        .expect("the first delete")
        .is_some());
    assert!(memory
        .delete_session(session)
        .await
        .expect("the second delete")
        .is_none());
}
