//! SQLite storage for the memory DAG: schema, migration and the [`Memory`]
//! implementation.
//!
//! Everything runs behind one `parking_lot::Mutex<Connection>`. SQLite has a
//! single writer anyway, and holding a guard across a statement is microseconds
//! of work, so the alternative — a pool, or `spawn_blocking` per call — buys
//! throughput this workload does not have while costing a `Connection` ownership
//! dance on every method. If memory access ever shows up in a profile, this is
//! the first thing to revisit.
//!
//! The one deliberate oddity is `blobs.body` being nullable: trim's reference
//! compaction clears the body of a duplicate row instead of deleting it, so the
//! row keeps the digest that tells `raw_output` where the canonical copy lives.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};

use harness_core::message::ToolCall;
use harness_core::{
    HarnessError, HeuristicEstimator, Memory, MemoryStats, Message, Node, NodeId, NodeKind,
    RecallHit, RecallQuery, Result, Role, SessionDeleteReport, SessionId, SessionInfo,
    TokenEstimator, TrimOptions, TrimReport,
};

use crate::hash::sha256_hex;
use crate::index::HybridIndex;
use crate::recall::{fit_snippet, keyword_ranking, snippet, CANDIDATE_FANOUT};
use crate::trim::{elide_tool_output, is_elision_reference, strip_inline_binary};
use crate::vector::{BruteForceIndex, HashingEmbedder};

/// Embedding width used when the caller does not pick one; matches
/// `harness_core::MemoryConfig::default`.
pub const DEFAULT_EMBEDDING_DIM: usize = 256;

/// Message content at or above this many bytes is copied into `blobs` as it is
/// appended. Trimming then only has to flip the content to a reference — the
/// full body is already durable, which is what makes trim lossless and cheap.
pub const BLOB_THRESHOLD_CHARS: usize = 4 * 1024;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    label       TEXT,
    created_at  TEXT NOT NULL,
    forked_from TEXT
);

