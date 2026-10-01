# Architecture

The harness is a Cargo workspace (`Cargo.toml`, `members = ["crates/*"]`) of 14
crates. Measured size:

```
$ ls crates | wc -l
14
$ find crates -name '*.rs' | xargs wc -l | tail -1
  44812 total
```

Five layers, each a set of crates. Citations name the file and the symbol; line
numbers are deliberately not quoted, because several of these files are edited
concurrently and a stale line number is worse than none. Anything not checkable
from source is called out as such.

| Layer | Crates | Entry type |
|---|---|---|
| Storage | `harness-memory` | `SqliteMemory` |
| Kernel | `harness-agent`, `harness-tools`, `harness-llm` | `AgentLoop`, `ToolRegistry`, `Provider` |
| Orchestration | `harness-orchestrator`, `harness-workflows` | `Planner` / `Executor` / `Verifier`, `WorkflowRegistry` |
| Protocol | `harness-core` (event vocabulary), `harness-server` | `AgentEvent` / `ServerEnvelope`, `Connection` |
| Extension system | `harness-skills`, `harness-macros`, `harness-mcp`, `harness-sandbox`, `harness-guardrails` | `SkillRegistry`, `#[harness::tool]`, `McpRegistry`, `PluginHost`, `GuardrailPipeline` |

---

## 1. Storage layer — `harness-memory`

Key type: `SqliteMemory` (`crates/harness-memory/src/store.rs`), implementing the
`Memory` trait declared in `crates/harness-core/src/memory.rs`.

A conversation is a DAG of immutable nodes, not a flat log. Schema
(`store.rs::SCHEMA`):

| table | role |
|---|---|
| `sessions` | one row per conversation, with `forked_from` |
| `nodes` | one row per turn or snapshot marker: `parent_id`, `kind`, `role`, `content`, `tool_calls`, `tokens`, `trimmed` |
| `blobs` | oversized bodies parked by trim, keyed by `node_id`, with a `sha256` digest and a nullable `body` |
| `embeddings` | `(node_id, dim, vector)` — `f32` little-endian bytes |
| `nodes_fts` | FTS5 external-content index over `nodes.content`, kept current by three triggers |

**Snapshot / branch / trim.**

- `snapshot` inserts a `NodeKind::Snapshot` marker whose parent is the current
  head. It is not a conversational turn, so `history` skips it.
- `branch` opens a new session and inserts a `Snapshot` marker whose `parent_id`
  is `from_node`. The fork **inherits** the ancestor chain rather than copying
  it: nodes are immutable, so the parent chain reproduces the original prefix and
  the fork appears as a second child of `from_node`. The doc comment explains why
  a copying fork cannot express this edge.
- `trim` runs three phases over every node except the `keep_recent` tail:
  (1) rewrite tool output at or above `min_tool_output_chars` into an elision
  reference, (2) mechanically strip inline base64 data URIs and long `\uXXXX`
  escape runs (`trim.rs`), (3) compact byte-identical blob bodies onto one
  surviving row by `sha256` (`store.rs::compact_blobs`). The rule stated in
  `trim.rs` is that a rewrite never destroys information, only moves it: every
  large append is copied into `blobs` at append time when it reaches
  `BLOB_THRESHOLD_CHARS` (4 KiB), so trim only flips the node's content to a
  reference. `raw_output` follows the digest back to the survivor.

**Delete.** `delete_session` (`store.rs::delete_session_locked`) removes a
session's own rows in one transaction: its `blobs` and `embeddings` first — they
are keyed by `node_id`, so they must go while the nodes still exist to select on
— then its `nodes`, which fires the FTS delete trigger once per row, then the
`sessions` row. Forks are **not** cascaded: a session forked from the deleted one
is a separate conversation with its own nodes, and its history simply loses the
prefix it inherited. The `SessionDeleteReport` counts the rows removed and the
forks left behind, so `dhs memory rm` can warn.

**Recall.** `recall` runs two retrievers and fuses them with Reciprocal Rank
Fusion (`index.rs`, `RRF_K = 60`): an FTS5 keyword ranking (`recall.rs`, every
term quoted and OR-ed) and a vector ranking. Each retriever contributes
`limit * CANDIDATE_FANOUT` candidates; hits are clipped to `SNIPPET_CHARS` (400)
and cut to the caller's `max_tokens`. The index is rebuilt from the `embeddings`
table per query (`BruteForceIndex::from_rows`) so it cannot go stale across a
restart.

