//! `harness memory`: inspect and manipulate the conversation DAG.

use chrono::{DateTime, Duration, Utc};
use harness_core::{Memory, NodeId, NodeKind, SessionId, TrimOptions};
use harness_memory::SqliteMemory;

use crate::commands::MemoryCommand;
use crate::context::AppContext;

pub(crate) async fn execute(command: MemoryCommand, ctx: &AppContext) -> anyhow::Result<()> {
    let memory = SqliteMemory::open(ctx.db_path())
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    match command {
        MemoryCommand::Tree { session } => tree(&memory, session.as_deref()).await,
        MemoryCommand::Stats => stats(&memory).await,
        MemoryCommand::Trim { session } => trim(&memory, &session).await,
        MemoryCommand::Branch { session, from } => branch(&memory, &session, from.as_deref()).await,
        MemoryCommand::Rm {
            session,
            older_than,
            dry_run,
        } => rm(&memory, session.as_deref(), older_than.as_deref(), dry_run).await,
    }
}

async fn tree(memory: &SqliteMemory, only: Option<&str>) -> anyhow::Result<()> {
    let sessions = memory.sessions().await.map_err(err)?;
    if sessions.is_empty() {
        println!("no sessions stored yet; `harness run` writes one per invocation");
        return Ok(());
    }

    let mut shown = 0usize;
    for info in sessions {
        if let Some(wanted) = only {
            if info.id.to_string() != wanted {
                continue;
            }
        }
        shown += 1;

        println!(
            "session {}  {}",
            info.id,
            info.label.as_deref().unwrap_or("(unlabelled)")
        );
        println!(
            "  owned nodes: {}   tokens: {}   created: {}",
            info.nodes,
            info.tokens,
            info.created_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        if let Some(origin) = info.forked_from {
            println!("  forked from node {}", short(origin));
        }

        let Some(head) = info.head else { continue };
        for node in memory.path(head).await.map_err(err)? {
            let marker = match node.kind {
                NodeKind::Snapshot => "~",
                NodeKind::Turn => "|",
            };
            let role = node.role.map(|role| role.as_str()).unwrap_or("marker");
            let suffix = if node.trimmed { " [elided]" } else { "" };
            // A turn that only requests tools carries no text, so show the calls
            // that the empty content would otherwise hide. A snapshot has no
            // content at all: its meaning is in the label, so a stage boundary
            // or a fork point reads as itself rather than as an empty marker.
            let body = if node.content.trim().is_empty() && !node.tool_calls.is_empty() {
                node.tool_calls
                    .iter()
                    .map(|call| format!("-> {}({})", call.name, call.arguments))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else if node.content.trim().is_empty() {
                node.label.clone().unwrap_or_default()
            } else {
                node.content.clone()
            };
            println!(
                "  {marker} {} {role:<9} {}{suffix}",
                short(node.id),
                preview(&body, 68)
            );
        }
    }

    if shown == 0 {
        anyhow::bail!("no session matched that id");
    }
    Ok(())
}

async fn stats(memory: &SqliteMemory) -> anyhow::Result<()> {
    let stats = memory.stats().await.map_err(err)?;
    println!("sessions: {}", stats.sessions);
    println!("nodes:    {}", stats.nodes);
    println!(
        "blobs:    {} ({:.1} KB retained off-context)",
        stats.blobs,
        stats.blob_bytes as f64 / 1024.0
    );
    println!("tokens:   {}", stats.total_tokens);
    Ok(())
}

async fn trim(memory: &SqliteMemory, session: &str) -> anyhow::Result<()> {
    let session_id: SessionId = session.parse()?;
    let report = memory
        .trim(session_id, TrimOptions::default())
        .await
        .map_err(err)?;

    println!(
        "examined {} nodes, elided {}",
        report.nodes_examined, report.nodes_trimmed
    );
    println!(
        "tokens: {} -> {} (saved {:.1}%)",
        report.tokens_before,
        report.tokens_after,
        report.saved_percent()
    );
    println!("elided tool output stays retrievable; nothing conversational was rewritten");
    Ok(())
}

async fn branch(memory: &SqliteMemory, session: &str, from: Option<&str>) -> anyhow::Result<()> {
    let session_id: SessionId = session.parse()?;
    let node =
        match from {
            Some(raw) => raw.parse::<NodeId>()?,
            None => memory.head(session_id).await.map_err(err)?.ok_or_else(|| {
                anyhow::anyhow!("session {session_id} has no nodes to branch from")
            })?,
        };

    let forked = memory.branch(session_id, node, None).await.map_err(err)?;
    println!("branched session {forked} from node {node}");
    println!("the fork shares the ancestor chain; new turns land only in the fork");
    Ok(())
}

/// `memory rm`: delete one session, or every session older than an age.
///
/// The two forms are mutually exclusive because they name different sets: an id
/// is one conversation, `--older-than` is a sweep. Accepting both would leave
/// the user guessing which one won.
async fn rm(
    memory: &SqliteMemory,
    session: Option<&str>,
    older_than: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    match (session, older_than) {
        (Some(id), None) => rm_one(memory, id, dry_run).await,
        (None, Some(age)) => rm_older_than(memory, age, dry_run).await,
        (Some(_), Some(_)) => anyhow::bail!(
            "give either a session id or --older-than, not both: they name different sets"
        ),
        (None, None) => anyhow::bail!(
            "nothing to delete: give a session id, or --older-than <AGE> to delete in bulk"
        ),
    }
}

async fn rm_one(memory: &SqliteMemory, raw: &str, dry_run: bool) -> anyhow::Result<()> {
    let session_id: SessionId = raw.parse()?;

    if dry_run {
        let info = memory
            .session(session_id)
            .await
            .map_err(err)?
            .ok_or_else(|| anyhow::anyhow!("no session with id {session_id}"))?;
        println!(
            "would delete session {session_id}  {}  ({} nodes, created {})",
            info.title.as_deref().unwrap_or("(untitled)"),
            info.nodes,
            info.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
        );
        return Ok(());
    }

    let report = memory
        .delete_session(session_id)
        .await
        .map_err(err)?
        .ok_or_else(|| anyhow::anyhow!("no session with id {session_id}"))?;
    println!(
        "deleted session {session_id}: {} nodes, {} blobs, {} embeddings",
        report.nodes, report.blobs, report.embeddings
    );
    if report.forks > 0 {
        println!(
            "note: {} session(s) forked from this one were left in place; the history they \
             inherited from it is gone",
            report.forks
        );
    }
    Ok(())
}

async fn rm_older_than(memory: &SqliteMemory, age: &str, dry_run: bool) -> anyhow::Result<()> {
    let cutoff = cutoff_of(age)?;
    let victims: Vec<_> = memory
        .sessions()
        .await
        .map_err(err)?
        .into_iter()
        .filter(|info| info.created_at < cutoff)
        .collect();

    if victims.is_empty() {
        println!(
            "no sessions are older than {age} (before {})",
            cutoff.format("%Y-%m-%d %H:%M:%S UTC")
        );
        return Ok(());
    }

    if dry_run {
        for info in &victims {
            println!(
                "would delete {}  {}  ({} nodes, created {})",
                info.id,
                info.title.as_deref().unwrap_or("(untitled)"),
                info.nodes,
                info.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
            );
        }
        println!(
            "\n{} session(s) would be deleted; nothing was changed",
            victims.len()
        );
        return Ok(());
    }

    let mut deleted = 0usize;
    let mut nodes = 0usize;
    let mut blobs = 0usize;
    let mut embeddings = 0usize;
    for info in &victims {
        // A session can vanish between the list and the delete if another
        // process is cleaning up too; that is `None`, not a failure.
        if let Some(report) = memory.delete_session(info.id).await.map_err(err)? {
            deleted += 1;
            nodes += report.nodes;
            blobs += report.blobs;
            embeddings += report.embeddings;
        }
    }
    println!("deleted {deleted} session(s): {nodes} nodes, {blobs} blobs, {embeddings} embeddings");
    Ok(())
}

/// The instant `--older-than <AGE>` means: now minus the age.
///
/// Units are single letters so the flag reads like a duration (`30d`) rather
/// than like a date, which is what a cleanup sweep is usually phrased in. A
/// bare number is refused because its unit would be a guess.
fn cutoff_of(spec: &str) -> anyhow::Result<DateTime<Utc>> {
    let spec = spec.trim();
    let mut chars = spec.chars();
    let unit = chars
        .next_back()
        .ok_or_else(|| anyhow::anyhow!("--older-than needs an age like `30d`"))?;
    // Caught before the unit match so `--older-than 30` says "missing a unit"
    // rather than naming the last digit as an unknown unit.
    if unit.is_ascii_digit() {
        anyhow::bail!("`{spec}` is missing a unit; use e.g. `30d`, `12h`, `90m`, `2w`");
    }
    let amount: i64 = chars.as_str().trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "`{spec}` is not an age; use a number and a unit, e.g. `30d`, `12h`, `90m`, `2w`"
        )
    })?;
    if amount < 0 {
        anyhow::bail!("`{spec}` is negative; --older-than takes a positive age");
    }

    let seconds = match unit {
        's' => amount,
        'm' => amount.saturating_mul(60),
        'h' => amount.saturating_mul(3_600),
        'd' => amount.saturating_mul(86_400),
        'w' => amount.saturating_mul(604_800),
        other => anyhow::bail!("unknown age unit `{other}` in `{spec}`; use s, m, h, d or w"),
    };
    Ok(Utc::now() - Duration::seconds(seconds))
}