CREATE TABLE IF NOT EXISTS nodes (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL,
    parent_id    TEXT,
    kind         TEXT NOT NULL,
    role         TEXT,
    content      TEXT NOT NULL DEFAULT '',
    tool_calls   TEXT,
    tool_call_id TEXT,
    label        TEXT,
    tokens       INTEGER NOT NULL DEFAULT 0,
    trimmed      INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_nodes_session_parent ON nodes(session_id, parent_id);
CREATE INDEX IF NOT EXISTS idx_nodes_parent ON nodes(parent_id);

CREATE TABLE IF NOT EXISTS blobs (
    node_id TEXT PRIMARY KEY,
    bytes   INTEGER NOT NULL,
    sha256  TEXT NOT NULL,
    body    TEXT
);
CREATE INDEX IF NOT EXISTS idx_blobs_sha ON blobs(sha256);

CREATE TABLE IF NOT EXISTS embeddings (
    node_id TEXT PRIMARY KEY,
    dim     INTEGER NOT NULL,
    vector  BLOB NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    content,
    content='nodes',
    content_rowid='rowid',
    tokenize='unicode61'
);

CREATE TRIGGER IF NOT EXISTS nodes_fts_after_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS nodes_fts_after_delete AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
END;

CREATE TRIGGER IF NOT EXISTS nodes_fts_after_update AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
    INSERT INTO nodes_fts(rowid, content) VALUES (new.rowid, new.content);
END;
"#;

/// SQLite-backed memory. Cheap to clone the handle it hands out, but the type
/// itself is the single point of contention for every read and write.
pub struct SqliteMemory {
    conn: Mutex<Connection>,
    embedder: HashingEmbedder,
    estimator: HeuristicEstimator,
}

impl SqliteMemory {
    /// Creates parent directories, opens the database, applies migrations and
    /// switches on WAL.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_dim(path, DEFAULT_EMBEDDING_DIM).await
    }

    /// Like [`SqliteMemory::open`], but seeds the embedder width. A database
    /// that already holds embeddings wins: the stored width is adopted so a
    /// reopen cannot silently append vectors the existing index cannot compare.
    pub async fn open_with_dim(path: impl AsRef<Path>, dim: usize) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(HarnessError::Io)?;
            }
        }
        let conn = Connection::open(path).map_err(db_err)?;
        Self::prepare(conn, dim)
    }

    /// A throwaway store, for tests and dry runs.
    ///
    /// In-memory databases cannot use WAL; [`configure`] reports the journal
    /// mode instead of failing when the request is not honoured.
    pub async fn in_memory() -> Result<Self> {
        Self::in_memory_with_dim(DEFAULT_EMBEDDING_DIM).await
    }

    pub async fn in_memory_with_dim(dim: usize) -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        Self::prepare(conn, dim)
    }

    fn prepare(conn: Connection, dim: usize) -> Result<Self> {
        configure(&conn)?;
        migrate(&conn)?;
        let dim = stored_embedding_dim(&conn)?.unwrap_or_else(|| dim.max(1));
        Ok(Self {
            conn: Mutex::new(conn),
            embedder: HashingEmbedder::new(dim),
            estimator: HeuristicEstimator::default(),
        })
    }

    /// The embedder in use, after any adopted width.
    pub fn embedder(&self) -> &HashingEmbedder {
        &self.embedder
    }

    fn trim_locked(
        &self,
        conn: &Connection,
        session_id: SessionId,
        options: TrimOptions,
    ) -> Result<TrimReport> {
        let nodes = session_nodes(conn, session_id, true)?;
        let tokens_before = session_tokens(conn, session_id)?;

        // The tail is left alone so the active context keeps full detail.
        let unprotected = nodes.len().saturating_sub(options.keep_recent);
        let mut nodes_trimmed = 0usize;

        for raw in nodes.iter().take(unprotected) {
            if raw.trimmed != 0 || is_elision_reference(&raw.content) {
                continue;
            }
            let node_id: NodeId = raw.id.parse()?;
            let is_tool = raw.role.as_deref() == Some(Role::Tool.as_str());

            // Phase 1 — wholesale rewrite of oversized tool output.
            if is_tool && raw.content.len() >= options.min_tool_output_chars {
                store_blob(conn, node_id, &raw.content)?;
                let reference = elide_tool_output(node_id, raw.content.len());
                write_node_content(conn, node_id, &reference, true, &self.estimator)?;
                nodes_trimmed += 1;
                continue;
            }

            // Phase 2 — mechanical stripping of inline binary, wherever it is.
            if options.strip_base64 {
                if let Some(stripped) = strip_inline_binary(&raw.content) {
                    // Keep the original, so a conversational turn that loses an
                    // embedded image is still recoverable through `raw_output`.
                    store_blob(conn, node_id, &raw.content)?;
                    write_node_content(conn, node_id, &stripped, true, &self.estimator)?;
                    nodes_trimmed += 1;
                }
            }
        }

        compact_blobs(conn, session_id)?;

        Ok(TrimReport {
            nodes_examined: nodes.len(),
            nodes_trimmed,
            tokens_before,
            tokens_after: session_tokens(conn, session_id)?,
        })
    }

    fn recall_locked(&self, conn: &Connection, query: RecallQuery) -> Result<Vec<RecallHit>> {
        if query.limit == 0 || query.max_tokens == 0 {
            return Ok(Vec::new());
        }
        let candidates = query
            .limit
            .saturating_mul(CANDIDATE_FANOUT)
            .max(query.limit);

        let index = BruteForceIndex::from_rows(load_embeddings(conn)?);
        let hybrid = HybridIndex::new(Box::new(index), self.embedder.clone());
        let keyword = keyword_ranking(conn, &query.text, candidates)?;
        let fused = hybrid.search(&query.text, &keyword, candidates)?;

        let mut hits: Vec<RecallHit> = Vec::new();
        let mut used = 0usize;

        for (node_id, score) in fused {
            if hits.len() >= query.limit {
                break;
            }
            let Some(node) = node_by_id(conn, node_id)? else {
                continue;
            };
            if let Some(session_id) = query.session_id {
                if node.session_id != session_id {
                    continue;
                }
            }
            if !query.roles.is_empty() && !node.role.is_some_and(|role| query.roles.contains(&role))
            {
                continue;
            }

            let (body, clipped) = snippet(&node.content);
            let tokens = self.estimator.estimate(&body);
            if used + tokens <= query.max_tokens {
                used += tokens;
                hits.push(RecallHit {
                    node_id,
                    session_id: node.session_id,
                    role: node.role,
                    snippet: body,
                    score,
                    tokens,
                    truncated: clipped,
                });
                continue;
            }

            // Part of the budget is still free: spend it on a prefix of this hit
            // rather than dropping the candidate, but never overshoot. The budget
            // is full afterwards, so this is the last hit either way.
            let remaining = query.max_tokens - used;
            if let Some(fitted) = fit_snippet(&body, remaining, &self.estimator) {
                let tokens = self.estimator.estimate(&fitted);
                hits.push(RecallHit {
                    node_id,
                    session_id: node.session_id,
                    role: node.role,
                    snippet: fitted,
                    score,
                    tokens,
                    truncated: true,
                });
            }
            break;
        }

        Ok(hits)
    }
}