**The embedder is lexical, not semantic.** `HashingEmbedder` (`vector.rs`) is a
feature-hashing bag of words: lowercased tokens plus weighted adjacent pairs
hashed into 256 buckets by FNV-1a, L2-normalised. Its own doc comment says
retrieval "behaves like a fuzzy keyword match: two texts that share vocabulary
score alike, while a paraphrase that shares no words scores near zero."
`sqlite-vec` was evaluated and dropped (`lib.rs`); `BruteForceIndex` is an exact
cosine scan.

**Limitation.** Recall quality is bounded by the lexical embedder above — there
is no semantic model in the offline path. Operationally, every read and write
goes through one `parking_lot::Mutex<Connection>` (`store.rs`); the doc comment
concedes this is the first thing to revisit if memory access shows up in a
profile.

---

## 2. Kernel — `harness-agent`, `harness-tools`, `harness-llm`

Key type: `AgentLoop` (`crates/harness-agent/src/agent_loop.rs`).

**ReAct loop.** `AgentLoop::run` repeats: stream a model response, append it to
history, and if it requested tools, run them and feed the results back. It stops
on `EndTurn`, `MaxIterations`, `Aborted`, or `BudgetExceeded` (`CompletionReason`,
`crates/harness-core/src/event.rs`). The loop is interruptible at exactly two
points — mid-stream and mid-tool-batch. Tools in one turn run concurrently
through a `JoinSet` and their results are re-sorted into call order. An abort
signals the shared `AbortFlag` first, then gives tools an `ABORT_GRACE` to clean
up children before dropping futures.

**Control channel.** `ControlHandle` / `ControlChannel` (`control.rs`) carry
three messages: `Steering { text, priority }`, `Abort { reason }`, and
`ToolApproval { tool_call_id, approved, reason }`. Steering is folded in at a
turn boundary, never underneath an in-flight request. `ToolApproval` is still
discarded by the loop and has no producer (see the cross-cutting table).

**Token budget with 80 % degradation.** `TokenBudget` (`budget.rs`) is
`Arc`-shared atomics. `DEFAULT_DEGRADE_AT = 0.8`: crossing 80 % of the ceiling
arms a take-once latch (`should_degrade`), and the loop then switches to
`fallback_model` and emits one `AgentEvent::Guardrail`. `max_tokens == 0` means
unbounded, not zero. `max_iterations` defaults to 64
(`HarnessSection::default`); the in-flight conversation is trimmed past
`[context] trim_threshold_chars`. Both ceilings end the run with one tool-free
wrap-up turn — `WRAP_UP_INSTRUCTION` for a spent budget,
`MAX_ITERATIONS_INSTRUCTION` for the iteration limit — so the run answers from
the work already done instead of ending with an empty result.

**Deferred tool schemas.** `AgentConfig::deferred_tools` (default true) makes the
request carry `ToolRegistry::index_spec()` — one `request_tools` entry naming
every tool with a one-line summary — instead of every schema. The loop answers a
`request_tools` call itself (`AgentLoop::activate_tools`), returning the named
tools' full schemas and advertising them for the rest of the run. `request_tools`
is not a registered `Tool`; its name is the shared const
`harness_tools::REQUEST_TOOLS`.

**Guardrails.** `AgentLoop::with_guardrails` attaches a
`harness_guardrails::GuardrailPipeline`; when it is present the loop inspects the
user message, the assistant output, every tool call and every tool result. Both
front ends install the same pipeline when `[guardrails] enabled` is true (the
default) — see §5.

**Skills.** An agent's declared skills become a metadata index — name,
description and the `SKILL.md` path — assembled into the system prompt by
`prompt::assemble` from `agent_loop.rs::skill_index`. Only the index is injected;
`SkillRegistry::activate` (the full body) still has no caller, so the model is
told where a body lives and reads it with a tool if it wants it.