fn err(error: harness_core::HarnessError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

/// ULIDs are 26 characters of mostly-entropy; the tail is what distinguishes them.
fn short(id: NodeId) -> String {
    let text = id.to_string();
    text.chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn preview(text: &str, width: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > width {
        let clipped: String = flat.chars().take(width).collect();
        format!("{clipped}…")
    } else if flat.is_empty() {
        "(empty)".to_string()
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_age_is_now_minus_that_much() {
        let before = Utc::now();
        let cutoff = cutoff_of("30d").expect("30d is a valid age");
        let after = Utc::now();

        assert!(cutoff >= before - Duration::days(30));
        assert!(cutoff <= after - Duration::days(30));
    }

    #[test]
    fn every_unit_is_accepted() {
        for spec in ["30s", "30m", "30h", "30d", "30w"] {
            assert!(cutoff_of(spec).is_ok(), "{spec}");
        }
    }

    #[test]
    fn a_bare_number_or_an_unknown_unit_is_refused() {
        let bare = cutoff_of("30").expect_err("a bare number has no unit");
        assert!(bare.to_string().contains("missing a unit"), "{bare}");

        let unknown = cutoff_of("30y").expect_err("y is not a unit");
        assert!(
            unknown.to_string().contains("unknown age unit"),
            "{unknown}"
        );
    }

    #[test]
    fn a_negative_age_is_refused() {
        let err = cutoff_of("-5d").expect_err("a negative age is nonsense");
        assert!(err.to_string().contains("negative"), "{err}");
    }
}