#[async_trait]
impl Memory for SqliteMemory {
    async fn create_session(&self, label: Option<&str>) -> Result<SessionId> {
        let conn = self.conn.lock();
        insert_session(&conn, label, None)
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare(&format!("{SESSION_INFO_SQL} ORDER BY created_at, id"))
            .map_err(db_err)?;
        let rows = statement.query_map([], session_row).map_err(db_err)?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(session_info(&conn, row.map_err(db_err)?)?);
        }
        Ok(sessions)
    }

    async fn session(&self, session_id: SessionId) -> Result<Option<SessionInfo>> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                &format!("{SESSION_INFO_SQL} WHERE sessions.id = ?1"),
                params![session_id.to_string()],
                session_row,
            )
            .optional()
            .map_err(db_err)?;

        row.map(|row| session_info(&conn, row)).transpose()
    }

    async fn head(&self, session_id: SessionId) -> Result<Option<NodeId>> {
        let conn = self.conn.lock();
        head_of(&conn, session_id)
    }

    async fn append(&self, session_id: SessionId, message: &Message) -> Result<NodeId> {
        let conn = self.conn.lock();
        if !session_exists(&conn, session_id)? {
            return Err(HarnessError::Memory(format!(
                "cannot append: session {session_id} does not exist"
            )));
        }

        let node_id = NodeId::new();
        let content = message.text().to_string();
        let created_at = Utc::now();
        insert_node(
            &conn,
            &NewNode {
                id: node_id,
                session_id,
                parent_id: head_of(&conn, session_id)?,
                kind: NodeKind::Turn,
                role: Some(message.role),
                content: &content,
                tool_calls: &message.tool_calls,
                tool_call_id: message.tool_call_id.as_deref(),
                // `Node::to_message` reads the message name back out of `label`,
                // so storing it here is what keeps `history` faithful.
                label: message.name.as_deref(),
                tokens: self.estimator.estimate(&content),
                trimmed: false,
                created_at,
            },
        )?;

        if content.len() >= BLOB_THRESHOLD_CHARS {
            store_blob(&conn, node_id, &content)?;
        }
        // Embedding from the untrimmed content is deliberate: the vector index is
        // what finds a tool output again after trim has replaced its text with a
        // reference, and that reference carries almost no signal.
        if content.chars().any(|c| c.is_alphanumeric()) {
            let vector = self.embedder.embed(&content);
            store_embedding(&conn, node_id, &vector)?;
        }
        Ok(node_id)
    }

    async fn snapshot(&self, session_id: SessionId, label: &str) -> Result<NodeId> {
        let conn = self.conn.lock();
        if !session_exists(&conn, session_id)? {
            return Err(HarnessError::Memory(format!(
                "cannot snapshot: session {session_id} does not exist"
            )));
        }

        let node_id = NodeId::new();
        insert_node(
            &conn,
            &NewNode {
                id: node_id,
                session_id,
                parent_id: head_of(&conn, session_id)?,
                kind: NodeKind::Snapshot,
                role: None,
                content: "",
                tool_calls: &[],
                tool_call_id: None,
                label: Some(label),
                tokens: 0,
                trimmed: false,
                created_at: Utc::now(),
            },
        )?;
        Ok(node_id)
    }

    /// Forks a conversation.
    ///
    /// The new session does not duplicate the ancestor nodes. Nodes are
    /// immutable, so the fork is recorded as a `Snapshot`-kind marker whose
    /// parent is `from_node`: the parent chain then carries the inherited
    /// history, `history` reproduces exactly the original's prefix, and the fork
    /// becomes visible as a second child of `from_node`.
    ///
    /// A copying fork cannot have that edge. Each node holds exactly one
    /// `parent_id`, so a chain of copies would have to root itself at a fresh
    /// node — leaving nothing pointing at `from_node` — and re-parenting it to
    /// `from_node` would repeat the whole ancestor path in the branch's history.
    /// Inheritance also matches the contract's own description ("forks a new
    /// session that inherits an ancestor chain … share history without copying
    /// it"). Blob payloads stay reachable through the same node ids, which
    /// `raw_output` resolves from the shared `blobs` rows.
    async fn branch(
        &self,
        from_session: SessionId,
        from_node: NodeId,
        label: Option<&str>,
    ) -> Result<SessionId> {
        let conn = self.conn.lock();
        let origin = node_by_id(&conn, from_node)?.ok_or_else(|| {
            HarnessError::Memory(format!("cannot branch: node {from_node} does not exist"))
        })?;
        if origin.session_id != from_session {
            return Err(HarnessError::Memory(format!(
                "cannot branch: node {from_node} does not belong to session {from_session}"
            )));
        }

        let branch_session = insert_session(&conn, label, Some(from_node))?;
        let marker = NodeId::new();
        insert_node(
            &conn,
            &NewNode {
                id: marker,
                session_id: branch_session,
                parent_id: Some(from_node),
                kind: NodeKind::Snapshot,
                role: None,
                content: "",
                tool_calls: &[],
                tool_call_id: None,
                label: Some(label.unwrap_or("fork")),
                tokens: 0,
                trimmed: false,
                created_at: Utc::now(),
            },
        )?;
        Ok(branch_session)
    }

    async fn history(&self, session_id: SessionId, head: Option<NodeId>) -> Result<Vec<Message>> {
        let conn = self.conn.lock();
        let start = match head {
            Some(node_id) => Some(node_id),
            None => head_of(&conn, session_id)?,
        };
        let Some(start) = start else {
            return Ok(Vec::new());
        };

        Ok(ancestors(&conn, start)?
            .into_iter()
            .filter(|node| node.kind == NodeKind::Turn)
            .map(|node| node.to_message())
            .collect())
    }

    async fn path(&self, node_id: NodeId) -> Result<Vec<Node>> {
        let conn = self.conn.lock();
        ancestors(&conn, node_id)
    }

    async fn node(&self, node_id: NodeId) -> Result<Option<Node>> {
        let conn = self.conn.lock();
        node_by_id(&conn, node_id)
    }

    async fn children(&self, parent: NodeId) -> Result<Vec<Node>> {
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare(
                "SELECT id, session_id, parent_id, kind, role, content, tool_calls, tool_call_id, \
                 label, tokens, trimmed, created_at FROM nodes WHERE parent_id = ?1 \
                 ORDER BY created_at, rowid",
            )
            .map_err(db_err)?;
        let rows = statement
            .query_map(params![parent.to_string()], RawNode::from_row)
            .map_err(db_err)?;

        let mut children = Vec::new();
        for row in rows {
            children.push(row.map_err(db_err)?.into_node()?);
        }
        Ok(children)
    }

    async fn trim(&self, session_id: SessionId, options: TrimOptions) -> Result<TrimReport> {
        let conn = self.conn.lock();
        self.trim_locked(&conn, session_id, options)
    }

    async fn recall(&self, query: RecallQuery) -> Result<Vec<RecallHit>> {
        let conn = self.conn.lock();
        self.recall_locked(&conn, query)
    }

    async fn raw_output(&self, node_id: NodeId) -> Result<Option<String>> {
        let conn = self.conn.lock();
        raw_output_of(&conn, node_id)
    }

    async fn stats(&self) -> Result<MemoryStats> {
        let conn = self.conn.lock();
        Ok(MemoryStats {
            sessions: count(&conn, "SELECT count(*) FROM sessions")?,
            nodes: count(&conn, "SELECT count(*) FROM nodes")?,
            // A row whose body was compacted away is not a retrievable payload;
            // its twin is counted instead, so the number stays "distinct bodies
            // still kept out of the context".
            blobs: count(&conn, "SELECT count(*) FROM blobs WHERE body IS NOT NULL")?,
            blob_bytes: count(
                &conn,
                "SELECT COALESCE(SUM(LENGTH(body)), 0) FROM blobs WHERE body IS NOT NULL",
            )?,
            total_tokens: count(&conn, "SELECT COALESCE(SUM(tokens), 0) FROM nodes")?,
        })
    }

    async fn delete_session(&self, session_id: SessionId) -> Result<Option<SessionDeleteReport>> {
        let mut conn = self.conn.lock();
        delete_session_locked(&mut conn, session_id)
    }
}