**Tools.** `Tool` trait and `ToolRegistry` (`crates/harness-tools/src/lib.rs`).
Seven builtins (`builtin/mod.rs`): `edit_file`, `grep`, `list_dir`, `read_file`,
`shell`, `web_fetch`, `write_file`. Every filesystem path a builtin touches goes
through `ToolContext::resolve`, which delegates to
`harness_core::path::resolve_within` and refuses anything outside the workspace
root. `shell` merges stdout/stderr, enforces a timeout, and on Windows kills the
whole process tree with `taskkill /T`. Oversized output is cut to
`ToolsConfig::max_output_bytes` and the cut is announced with a
`truncation_marker` appended to the content, not only recorded in metadata,
because the loop forwards only `content` to the model.

**Providers.** `Provider` trait (`crates/harness-llm/src/provider.rs`), one
streaming client `OpenAiProvider` for any OpenAI-compatible
`/chat/completions` endpoint, and `MockProvider` (`mock.rs`) which replays a
script. `from_config` (`factory.rs`) is the single place a provider id becomes a
backend; it refuses a remote endpoint with no key, naming the environment
variable to export, and leaves a keyless loopback endpoint alone.

**Limitations.**
- `ControlMessage::ToolApproval` is inert: the loop discards it and nothing in
  the tree raises an approval request, so `ServerMessage::ToolApprovalRequest`
  has no producer.
- `AgentEvent::ToolCallProgress` is defined and converted to a wire message but
  never emitted: `ToolContext::progress` is only ever set in a test
  (`crates/harness-tools/tests/public_api.rs`), so the `emit_progress` calls in
  `shell`, `write_file`, `edit_file` and `web_fetch` go nowhere.
- The seven builtin tools hand-write `impl Tool for …`; `#[harness::tool]` is not
  used for any of them (see §5).

---

## 3. Orchestration layer — `harness-orchestrator` + `harness-workflows`

Key types: `Planner`, `Executor`, `Verifier`
(`crates/harness-orchestrator/src/{planner,executor,verify}.rs`) and
`WorkflowRegistry` (`crates/harness-workflows/src/lib.rs`).

**Planner.** `Planner::plan` asks the model for one JSON object describing a
`TaskGraph` (`graph.rs`). The request carries **no tools**. A reply that cannot
be read as a DAG is retried exactly once with the parse error fed back, then the
planner fails quoting the model. The graph is validated for cycles before any
node runs.

**Executor.** `Executor::execute` validates the graph, takes its topological
`layers()`, and runs each layer concurrently under a `Semaphore` of
`max_parallel` (default 4). Two rules from the module comment: a sub-agent starts
clean — its history is its objective, the four-field instruction, its
dependencies' states, and a recall summary, never the planner's conversation;
and a failure is local — a failed node does not abort its siblings, dependents
are reported `Skipped`. A node whose agent spec resolves its model to a different
provider than the one executor holds is refused before spending a token
(`node_provider_matches`).

**The four-field contract.** `SubtaskResult`
(`crates/harness-orchestrator/src/result.rs`) is
`{ objective, state, evidence, boundary }`, parsed with
`#[serde(deny_unknown_fields)]`; a missing, unknown or empty field is an error
naming what was wrong, never a default. The instruction text is a `const` next to
the parser so the two cannot drift.

**Reflexion.** On a parse failure, a verification failure, or a provider error,
the executor retries up to `max_retries` times (default 1), feeding back
`retry_message`: the reason, the previous reply (truncated at
`MAX_QUOTED_REPLY = 2_000`), and the verifier's rendered issues.

**Verifier.** `Verifier::verify` combines two signals. Deterministic checks are
commands run in the node's worktree with a timeout (default 120 s) whose output
is parsed into `error[E…]`/`warning:` lines; an adversarial review asks a
**separate** provider instance to look for what the checks cannot see. `valid` is
the conjunction. The commands come from the node's own `verify` list when it has
one — a workflow stage's declared checks, which outrank the executor-wide
default — and from `VerifierConfig::checks` otherwise. An empty command list is
vacuously true but is never silent: the report carries a warning that nothing was
checked.

**A blocked node is not a success.** `SubtaskResult.state` is free text, and its
first word is the verdict (`done`, `partly done` or `blocked`, per the
instruction). `executor.rs::blocked_state` reads it: a result that reports itself
blocked or partly done fails the node before verification, with no retry, so its
dependents are skipped rather than run on incomplete work. A `done` whose prose
merely mentions a blocker is still done.

