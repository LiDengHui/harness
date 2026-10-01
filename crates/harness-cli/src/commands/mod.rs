use std::sync::Arc;
use std::time::Duration;

use clap::Subcommand;
use harness_core::Config;
use harness_llm::Provider;
use harness_orchestrator::{Verifier, VerifierConfig};

use crate::context::AppContext;

pub mod agent;
pub mod mcp;
pub mod memory;
pub mod plan;
pub mod plugin;
pub mod run;
pub mod serve;
pub mod skill;
pub mod web;
pub mod workflow;

/// A leaf subcommand. Async because most commands talk to a model or a socket.
#[allow(async_fn_in_trait)]
pub trait Runnable {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()>;
}

#[derive(Debug, clap::Args)]
pub struct ServeCommand {
    /// Overrides `server.host`.
    #[arg(long)]
    pub host: Option<String>,
    /// Overrides `server.port`.
    #[arg(long)]
    pub port: Option<u16>,
    /// Provider id used as the session default.
    #[arg(long)]
    pub provider: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct WebCommand {
    /// Overrides `server.host`. Defaults to `127.0.0.1`, or to `server.host`
    /// when the config sets one; pass `0.0.0.0` to reach the UI from another
    /// device, which also makes the server require a token. Cannot be combined
    /// with `--local`.
    #[arg(long)]
    pub host: Option<String>,
    /// Overrides `server.port`. Defaults to `8787`.
    #[arg(long)]
    pub port: Option<u16>,
    /// Provider id used as the session default.
    #[arg(long)]
    pub provider: Option<String>,
    /// Reasoning effort used as the session default (`low`, `high` or `max`).
    /// An agent file that declares its own still wins.
    #[arg(long)]
    pub effort: Option<String>,
    /// Do not open a browser; just serve.
    #[arg(long)]
    pub no_open: bool,
    /// Bind `127.0.0.1` only, so nothing outside this machine can connect.
    #[arg(long)]
    pub local: bool,
}

#[derive(Debug, clap::Args)]
pub struct RunCommand {
    /// The task to run. Reads stdin when omitted.
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Reasoning effort for this run (`low`, `high` or `max`). Outranks the
    /// agent's own declaration, which outranks the configured default.
    #[arg(long)]
    pub effort: Option<String>,
    /// Overrides the agent's `max_tokens` budget for this run.
    #[arg(long)]
    pub max_tokens: Option<u64>,
    /// Emit the raw `AgentEvent` stream as JSON lines instead of pretty output.
    #[arg(long)]
    pub json_events: bool,
    /// Command that decides whether the run's result is acceptable, e.g.
    /// `--verify "cargo test"`. Repeatable; each value is split on whitespace
    /// into program and arguments. Outranks `[verify] checks`.
    #[arg(long, value_name = "CMD")]
    pub verify: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub struct PlanCommand {
    /// A task description to decompose into a DAG of sub-tasks.
    #[arg(long)]
    pub task: String,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Model for the planning call. Takes `provider/model` or a bare model name,
    /// like `--model`. Defaults to the resolved agent's model.
    #[arg(long)]
    pub planner_model: Option<String>,
    /// Maximum number of Reflexion retries per sub-task.
    #[arg(long, default_value_t = 2)]
    pub max_retries: u32,
    /// Command that decides whether a node's result is acceptable, e.g.
    /// `--verify "cargo test"`. Repeatable; each value is split on whitespace
    /// into program and arguments. Outranks `[verify] checks`.
    #[arg(long, value_name = "CMD")]
    pub verify: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// List every `.agent.md` found in the global and project agent directories.
    List,
    /// Show the resolved specification for one agent.
    Show { id: String },
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// List discoverable skills.
    List,
    /// Show the full body of one skill.
    Show { name: String },
}

#[derive(Debug, Subcommand)]
pub enum WorkflowCommand {
    /// List the workflow procedures declared under `workflows/`.
    List,
    /// Show one workflow's stages, guidance and source file.
    Show { id: String },
    /// Check that every workflow loads and that each stage's agent exists.
    Validate {
        /// Check only this workflow. Omit to check them all.
        id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Print the session DAG.
    Tree { session: Option<String> },
    /// Fork a new session from an existing node.
    Branch {
        session: String,
        #[arg(long)]
        from: Option<String>,
    },
    /// Run lossless trimming over a session and report token savings.
    Trim { session: String },
    /// Delete a session, or every session older than an age.
    ///
    /// Deletes only the sessions named: a session forked from one of them is a
    /// separate conversation and is left in place, though its inherited history
    /// goes with the deleted session.
    Rm {
        /// Session id to delete. Give this or `--older-than`, not both.
        session: Option<String>,
        /// Delete every session created more than this long ago, e.g. `30d`,
        /// `12h`, `90m`, `2w`.
        #[arg(long, value_name = "AGE")]
        older_than: Option<String>,
        /// List what would be deleted without deleting it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print storage statistics.
    Stats,
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// List configured MCP servers and their tools.
    List,
    /// Invoke a tool on a configured MCP server.
    Call {
        server: String,
        tool: String,
        #[arg(long, default_value = "{}")]
        json: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    /// List installed WASM plugins and their granted capabilities.
    List,
    /// Run a plugin entry point.
    Run {
        name: String,
        #[arg(long, default_value = "{}")]
        json: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the merged configuration.
    Show {
        #[arg(long, value_enum, default_value_t = OutputFormat::Toml)]
        format: OutputFormat,
    },
    /// Print the file paths that were consulted, and whether each exists.
    Paths,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Toml,
    Json,
}

impl Runnable for ServeCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        serve::execute(self, ctx).await
    }
}

impl Runnable for WebCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        web::execute(self, ctx).await
    }
}

impl Runnable for RunCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        run::execute(self, ctx).await
    }
}