/// Deletes a session's own rows in one transaction.
///
/// The order is forced by the schema: blobs and embeddings are keyed by
/// `node_id`, so they must go while the nodes still exist to select on. The
/// nodes then go last of the three, which is also what makes the `nodes_fts`
/// delete trigger fire once per row and keep the FTS index consistent — there is
/// no separate FTS cleanup here because the triggers own it.
///
/// The fork count is taken before the nodes disappear: `sessions.forked_from`
/// stores the node a fork was taken from, so once that node is gone the
/// reference can no longer be resolved and the count would read zero.
fn delete_session_locked(
    conn: &mut Connection,
    session_id: SessionId,
) -> Result<Option<SessionDeleteReport>> {
    if !session_exists(conn, session_id)? {
        return Ok(None);
    }

    let tx = conn.transaction().map_err(db_err)?;
    let id = session_id.to_string();

    let forks: i64 = tx
        .query_row(
            "SELECT count(*) FROM sessions WHERE forked_from IN \
             (SELECT id FROM nodes WHERE session_id = ?1)",
            params![id],
            |row| row.get(0),
        )
        .map_err(db_err)?;

    let blobs = tx
        .execute(
            "DELETE FROM blobs WHERE node_id IN (SELECT id FROM nodes WHERE session_id = ?1)",
            params![id],
        )
        .map_err(db_err)?;
    let embeddings = tx
        .execute(
            "DELETE FROM embeddings WHERE node_id IN (SELECT id FROM nodes WHERE session_id = ?1)",
            params![id],
        )
        .map_err(db_err)?;
    let nodes = tx
        .execute("DELETE FROM nodes WHERE session_id = ?1", params![id])
        .map_err(db_err)?;
    tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])
        .map_err(db_err)?;

    tx.commit().map_err(db_err)?;

    Ok(Some(SessionDeleteReport {
        nodes,
        blobs,
        embeddings,
        forks: forks.max(0) as usize,
    }))
}

