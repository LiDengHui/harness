//! Incremental persistence: each completed turn is durable before the next one
//! starts, so a crash between turns keeps everything already produced.
//!
//! The loop's `run_persisting*` hands a `MemoryRecorder` the messages appended
//! since the previous turn boundary; this exercises the storage half of that
//! contract against a real file, which is what survives a process dying.

use harness_core::{Memory, Message, Role};
use harness_memory::SqliteMemory;

/// Appends `history[cursor..]` and advances the cursor, the way the loop's
/// recorder does at a turn boundary.
async fn flush(
    memory: &SqliteMemory,
    session: harness_core::SessionId,
    history: &[Message],
    cursor: &mut usize,
) {
    for message in &history[*cursor..] {
        memory.append(session, message).await.unwrap();
        *cursor += 1;
    }
}

#[tokio::test]
async fn turns_recorded_before_a_crash_are_all_present_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    let session;

    {
        let memory = SqliteMemory::open(&path).await.unwrap();
        session = memory.create_session(Some("crashy")).await.unwrap();

        let mut history = vec![Message::user("investigate the failing build")];
        let mut cursor = 0usize;
        // The user's question is recorded before the first model call.
        flush(&memory, session, &history, &mut cursor).await;

        // Turn 1 completes: assistant reasoning plus a tool result.
        history.push(Message::assistant("Reading the build log."));
        history.push(Message::tool_result(
            "call_1",
            "error: missing symbol `foo`",
        ));
        flush(&memory, session, &history, &mut cursor).await;

        // Turn 2 completes.
        history.push(Message::assistant(
            "The symbol is defined behind a cfg flag.",
        ));
        flush(&memory, session, &history, &mut cursor).await;

        // Turn 3 is in flight: the process dies before its answer exists.
        drop(memory);
    }

    let reopened = SqliteMemory::open(&path).await.unwrap();
    let history = reopened.history(session, None).await.unwrap();
    assert_eq!(
        history.len(),
        4,
        "the four recorded messages must survive: {:?}",
        history
            .iter()
            .map(|message| message.text())
            .collect::<Vec<_>>()
    );
    assert_eq!(history[0].text(), "investigate the failing build");
    assert_eq!(history[1].role, Role::Assistant);
    assert_eq!(history[2].tool_call_id.as_deref(), Some("call_1"));
    assert!(history[3].text().contains("cfg flag"));

    // The DAG is still usable: a new turn lands on the head the crash left.
    reopened
        .append(session, &Message::user("continue from here"))
        .await
        .unwrap();
    let continued = reopened.history(session, None).await.unwrap();
    assert_eq!(continued.len(), 5);
    assert_eq!(continued[4].text(), "continue from here");
}

#[tokio::test]
async fn a_crash_after_only_the_question_loses_nothing_the_user_said() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("question.db");
    let session;

    {
        let memory = SqliteMemory::open(&path).await.unwrap();
        session = memory.create_session(Some("early-crash")).await.unwrap();
        // The question is flushed before the first request; then the crash.
        memory
            .append(session, &Message::user("what does the deploy script do?"))
            .await
            .unwrap();
        drop(memory);
    }

    let reopened = SqliteMemory::open(&path).await.unwrap();
    let history = reopened.history(session, None).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text(), "what does the deploy script do?");
}
