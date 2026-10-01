# dhl-harness

A Rust agent harness: a ReAct kernel, a SQLite DAG memory, an orchestrator that
runs a task as a graph of sub-agents, a declarative workflow layer, and a
WebSocket + REST server with a browser UI. One binary, `dhs`, drives all of it,
and the same program also builds as `harness`.

The workspace is 14 crates under `crates/`. Everything is local: conversations
live in SQLite, workflows and agents are Markdown files in the repository, and
the only network traffic is to the model provider you configure.

---

## Requirements

- Rust stable with `rustfmt` and `clippy` (pinned by `rust-toolchain.toml`).
- Node.js and [pnpm](https://pnpm.io/) — only if you want the browser UI.
- A model provider: either a local endpoint that needs no key, or an API key for
  a hosted one. `--provider mock` runs the whole loop offline with no key at all.

## Build

```bash
cargo build              # debug build of every crate
cargo test               # the whole suite
```

The browser UI is a separate build. The server serves it from `web/dist`, so
build it before `dhs web`:

```bash
cd web
pnpm install
pnpm build               # writes web/dist
```

To install `dhs` globally as one self-contained binary, compile the UI in:

```bash
cargo install --path crates/harness-cli --features embed-ui --bin dhs
```

Without `embed-ui` and without a built `web/dist`, the API and the WebSocket bus
still work; the browser just gets an instruction page instead of the app.

## Run

From a clone, the fastest path to a running conversation with **no API key**:

```bash
# one headless prompt, offline
cargo run --bin dhs -- run --provider mock --prompt "say hello"

# the server plus the browser UI, offline
cd web && pnpm install && pnpm build && cd ..
cargo run --bin dhs -- web --provider mock
```

`dhs web` binds `127.0.0.1` by default and opens your browser, so the UI starts
out local-only. A network bind is opt-in: `--host 0.0.0.0` asks for it and is
also what **generates a token**, printed with the URL it opens. `--local` pins
the bind back to loopback and refuses to be combined with `--host`. The bus can
drive the `shell` tool, so handing it to the network must be a deliberate act.

The other entry points:

| command | what it does |
|---|---|
| `dhs run --prompt "…"` | one prompt headlessly; reads stdin when `--prompt` is absent |
| `dhs plan --task "…"` | decompose a task into a DAG and execute it with sub-agents |
| `dhs web` | server + browser UI (loopback; `--host 0.0.0.0` for network + token) |
| `dhs serve` | server only, loopback by default |
| `dhs agent list\|show` | inspect the `.agent.md` files |
| `dhs skill list\|show` | inspect the `SKILL.md` files |
| `dhs workflow list\|show\|validate` | inspect and check the workflow procedures |
| `dhs memory tree\|stats\|trim\|branch\|rm` | inspect and clean up stored conversations |
| `dhs mcp list\|call` | inspect and call configured MCP servers |
| `dhs plugin list\|run` | inspect and run WASM plugins |
| `dhs config show\|paths` | the merged configuration and the files it came from |

Run `dhs --help` or `dhs <command> --help` for the flags.

## Provider configuration and the key requirement

Configuration is layered, later layers winning key by key:

1. `~/.harness/config.toml` (global)
2. `<workspace>/.harness/config.toml` (project)
3. `--config FILE` (one or more, last wins)

`.harness/secrets.toml` is loaded automatically after the project file and is
gitignored — it is where a key goes when you do not want it in the environment.

A provider is a named table; the default picks which one a run uses:

```toml
[provider]
default = "deepseek"

[providers.deepseek]
kind = "openai"                                  # an OpenAI-compatible endpoint
base_url = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"                 # the env var holding the key
default_model = "deepseek-chat"
```

**The key requirement.** A hosted endpoint with no key is refused before a
request is sent, and the error names the environment variable to export. The two
ways to supply a key are the environment variable named by `api_key_env`, or an
`api_key = "…"` line in `.harness/secrets.toml`. A keyless endpoint that resolves
to the loopback interface (`localhost`, `127.0.0.1`, `::1`) is left alone: a
local Ollama/llama.cpp/vLLM server is a deliberate setup, not a misconfiguration.

`--provider mock` needs no key and replays a fixed script (one tool round-trip,
then a text answer), which is what makes the offline path usable for smoke tests.

## Where things live

| path | what it is |
|---|---|
| `crates/harness-core` | shared types: config, errors, the event vocabulary, ids, the `Memory` contract |
| `crates/harness-llm` | the streaming `Provider` trait, one OpenAI-compatible client, and a scripted mock |
| `crates/harness-tools` | the `Tool` trait, the registry, and the seven builtins (`read_file`, `write_file`, `edit_file`, `list_dir`, `grep`, `shell`, `web_fetch`) |
| `crates/harness-memory` | SQLite-backed DAG memory: snapshot, branch, trim, hybrid recall |
| `crates/harness-agent` | the ReAct `AgentLoop`, `.agent.md` specs and registry, token budget, control channel |
| `crates/harness-skills` | `SKILL.md` discovery with progressive disclosure |
| `crates/harness-macros` | `#[harness::tool]` and `#[harness::skill]` |
| `crates/harness-mcp` | an MCP client and the adapter that presents remote tools as local ones |
| `crates/harness-sandbox` | the WebAssembly plugin host and its capability checks |
| `crates/harness-guardrails` | deterministic guards (secrets, PII, injection fences, tool policy) and an optional model judge |
| `crates/harness-orchestrator` | `Planner`, `Executor`, `Verifier`, and git-worktree isolation |
| `crates/harness-workflows` | declarative stage graphs loaded from `<id>.workflow.md` |
| `crates/harness-server` | the axum REST surface and the WebSocket bus |
| `crates/harness-cli` | the `dhs` (and `harness`) command line |
| `web/` | the Vue 3 + Pinia browser UI |
| `workflows/` | the built-in workflow procedures |
| `agents/`, `skills/` | the built-in agent and skill declarations |
| `plugins/` | example `.wat` WASM plugins and their manifests |
| `.harness/` | `config.toml` is tracked; the rest is gitignored runtime state (`secrets.toml`, `harness.db`, …) |
| `docs/` | architecture, acceptance evidence |

## What is not done

This is a working harness, not a finished product. The honest gaps: **no
authentication beyond the auto-generated token** on a network bind, and no TLS —
put it behind a reverse proxy if you expose it. **Nothing merges worktrees**: a
sub-agent that succeeds keeps its checkout and branch, and integrating the work
is the caller's job. **Recall is lexical, not semantic** — the embedder is a
feature-hashing bag of words, so a paraphrase that shares no vocabulary is not
found. **`Capability::ShellExec` grants nothing**; the `shell` builtin runs
commands outside the plugin sandbox, contained only by its working directory and
the path guard. **`ToolApproval` is inert** — the control message exists and the
loop discards it, so nothing asks a human to approve a tool call. **Skill bodies
are never injected**: an agent's declared skills reach the prompt as a
name/description/path index, and the model reads the body with a tool if it wants
it. Finally, **no real third-party MCP server has been exercised**: the MCP tests
drive a locally written fake. `docs/acceptance.md` records what is verified and
how.

## License

MIT — see [`LICENSE`](LICENSE). Copyright (c) LiDengHui.