const SESSION_INFO_SQL: &str = "SELECT sessions.id, sessions.label, sessions.created_at, \
     sessions.forked_from, \
     (SELECT count(*) FROM nodes WHERE nodes.session_id = sessions.id), \
     (SELECT COALESCE(SUM(tokens), 0) FROM nodes WHERE nodes.session_id = sessions.id), \
     (SELECT id FROM nodes WHERE nodes.session_id = sessions.id \
        ORDER BY created_at DESC, rowid DESC LIMIT 1) \
     FROM sessions";

type SessionRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    i64,
    i64,
    Option<String>,
);

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

/// Characters kept from a session's first user turn before an ellipsis is
/// appended. A pasted prompt must not become the label the list shows.
const TITLE_PREVIEW_CHARS: usize = 60;

fn session_info(conn: &Connection, row: SessionRow) -> Result<SessionInfo> {
    let head = row.6.map(|id| id.parse::<NodeId>()).transpose()?;
    let forked_from = row.3.map(|id| id.parse::<NodeId>()).transpose()?;
    let forked_from_session = match forked_from {
        Some(node_id) => session_of_node(conn, node_id)?,
        None => None,
    };
    Ok(SessionInfo {
        id: row.0.parse()?,
        label: row.1,
        title: session_title(conn, head)?,
        created_at: parse_timestamp(&row.2)?,
        nodes: row.4 as usize,
        tokens: row.5 as usize,
        head,
        forked_from,
        forked_from_session,
    })
}

/// The session that owns `node_id`, if the node still exists.
///
/// A missing node is `None`, not an error: a session list must survive a
/// `forked_from` that points at a row no longer present.
fn session_of_node(conn: &Connection, node_id: NodeId) -> Result<Option<SessionId>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT session_id FROM nodes WHERE id = ?1",
            params![node_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?;
    id.map(|id| id.parse::<SessionId>()).transpose()
}

/// The session's first user turn, read from the chain the head sits on.
///
/// The head is used rather than the session's own rows so a branch reports the
/// turn it inherited: that is the message a reader sees when they open it.
fn session_title(conn: &Connection, head: Option<NodeId>) -> Result<Option<String>> {
    let Some(head) = head else {
        return Ok(None);
    };
    for node in ancestors(conn, head)? {
        if node.kind == NodeKind::Turn && node.role == Some(Role::User) {
            return Ok(preview(&node.content));
        }
    }
    Ok(None)
}