**Git worktree isolation, and successful nodes keep their worktree.**
`Worktree::create` runs `git worktree add -b harness/<slug>-<suffix> <path>` under
`$TMPDIR/harness-worktrees`. Creation against one repository is serialised behind
a `Mutex` because `git worktree add` mutates shared `.git/worktrees`. On
completion: a node that **succeeded** with `keep_worktrees` (default `true`)
**keeps** its checkout and branch and reports them as a `WorktreeRef`; a
**failed** node's checkout and branch are removed. Nothing merges automatically —
the caller integrates from the kept path.

**Workflows.** A `<id>.workflow.md` document
(`harness-workflows/src/lib.rs`) is frontmatter (id, name, description, `when`,
stages) plus a Markdown guidance body. `WorkflowSpec::to_graph` compiles the
stages into the same `TaskGraph` the executor already runs and carries the
guidance onto the graph; each stage's `verify` commands travel with it for the
verifier. Discovery mirrors agents and skills: `~/.harness/workflows`, then
`<workspace>/workflows`, then `<workspace>/.harness/workflows`, later roots
overriding by id. The loader is strict: a malformed file, an empty `when` or
description, a broken stage DAG, or a stage `agent:` that no `.agent.md` defines
fails the whole load and names the file. `WorkflowRegistry::load_with_agents` is
the agent-aware form; `save_new` refuses the same way, so `POST /api/workflows`
cannot persist a workflow that could never run. The repository ships 14 built-ins
under `workflows/`.

**How the server reaches it.** `harness-server`'s `ws.rs` spawns a plan
(`spawn_plan`) for a `plan_task` frame and a job (`spawn_job`) for a
`queue_workflow` frame; both build an `Executor` and stream its events. A
`plan_task` always plans freely. A `queue_workflow` asks the installed
`WorkflowSelector` (the CLI's `RegistrySelector`, backed by the same registry the
REST list reads) and falls back to the free-form planner when nothing fits. Each
node's sub-agent forks its own session from the parent, and the server tags every
frame a worker produced with `subagent_id` and `subagent_session_id`
(`ServerEnvelope::from_subagent_session`), so the UI can draw one lane per worker
and open that worker's stored conversation.

**Limitations.**
- Nothing merges worktrees; integration is entirely the caller's job.
- A free-form plan declares no checks: the planner does not emit `verify`
  commands, so such a run is verified by whatever `VerifierConfig` its executor
  was built with (the default checks nothing and says so in the report). Only a
  workflow stage's declared commands run automatically, because
  `WorkflowSpec::to_graph` carries them onto the node.
- `SkillRegistry::activate` still has no production caller: the index names the
  `SKILL.md` and the model reads it with `read_file`, so the body cost is paid by
  a tool call rather than by an activation API.

---

## 4. Protocol layer — `harness-core` + `harness-server`

**Frozen wire protocol.** `crates/harness-core/src/event.rs` defines the whole
vocabulary once. `PROTOCOL_VERSION = 1` and every envelope carries it. Two layers
exist on purpose: `AgentEvent` is the loop's internal stream; `ServerMessage` is
the wire form. Routing context lives in
`ServerEnvelope { v, session_id, agent_id, subagent_id?, subagent_session_id?, #[flatten] message }`
and inbound frames use the mirrored `ClientEnvelope`.

**Connection multiplexing.** `crates/harness-server/src/ws.rs` gives one
connection a reader loop, one writer task, and (per run) a run task plus an event
bridge. A client `Subscribe { session_id }` attaches the connection to a
session's `broadcast` fan-out, so many sockets can watch one session and one
socket can switch sessions. One connection drives at most one run: a second
`UserMessage` while a run is in flight is refused with `busy`. The reader keeps
consuming frames (steering, aborts) while the run task proceeds, because the run
is spawned rather than awaited. Events take one path: the bridge broadcasts onto
the session, and each attached connection's forwarder pulls into that
connection's own bounded queue. A writer heartbeat pings every 15 s.

**Two queues.** A session holds a **user-message queue** and a separate
**workflow-job queue** (`state.rs::SessionRecord`). They are numbered
independently — a client is told "1 message waiting" and "1 workflow queued" as
different things — and a waiting message outranks a waiting job. `queue_workflow`
enqueues a job; the client learns each job's state from `workflow_queued` /
`workflow_started` / `workflow_finished`. `plan_task` is neither queued nor
counted: it claims the session and runs like a message.