impl Runnable for PlanCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        plan::execute(self, ctx).await
    }
}

impl Runnable for AgentCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        agent::execute(self, ctx).await
    }
}

impl Runnable for SkillCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        skill::execute(self, ctx).await
    }
}

impl Runnable for WorkflowCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        workflow::execute(self, ctx).await
    }
}

impl Runnable for MemoryCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        memory::execute(self, ctx).await
    }
}

impl Runnable for McpCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        mcp::execute(self, ctx).await
    }
}

impl Runnable for PluginCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        plugin::execute(self, ctx).await
    }
}

impl Runnable for ConfigCommand {
    async fn run(self, ctx: &AppContext) -> anyhow::Result<()> {
        match self {
            ConfigCommand::Show { format } => show(ctx, format),
            ConfigCommand::Paths => paths(ctx),
        }
    }
}

fn show(ctx: &AppContext, format: OutputFormat) -> anyhow::Result<()> {
    // Never echo a key back: `config show` output ends up in terminals, issue
    // reports and CI logs. The server token is the same kind of secret — it is
    // what stands between a routed network and a bus that can run commands on
    // this machine — and `dhs serve`/`dhs web` print it themselves when they
    // generate one, so nothing is lost by hiding it here.
    let mut config = ctx.config.clone();
    let mut redacted = false;
    for provider in config.providers.values_mut() {
        if provider.api_key.is_some() {
            provider.api_key = Some("[redacted]".to_string());
            redacted = true;
        }
    }
    if config.server.token.is_some() {
        config.server.token = Some("[redacted]".to_string());
        redacted = true;
    }

    let rendered = match format {
        OutputFormat::Toml => toml::to_string_pretty(&config)
            .map_err(|e| anyhow::anyhow!("failed to serialize config: {e}"))?,
        OutputFormat::Json => serde_json::to_string_pretty(&config)
            .map_err(|e| anyhow::anyhow!("failed to serialize config: {e}"))?,
    };
    print!("{rendered}");
    if !rendered.ends_with('\n') {
        println!();
    }
    if redacted {
        eprintln!("note: inline api_key values are redacted here");
    }
    Ok(())
}

fn paths(ctx: &AppContext) -> anyhow::Result<()> {
    println!("workspace: {}", ctx.workspace_root.display());
    for path in harness_core::Config::layer_paths(&ctx.workspace_root) {
        let mark = if path.exists() { "found  " } else { "missing" };
        println!("{mark} {}", path.display());
    }
    println!("database:  {}", ctx.db_path().display());
    Ok(())
}

/// The checks a run verifies with: the ones given on the command line when there
/// are any, otherwise the `[verify]` section.
///
/// The flag wins because it is the narrower, more deliberate statement — typed
/// for this invocation rather than left in a file — and a run that says what to
/// check should not have that quietly widened by a config it never read.
pub(crate) fn resolve_verify_checks(
    from_flag: &[String],
    configured: &[Vec<String>],
) -> Vec<Vec<String>> {
    if from_flag.is_empty() {
        return configured.to_vec();
    }
    from_flag
        .iter()
        .map(|command| {
            command
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<String>>()
        })
        .collect()
}

/// The verifier both `run` and `plan` build from the `[verify]` section.
///
/// The adversarial reviewer is the run's own provider rather than a second one:
/// what makes the review independent is that it is a fresh, tool-free request
/// that never sees the worker's conversation, not that it is a different model.
pub(crate) fn build_verifier(
    config: &Config,
    provider: Arc<dyn Provider>,
    checks: Vec<Vec<String>>,
) -> Verifier {
    Verifier::new(VerifierConfig {
        checks,
        timeout: Duration::from_secs(config.verify.timeout_secs),
        adversarial_provider: config.verify.adversarial.then(|| Arc::clone(&provider)),
        adversarial_model: None,
    })
}