/// Whitespace-collapsed, length-capped preview.
///
/// Truncation counts characters, not bytes, so a multi-byte message is never
/// cut mid-character.
fn preview(content: &str) -> Option<String> {
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }

    let mut chars = collapsed.chars();
    let head: String = chars.by_ref().take(TITLE_PREVIEW_CHARS).collect();
    Some(if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    })
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(db_err)?;

    // Needs `query_row`: `PRAGMA journal_mode` returns the mode it settled on, so
    // `execute` would refuse the statement. An in-memory database legitimately
    // answers "memory" here, which is a downgrade rather than an error.
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .map_err(db_err)?;
    if !mode.eq_ignore_ascii_case("wal") {
        tracing::debug!(mode = %mode, "journal mode is not WAL");
    }

    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY;")
        .map_err(db_err)?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).map_err(db_err)?;

    // Repair pass: an external-content FTS index is only as good as its
    // triggers. If rows ever outnumber indexed documents (a database written by
    // an older schema, or a failed trigger), rebuild once so keyword search
    // cannot silently miss nodes.
    let total = match count(conn, "SELECT count(*) FROM nodes") {
        Ok(total) => total as i64,
        Err(_) => return Ok(()),
    };
    let indexed = match conn.query_row("SELECT count(*) FROM nodes_fts", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(indexed) => indexed,
        Err(_) => total,
    };
    if indexed != total {
        conn.execute("INSERT INTO nodes_fts(nodes_fts) VALUES ('rebuild')", [])
            .map_err(db_err)?;
    }
    Ok(())
}

fn stored_embedding_dim(conn: &Connection) -> Result<Option<usize>> {
    let dim: Option<i64> = conn
        .query_row("SELECT dim FROM embeddings LIMIT 1", [], |row| row.get(0))
        .optional()
        .map_err(db_err)?;
    Ok(dim.map(|dim| dim as usize))
}

fn db_err(err: rusqlite::Error) -> HarnessError {
    HarnessError::Memory(err.to_string())
}

fn count(conn: &Connection, sql: &str) -> Result<usize> {
    let value: i64 = conn.query_row(sql, [], |row| row.get(0)).map_err(db_err)?;
    Ok(value.max(0) as usize)
}

fn format_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|err| HarnessError::Memory(format!("invalid timestamp `{value}`: {err}")))
}

fn parse_role(value: &str) -> Result<Role> {
    match value {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(HarnessError::Memory(format!("unknown role `{other}`"))),
    }
}

fn parse_kind(value: &str) -> Result<NodeKind> {
    match value {
        "turn" => Ok(NodeKind::Turn),
        "snapshot" => Ok(NodeKind::Snapshot),
        other => Err(HarnessError::Memory(format!("unknown node kind `{other}`"))),
    }
}

struct RawNode {
    id: String,
    session_id: String,
    parent_id: Option<String>,
    kind: String,
    role: Option<String>,
    content: String,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
    label: Option<String>,
    tokens: i64,
    trimmed: i64,
    created_at: String,
}

impl RawNode {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            session_id: row.get(1)?,
            parent_id: row.get(2)?,
            kind: row.get(3)?,
            role: row.get(4)?,
            content: row.get(5)?,
            tool_calls: row.get(6)?,
            tool_call_id: row.get(7)?,
            label: row.get(8)?,
            tokens: row.get(9)?,
            trimmed: row.get(10)?,
            created_at: row.get(11)?,
        })
    }

    fn into_node(self) -> Result<Node> {
        let tool_calls = match self.tool_calls.as_deref() {
            Some(json) if !json.is_empty() => {
                serde_json::from_str::<Vec<ToolCall>>(json).map_err(|err| {
                    HarnessError::Memory(format!("node {} has invalid tool_calls: {err}", self.id))
                })?
            }
            _ => Vec::new(),
        };

        Ok(Node {
            id: self.id.parse()?,
            session_id: self.session_id.parse()?,
            parent_id: self.parent_id.map(|id| id.parse::<NodeId>()).transpose()?,
            kind: parse_kind(&self.kind)?,
            role: self.role.as_deref().map(parse_role).transpose()?,
            content: self.content,
            tool_calls,
            tool_call_id: self.tool_call_id,
            label: self.label,
            tokens: self.tokens.max(0) as usize,
            trimmed: self.trimmed != 0,
            created_at: parse_timestamp(&self.created_at)?,
        })
    }
}

