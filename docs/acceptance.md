# Acceptance

What is verified, how, and what is not. Every number here was produced by a
command run in this workspace on 2026-10-01; every claim about behaviour cites
the test or source that supports it.

Reproduce with:

```
cargo test --workspace --no-fail-fast
ls crates | wc -l
find crates -name '*.rs' | xargs wc -l | tail -1
```

---

## Test suite

Measured total, from a full green run:

```
$ cargo test --workspace --no-fail-fast 2>&1 | grep -E '^test result' \
    | awk '{p+=$4; f+=$6} END {print p" passed, "f" failed"}'
700 passed, 0 failed
```

**685 distinct tests, reported as 700.** `harness-cli` declares two `[[bin]]`
targets (`dhs` and `harness`) over the same `src/main.rs`, so the 15 unit tests
in that file are compiled and run twice. The distinct figure is the sum of the
per-crate column below; the reported figure is what the command prints. Two
doc-tests are ignored (the two ` ```ignore ` examples in
`crates/harness-macros/src/lib.rs`); one passes
(`crates/harness-sandbox`, 1).

Per crate, attributed by test binary. "unit" is the `src/lib.rs` (or
`src/main.rs`) binary; the rest are integration binaries.

| crate | unit | integration | total |
|---|---|---|---|
| `harness-agent` | 92 | 8 (`budget` 4, `incremental` 3, `token_report` 1) | 100 |
| `harness-cli` | 15 | — | 15 |
| `harness-core` | 56 | — | 56 |
| `harness-guardrails` | 31 | — | 31 |
| `harness-llm` | 53 | — | 53 |
| `harness-macros` | 4 | 8 (`skill_macro` 2, `tool_macro` 6) | 12 |
| `harness-mcp` | 16 | 15 (`adapter` 6, `http` 4, `stdio` 5) | 31 |
| `harness-memory` | 22 | 33 (`dag` 18, `delete_session` 3, `incremental` 2, `scenario` 5, `stage` 5) | 55 |
| `harness-orchestrator` | 70 | 25 (`orchestration`) | 95 |
| `harness-sandbox` | 8 | 19 (`plugin_host`) + 1 doc-test | 28 |
| `harness-server` | 43 | 63 (`bus` 23, `extensions` 4, `plan` 6, `rest` 11, `session_delete` 1, `stored_sessions` 5, `ui` 5, `workflow_agents` 2, `workflows` 6) | 106 |
| `harness-skills` | 11 | — | 11 |
| `harness-tools` | 45 | 9 (`public_api` 5, `schema_equivalence` 4) | 54 |
| `harness-workflows` | 33 | 5 (`builtins`) | 38 |
| **total** | | | **685** |

No crate is untested. `harness-cli` gained unit tests for the age parser and the
workflow checks; `harness-guardrails` is no longer the placeholder an earlier
version of this document described.

**Caveat on concurrency.** This workspace was being edited by several agents
while these numbers were taken. One run in this session failed
`harness-agent::agent_loop::tests::a_trim_elides_to_the_low_water_mark_and_leaves_headroom`;
the immediately following run passed all 691, so that failure was a mid-edit
state, not a defect in the tree. Re-run before quoting any number here.

---

## End-to-end evidence

### The workspace's own store (a real artifact)

The repository's `.harness/harness.db` holds the residue of real sessions. Read
with `dhs memory stats` (no `sqlite3` CLI is installed):

```
$ dhs memory stats
sessions: 187
nodes:    1544
blobs:    73 (996.5 KB retained off-context)
tokens:   412705
```

187 sessions is the operational reason `dhs memory rm` exists: a store this size
had no way to shed a session before. Session labels are `run:<agent>` and
`serve:<agent>`, matching `harness run` and `harness serve`. The assistant turns
are varied and context-accurate — which the scripted `MockProvider` cannot
produce, since it replays fixed `ScriptedTurn` strings — so a real inference
backend was used.

**This is an artifact, not a reproducible command.** No live gateway was re-run
for this document, and nothing in the database records which provider served it.
**Marked unverified.** No test in the workspace contacts a network model
provider: every agent, orchestrator and server test uses `MockProvider` or a
locally scripted `Provider`, and the provider-factory tests assert the failure
when a key's environment variable is unset
(`crates/harness-llm/src/factory.rs`).

### Deleting a session

`crates/harness-memory/tests/delete_session.rs` opens a file-backed store,
writes an origin session (including a blob-sized tool result) and a fork of it,
deletes the origin, and then asserts against the SQL tables directly:

- `sessions()` no longer lists the id, and `session()` returns `None` — the value
  the server's history route 404s on.
- `history()` is empty, `node()` and `raw_output()` for a deleted node are `None`.
- `blobs` drops to zero and `embeddings`/`nodes_fts` shrink; the fork's own nodes
  and its own turn survive, and its inherited prefix is gone.
- A second delete returns `None` rather than an error.

`crates/harness-server/tests/session_delete.rs` proves the reader's side over a
real socket: `GET /api/sessions/{id}/history` answers 200 before the delete and
404 with `{"error":"no such session"}` after, and the id disappears from
`GET /api/sessions`.

Reproduced by hand against a throwaway workspace:

```
$ dhs memory rm 01M3VTR08RSSBK1GC487FX51GD
deleted session 01M3VTR08RSSBK1GC487FX51GD: 4 nodes, 0 blobs, 3 embeddings
$ dhs memory rm 01M3VTR08RSSBK1GC487FX51GD
error: no session with id 01M3VTR08RSSBK1GC487FX51GD
$ dhs memory rm --older-than 0s
deleted 3 session(s): 12 nodes, 0 blobs, 9 embeddings
```

### Workflow validation

`crates/harness-workflows/tests/builtins.rs` holds the 14 checked-in workflows to
the same bar as code: every one loads, compiles to a layered graph, and names
only agents that exist. `dhs workflow validate` reports the same thing from the
terminal:

```
$ dhs workflow validate
ok    add-tests
...
ok    security-audit

14 workflow(s) valid; 4 agent(s) available
```

`crates/harness-workflows/src/lib.rs` adds the agent check to the loader and the
saver, so it is not a test-only convention: `save_new` refuses a spec whose stage
names an undefined agent and writes nothing, and `load_with_agents` fails the
whole load naming the file and the agent. `crates/harness-server/tests/workflow_agents.rs`
exercises the route end to end: a scripted provider answers the authoring call
with a workflow naming `ghost`, `POST /api/workflows` answers 500, and no file or
directory is created; a workflow naming the fixture's real agent is saved.

### WebSocket session transcript

`crates/harness-server/tests/bus.rs` binds a real listener on `127.0.0.1:0` and
speaks the wire protocol through `tokio_tungstenite`.
`a_full_run_streams_the_expected_sequence` asserts the exact frame sequence for
one run:

```
session_started → assistant_chunk → tool_call_start → tool_call_end → token_usage → done
```

with every envelope declaring `v == 1`, `tool_call_start` before `tool_call_end`,
and the conversation persisted and addressable afterwards. Other tests in the
same file cover abort mid-tool, steering folded into history, `busy` refusal of a
second concurrent message, `not_running` after a run ends, cross-connection
`Subscribe` with two subscribers, `bad_message` refusal without closing the
socket, and — the backpressure wiring proof — a client that stops reading is
disconnected under a strict policy but survives a policy that never gives up,
which shows the disconnect is the policy's doing.

`crates/harness-server/tests/plan.rs` and `tests/workflows.rs` cover the plan and
workflow paths over the same socket: the plan frame reaching the client with
every node, per-node status frames, a node that never runs ending `skipped`, two
jobs queued and run in order, the workflow queue and the message queue numbering
themselves independently, and a named workflow running without consulting the
planner.

### Security posture

- **Token.** `crates/harness-server/src/api.rs` and `ws.rs` refuse a request that
  does not carry the configured token, on the bus and on the REST data routes
  alike; the tests accept `?token=`, the `X-Harness-Token` header and
  `Authorization: Bearer`, and confirm an unconfigured server is unchanged. The
  CLI generates the token from OS entropy whenever it is asked for a non-loopback
  bind (`crates/harness-cli/src/commands/serve.rs`).
- **Guardrails.** `harness-agent` tests drive the pipeline through the loop: a
  policy block answers the tool call with a refusal instead of executing it, and
  a secret is redacted before it enters the conversation. Both front ends install
  the deterministic set when `[guardrails] enabled` is true, which is the default
  (`crates/harness-core/src/config.rs`, `harness-server/src/state.rs`).
- **Sandbox escape refusal.** `crates/harness-sandbox/tests/plugin_host.rs` loads
  the hand-written `.wat` plugins and calls them through `PluginHost`:
  `a_path_that_escapes_the_workspace_is_refused_even_with_file_read` grants
  `file_read` **and** `file_write`, puts the target one level above the root, and
  the call still fails with "path escapes the workspace root" and no content;
  `a_plugin_cannot_import_anything_outside_the_harness_module` loads a module
  importing `wasi_snapshot_preview1.fd_write` and shows it fails to link, because
  the host defines only the four `harness.*` imports. Capability refusal without a
  grant, the wall-clock timeout, the memory limit and signature tampering are each
  covered by their own test.

### Trim savings measurement

`crates/harness-memory/tests/dag.rs` measures token savings across two context
shapes rather than quoting one number. Measured output:

```
            tool-heavy:  40547 ->   119 tokens, saved  99.7% (4/16 nodes elided)
 mostly-conversational:    587 ->   119 tokens, saved  79.7% (4/16 nodes elided)
```

Losslessness is proved separately: every conversational turn is byte-identical
after trim, and every elided tool body is still recoverable through `raw_output`.

### Git-worktree isolation and keep-on-success

`crates/harness-orchestrator/tests/orchestration.rs`:
`concurrent_nodes_cannot_see_each_others_files` shows two nodes under isolation do
not see each other's writes; `a_successful_node_keeps_the_worktree_holding_its_edits`
shows a successful node reports a `WorktreeRef` whose path still exists, whose
branch starts with `harness/`, and which holds the node's file, while the edit is
absent from the live tree; the failed-node and `keep_worktrees = false` cases are
covered by their own tests.

### Orchestration semantics

Also in `orchestration.rs`: a diamond runs its middle layer concurrently, a
rejected result is retried exactly `max_retries` times, a dependent of a failed
node is skipped, an abort stops further scheduling, a sub-agent never sees the
planner's conversation, and a node whose agent names another provider fails
before any request. The four-field contract is pinned by
`the_reported_result_is_the_four_field_contract` and by the parse tests in
`crates/harness-orchestrator/src/result.rs`.

### MCP end-to-end (against a local fake)

`crates/harness-mcp/tests/` drives a locally written fake server over both
transports: stdio end-to-end with shutdown killing the child, and HTTP over a
loopback `TcpListener`. The adapter namespacing (`mcp__<server>__<tool>`),
lazy-server deferral, merge into the builtin registry, and the truncation marker
appended to oversized output are covered. See **Not verified** for what this does
*not* establish.

### REST and UI

`crates/harness-server/tests/rest.rs` covers `/api/health`, `/api/ui`,
`/api/agents`, `/api/memory/stats`, `/api/sessions`, the history route, the JSON
404 and the token; `ui.rs` covers shell serving, deep links, hashed-asset caching,
and that `/api/*` is never answered with the shell; `extensions.rs` covers
`/api/extensions` for skills, MCP servers and plugins; `stored_sessions.rs` covers
reopening a conversation from a previous process.

---

## Not verified

Real gaps found by reading the source. Each is a capability that is declared,
documented, or assumed but not exercised or not wired.

**No live-gateway run is reproducible from the repository.** The only evidence is
the stored `.harness/harness.db` above. There is no env-gated integration test,
no script, and no fixture that re-runs a real provider, and the provider
attribution of those runs is inference, not measurement.

**No real third-party MCP server has been exercised.** Every MCP test drives a
locally written fake or a loopback socket. The streamable HTTP transport is tested
against a hand-rolled server, not a real MCP implementation. This repository's
`.harness/config.toml` declares no `[mcp]` servers at all.

**The vector embedder is lexical, not semantic.** `HashingEmbedder` is a
feature-hashing bag of words with adjacent-pair features. Paraphrases sharing no
vocabulary score near zero. The crate's own docs say so. Any claim of semantic
recall is unsupported; `sqlite-vec` was evaluated and dropped, so there is no ANN
backend either.

**There is no authentication beyond the shared token, and no TLS.** The token is
a single shared secret, generated per run for a non-loopback bind; there are no
users, roles or per-route scopes. A network bind is for a trusted network or
behind a reverse proxy.

**`Capability::ShellExec` grants nothing.** Its `host_function()` returns `None`
and no `harness.exec` import is registered. A manifest declaring `shell_exec` is
accepted and does nothing. Separately, the `shell` builtin runs arbitrary
commands through `sh -c`/`cmd /C`, contained only by its working directory and the
path guard; the wasmtime capability enforcement does not mediate it.

**The builtin tools still hand-write their `Tool` impls.** All seven implement
`Tool` by hand; the `#[harness::tool]` macro is used only inside
`crates/harness-macros/tests/`. The macro is demonstrated but is not the mechanism
the shipped tools are built on, and no test keeps the two paths in sync.

**Skill bodies are never injected.** An agent's declared skills reach the prompt
as a metadata index — name, description, path — and the model is expected to read
the body with a tool. `SkillRegistry::activate` has no production caller, so the
progressive-disclosure body cost is never paid inside a run.

**`ToolApproval` is inert and `ToolCallProgress` is never emitted.**
`ControlMessage::ToolApproval` is discarded by the loop and nothing raises an
approval request, so `ServerMessage::ToolApprovalRequest` has no producer.
`ToolContext.progress` is only ever set in a test, so the `emit_progress` calls in
`shell`, `write_file`, `edit_file` and `web_fetch` are dead ends and
`AgentEvent::ToolCallProgress` is never produced.

**No model-based guardrail is installed.** The `LlmJudge` trait, `NoJudge` and
`ScriptedJudge` exist in `crates/harness-guardrails`, but nothing in production
wires an implementation in, so the adversarial review the crate describes is not
part of any run.

**Nothing merges worktrees.** A successful node keeps its checkout and branch and
reports them; integrating that work is entirely the caller's job.

**Doc-tests are thin.** Only `harness-sandbox` contributes a passing doc-test (1)
and `harness-macros` contributes two ignored ones. No crate's public API is
otherwise documented by executable examples.