**Backpressure.** The policy lives in `backpressure.rs`. A full queue is handled
by `try_send` plus a consecutive-full counter — awaiting a full queue inside the
bridge would let one stalled client freeze a run for every other subscriber:

| consecutive fulls | decision | effect |
|---|---|---|
| `0` | `Overflow::Wait` | stall the bridge at most `wait` once |
| `1..max` | `Overflow::Drop` | drop the frame; other subscribers stay live |
| `>= max` | `Overflow::CloseSubscriber` | close the connection |

Defaults: capacity 256, `max_consecutive_full` 8, `wait` 5 s. The farewell error
is best-effort — the queue was full by definition — so a stalled client usually
only sees the close frame; its session stays readable over REST.

**REST.** `crates/harness-server/src/api.rs::routes`: `/api/health`, `/api/ui`,
`/api/agents`, `/api/extensions`, `/api/workflows` (GET list, POST create),
`/api/sessions`, `/api/sessions/{id}/history`, `/api/memory/stats`, and a
fallback that keeps a JSON 404 for `/api/*` while answering deep links with the
UI shell. `POST /api/workflows` asks the configured model to author a document
and saves it through `WorkflowRegistry::save_new`; a reply that is not a valid,
runnable workflow is refused and nothing is written. An `ApiError` keeps
internals out of the response body — the client gets a fixed sentence and the
detail goes to the log.

**Security posture.** The bus can drive the `shell` tool, so the server treats
reachability as the threat. Two gates stand in front of the WebSocket upgrade:
an `Origin` check, so a page in the user's browser cannot become a proxy for the
attacker, and a shared **token**, which the CLI generates automatically whenever
it is asked for a non-loopback bind (`serve.rs::generate_token`, 32 bytes of OS
entropy). The token protects the REST data routes as well as the bus, and is
accepted as `?token=`, the `X-Harness-Token` header, or `Authorization: Bearer`.
A loopback bind has no token and is unchanged. There is no TLS: a network bind is
for a trusted network or behind a reverse proxy. Separately, the `[guardrails]`
pipeline (§5) is on by default and screens every conversation for credentials,
PII, injected instructions and destructive tool calls.

**Limitations.** `AgentEvent::TurnStarted` is dropped by the bridge because its
wire conversion is a `Pong` and would be indistinguishable from a heartbeat.
`parse_client` special-cases a `subscribe` frame by hand because serde's flatten
deserializer cannot read the duplicated `session_id` key.

---

## 5. Extension system

### Skills — `harness-skills`

`SkillSpec` / `SkillRegistry` (`crates/harness-skills/src/lib.rs`). The crate's
point is progressive disclosure: `metadata_tokens` (name + description) is what
an index costs, while `body_tokens` is paid only on `activate`. Discovery is
`~/.harness/skills`, then `<workspace>/skills`, then `<workspace>/.harness/skills`,
later roots overriding by name. Two file shapes are accepted:
`<dir>/<name>.md` and `<dir>/<name>/SKILL.md`. A malformed file fails the whole
load and names its path.

**Limitation.** The metadata index reaches the prompt (§2), but
`SkillRegistry::activate` — the full body — has no production caller. The model
is told each skill's name, description and path, and reads the body with a tool
if it decides it needs it; nothing injects the body for it.

### Macros — `harness-macros`

`#[harness::tool]` and `#[harness::skill]` (`crates/harness-macros/src/lib.rs`).
`#[tool]` attaches to the **argument struct**, not the handler, because stable
Rust exposes no span-to-source-file API to reach the struct from an attribute on
the function; it derives the JSON Schema from the named fields and resolves the
handler by name. Generated code refers to `::harness_core`, `::harness_tools`,
`::serde_json` and `::async_trait` by absolute path.

**Limitation.** No crate uses either macro outside its own tests: the seven
builtins hand-write `impl Tool`, and `#[harness::skill]` is exercised only in
`crates/harness-macros/tests/`. The macros are a demonstrated capability, not the
mechanism the builtins are built on.

### MCP — `harness-mcp`