const NODE_COLUMNS: &str = "id, session_id, parent_id, kind, role, content, tool_calls, \
     tool_call_id, label, tokens, trimmed, created_at";

struct NewNode<'a> {
    id: NodeId,
    session_id: SessionId,
    parent_id: Option<NodeId>,
    kind: NodeKind,
    role: Option<Role>,
    content: &'a str,
    tool_calls: &'a [ToolCall],
    tool_call_id: Option<&'a str>,
    label: Option<&'a str>,
    tokens: usize,
    trimmed: bool,
    created_at: DateTime<Utc>,
}

fn insert_session(
    conn: &Connection,
    label: Option<&str>,
    forked_from: Option<NodeId>,
) -> Result<SessionId> {
    let session_id = SessionId::new();
    conn.execute(
        "INSERT INTO sessions (id, label, created_at, forked_from) VALUES (?1, ?2, ?3, ?4)",
        params![
            session_id.to_string(),
            label,
            format_timestamp(Utc::now()),
            forked_from.map(|id| id.to_string()),
        ],
    )
    .map_err(db_err)?;
    Ok(session_id)
}

fn insert_node(conn: &Connection, node: &NewNode<'_>) -> Result<()> {
    let tool_calls = if node.tool_calls.is_empty() {
        None
    } else {
        Some(serde_json::to_string(node.tool_calls)?)
    };
    let kind = match node.kind {
        NodeKind::Turn => "turn",
        NodeKind::Snapshot => "snapshot",
    };

    conn.execute(
        "INSERT INTO nodes (id, session_id, parent_id, kind, role, content, tool_calls, \
         tool_call_id, label, tokens, trimmed, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            node.id.to_string(),
            node.session_id.to_string(),
            node.parent_id.map(|id| id.to_string()),
            kind,
            node.role.map(|role| role.as_str().to_string()),
            node.content,
            tool_calls,
            node.tool_call_id,
            node.label,
            node.tokens as i64,
            i64::from(node.trimmed),
            format_timestamp(node.created_at),
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

fn write_node_content(
    conn: &Connection,
    node_id: NodeId,
    content: &str,
    trimmed: bool,
    estimator: &HeuristicEstimator,
) -> Result<()> {
    conn.execute(
        "UPDATE nodes SET content = ?2, tokens = ?3, trimmed = ?4 WHERE id = ?1",
        params![
            node_id.to_string(),
            content,
            estimator.estimate(content) as i64,
            i64::from(trimmed),
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

fn session_exists(conn: &Connection, session_id: SessionId) -> Result<bool> {
    let found: Option<String> = conn
        .query_row(
            "SELECT id FROM sessions WHERE id = ?1",
            params![session_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?;
    Ok(found.is_some())
}

/// The session's current head: the newest node by `(created_at, rowid)`.
///
/// `rowid` is the tie-break rather than the node id. ULIDs are only ordered to
/// the millisecond, so two appends in the same millisecond would otherwise sort
/// by their random low bits and hand back the wrong head.
fn head_of(conn: &Connection, session_id: SessionId) -> Result<Option<NodeId>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM nodes WHERE session_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![session_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?;
    id.map(|id| id.parse::<NodeId>()).transpose()
}

fn node_by_id(conn: &Connection, node_id: NodeId) -> Result<Option<Node>> {
    let raw: Option<RawNode> = conn
        .query_row(
            &format!("SELECT {NODE_COLUMNS} FROM nodes WHERE id = ?1"),
            params![node_id.to_string()],
            RawNode::from_row,
        )
        .optional()
        .map_err(db_err)?;
    raw.map(RawNode::into_node).transpose()
}

/// Root-first chain ending at `node_id`, snapshots included.
fn ancestors(conn: &Connection, node_id: NodeId) -> Result<Vec<Node>> {
    let mut chain: Vec<Node> = Vec::new();
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut cursor = Some(node_id);

    while let Some(current) = cursor {
        if !seen.insert(current) {
            return Err(HarnessError::Memory(format!(
                "node {current} is part of a parent cycle"
            )));
        }
        let Some(node) = node_by_id(conn, current)? else {
            break;
        };
        cursor = node.parent_id;
        chain.push(node);
    }

    chain.reverse();
    Ok(chain)
}

fn session_nodes(
    conn: &Connection,
    session_id: SessionId,
    turns_only: bool,
) -> Result<Vec<RawNode>> {
    let filter = if turns_only { " AND kind = 'turn'" } else { "" };
    let mut statement = conn
        .prepare(&format!(
            "SELECT {NODE_COLUMNS} FROM nodes WHERE session_id = ?1{filter} ORDER BY created_at, rowid"
        ))
        .map_err(db_err)?;
    let rows = statement
        .query_map(params![session_id.to_string()], RawNode::from_row)
        .map_err(db_err)?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row.map_err(db_err)?);
    }
    Ok(nodes)
}

fn session_tokens(conn: &Connection, session_id: SessionId) -> Result<usize> {
    let tokens: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(tokens), 0) FROM nodes WHERE session_id = ?1",
            params![session_id.to_string()],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(tokens.max(0) as usize)
}

/// Copies `body` into `blobs` if the node does not have a row yet.
///
/// Deliberately an `INSERT … DO NOTHING`: a node whose content is already a
/// reference must never overwrite the original it points away from.
fn store_blob(conn: &Connection, node_id: NodeId, body: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO blobs (node_id, bytes, sha256, body) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(node_id) DO NOTHING",
        params![
            node_id.to_string(),
            body.len() as i64,
            sha256_hex(body.as_bytes()),
            body,
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

/// Phase 3 — reference compaction.
///
/// Byte-identical bodies keep one row; the later rows lose their `body` but keep
/// the digest, which is what `raw_output` follows to reach the survivor.
fn compact_blobs(conn: &Connection, session_id: SessionId) -> Result<()> {
    let mut statement = conn
        .prepare(
            "SELECT b.node_id, b.sha256 FROM blobs b JOIN nodes n ON n.id = b.node_id \
             WHERE n.session_id = ?1 AND b.body IS NOT NULL ORDER BY n.created_at, n.rowid",
        )
        .map_err(db_err)?;
    let rows = statement
        .query_map(params![session_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(db_err)?;

    let mut canonical: HashMap<String, String> = HashMap::new();
    let mut duplicates: Vec<String> = Vec::new();
    for row in rows {
        let (node_id, digest) = row.map_err(db_err)?;
        match canonical.entry(digest) {
            std::collections::hash_map::Entry::Occupied(_) => duplicates.push(node_id),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(node_id);
            }
        }
    }

    for node_id in duplicates {
        conn.execute(
            "UPDATE blobs SET body = NULL WHERE node_id = ?1",
            params![node_id],
        )
        .map_err(db_err)?;
    }
    Ok(())
}

fn raw_output_of(conn: &Connection, node_id: NodeId) -> Result<Option<String>> {
    let row: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT body, sha256 FROM blobs WHERE node_id = ?1",
            params![node_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(db_err)?;

    match row {
        None => Ok(None),
        Some((Some(body), _)) => Ok(Some(body)),
        Some((None, digest)) => {
            let body: Option<String> = conn
                .query_row(
                    "SELECT body FROM blobs WHERE sha256 = ?1 AND body IS NOT NULL LIMIT 1",
                    params![digest],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_err)?;
            Ok(body)
        }
    }
}

fn store_embedding(conn: &Connection, node_id: NodeId, vector: &[f32]) -> Result<()> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    conn.execute(
        "INSERT INTO embeddings (node_id, dim, vector) VALUES (?1, ?2, ?3) \
         ON CONFLICT(node_id) DO UPDATE SET dim = excluded.dim, vector = excluded.vector",
        params![node_id.to_string(), vector.len() as i64, bytes],
    )
    .map_err(db_err)?;
    Ok(())
}

fn load_embeddings(conn: &Connection) -> Result<Vec<(NodeId, Vec<f32>)>> {
    let mut statement = conn
        .prepare("SELECT node_id, dim, vector FROM embeddings")
        .map_err(db_err)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(db_err)?;

    let mut vectors = Vec::new();
    for row in rows {
        let (node_id, dim, bytes) = row.map_err(db_err)?;
        vectors.push((
            node_id.parse::<NodeId>()?,
            decode_vector(&bytes, dim as usize),
        ));
    }
    Ok(vectors)
}

/// `f32` little-endian bytes, as written by [`store_embedding`].
fn decode_vector(bytes: &[u8], dim: usize) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .take(dim)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}