`McpServer` / `McpRegistry` (`crates/harness-mcp/src/{server,registry}.rs`). A
remote tool is wrapped as an ordinary `harness_tools::Tool` named
`mcp__<server>__<tool>` (`crates/harness-mcp/src/adapter.rs`), so MCP servers
join the same registry as the builtins and cannot shadow them. Transports are
stdio and streamable HTTP. Connection is deferred: `McpServer::connect` performs
no I/O, the handshake happens on first use, and `lazy` servers are skipped until
asked for. The adapter enforces the same `max_output_bytes` ceiling as the
builtins and appends the same truncation marker; the helper is mirrored locally
because `harness_tools::builtin::truncation_marker` is `pub(crate)`.

**Limitation.** No real third-party MCP server has been exercised. Every MCP
test drives a locally written fake (`crates/harness-mcp/tests/fake_mcp_server.rs`)
or a loopback `TcpListener`, and this repository's own `.harness/config.toml`
declares no `[mcp]` servers.

### Sandbox — `harness-sandbox`

`PluginHost` / `PluginManifest` / `Capability`
(`crates/harness-sandbox/src/{host,manifest}.rs`). Plugins are plain WebAssembly
modules with no WASI; the only imports are four host functions under the
`harness` module, registered in `host.rs`:

| import | capability |
|---|---|
| `harness.log` | none — always allowed |
| `harness.read_file` | `file_read` |
| `harness.write_file` | `file_write` |
| `harness.http_get` | `network_access` |

The manifest is supplied by the caller and re-checked on every host call; a
refused call fails the plugin call rather than returning empty data. Every guest
path goes through `resolve_within`, so `file_read` on `../../etc/passwd` is
refused even when granted. Limits are enforced by two mechanisms: epoch
interruption for the wall-clock timeout and a wasmtime `StoreLimits` for linear
memory. Defaults (`limits.rs`): 5 s, 16 MiB, 64 KiB per call. Modules may be
Ed25519-signed (`PluginHost::load_signed`).

**Limitation.** `Capability::ShellExec` grants nothing. Its `host_function()`
returns `None` (`manifest.rs`) and there is no `harness.exec` import — the enum
is a reservation so manifests can be written ahead of the import. Separately, the
`shell` builtin in `harness-tools` runs arbitrary commands and is **not**
mediated by this sandbox; it is contained only by its working directory and the
path guard.

### Guardrails — `harness-guardrails`

`GuardrailPipeline` runs an ordered list of `Guardrail`s over one piece of
content and returns an `Inspection`: the (possibly redacted) text, the guard that
blocked if any, and the guards that rewrote it. The deterministic guards are
cheap enough for every turn:

| guard | what it does |
|---|---|
| `SecretScanner` | redacts credential-shaped strings |
| `PiiDetector` | redacts personal data |
| `ContentFence` | fences untrusted tool output against injected instructions |
| `ToolPolicy` | refuses destructive tool calls by policy |
| `BehaviorMonitor` | refuses a tool called repeatedly with identical arguments |

Model-based review sits behind the `LlmJudge` trait; `NoJudge` and
`ScriptedJudge` are provided. Both front ends install the deterministic set when
`[guardrails] enabled` is true — the default — via
`AgentLoop::with_guardrails` (`harness-cli/src/commands/run.rs` and
`harness-server/src/state.rs::guardrail_pipeline`). A guard that fires emits an
`AgentEvent::Guardrail`, and a `Block` becomes `HarnessError::GuardrailBlocked`.

**Limitation.** No `LlmJudge` implementation is installed in production: the
trait and its test double exist, but nothing wires a model-based reviewer in.

---

## Cross-cutting: implemented but not wired

| Thing | State | Evidence |
|---|---|---|
| `ToolApproval` control message | defined, ignored by the loop, no producer | `crates/harness-agent/src/agent_loop.rs` |
| `ToolCallProgress` event | defined and converted, never emitted | `crates/harness-tools/src/lib.rs` (`progress` set only in a test) |
| `SkillRegistry::activate` | the metadata index is injected; the body is not | `crates/harness-agent/src/agent_loop.rs::skill_index` |
| `#[harness::tool]` on builtins | macro works, builtins hand-write | `crates/harness-tools/src/builtin/*.rs` |
| `LlmJudge` | trait and test double exist, nothing installs one | `crates/harness-guardrails/src/judge.rs` |
| `Capability::ShellExec` | grants nothing | `crates/harness-sandbox/src/manifest.rs` |
| Worktree integration | kept on success, never merged | `crates/harness-orchestrator/src/executor.rs` |
