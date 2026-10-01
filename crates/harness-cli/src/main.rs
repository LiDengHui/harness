mod commands;
mod context;
mod workflows;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use commands::{
    AgentCommand, ConfigCommand, McpCommand, MemoryCommand, PlanCommand, PluginCommand, RunCommand,
    Runnable, ServeCommand, SkillCommand, WebCommand, WorkflowCommand,
};
use context::AppContext;

/// Exit code used for command failures, so scripts can tell them apart from
/// clap's own usage errors (which exit with 2 as well, but print a usage block).
const EXIT_FAILURE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "dhs",
    version,
    about = "Rust agent harness: ReAct kernel, DAG memory, WebSocket bus",
    propagate_version = true
)]
struct Cli {
    /// Workspace root. Defaults to the current directory.
    #[arg(long, global = true, value_name = "DIR", default_value = ".")]
    workspace: PathBuf,

    /// Extra configuration files, layered on top of the global and project files.
    #[arg(long = "config", global = true, value_name = "FILE")]
    config_files: Vec<PathBuf>,

    /// Log verbosity. `RUST_LOG` overrides this when set.
    #[arg(long, global = true, value_enum, default_value_t = LogLevel::Info)]
    log: LogLevel,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn as_filter(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the HTTP API and the WebSocket bus.
    Serve(ServeCommand),
    /// Serve the browser UI and open it in the default browser.
    #[command(long_about = "Start the harness server and open the browser UI.\n\n\
The bind is chosen in this order: `--host`; then `server.host` from the config, when it\n\
differs from the built-in `127.0.0.1` (a config that names a host is taken as deliberate);\n\
then `127.0.0.1`. `--local` pins `127.0.0.1` whatever the config says and cannot be\n\
combined with `--host`.\n\n\
The WebSocket bus can drive the `shell` tool, so anything that can reach the port can run\n\
commands on this machine. A bare `dhs web` therefore stays on this machine; reaching it\n\
from a phone or another laptop is `--host 0.0.0.0`, and that also makes the server\n\
generate a token which the client must present on the API and the bus — it is printed at\n\
startup and is part of the URL the browser is opened at. On Windows the first run usually\n\
raises a firewall prompt for the binary; if it is dismissed, the port stays unreachable\n\
from other devices even though the bind succeeded.\n\n\
The UI is served from the copy compiled into the binary, or from `web/dist` in the\n\
workspace. A globally installed build has neither next to it, so it must embed the UI:\n\n    \
cargo install --path crates/harness-cli --features embed-ui --bin dhs\n\n\
Without `embed-ui` and without a built `web/dist`, the API and WebSocket bus still work,\n\
but the browser gets an instruction page instead of the app.")]
    Web(WebCommand),
    /// Run one prompt headlessly and print the result.
    Run(RunCommand),
    /// Decompose a task into a DAG and execute it.
    Plan(PlanCommand),
    /// Inspect agents declared by `.agent.md` files.
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Inspect skills declared by `SKILL.md` files.
    #[command(subcommand)]
    Skill(SkillCommand),
    /// Inspect the workflow procedures declared under `workflows/`.
    #[command(subcommand)]
    Workflow(WorkflowCommand),
    /// Inspect and manipulate the memory DAG.
    #[command(subcommand)]
    Memory(MemoryCommand),
    /// Inspect and call MCP servers.
    #[command(subcommand)]
    Mcp(McpCommand),
    /// Inspect and run WASM plugins.
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Inspect the resolved configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.log);

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let ctx = AppContext::load(cli.workspace, &cli.config_files)
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    match cli.command {
        Command::Serve(cmd) => cmd.run(&ctx).await,
        Command::Web(cmd) => cmd.run(&ctx).await,
        Command::Run(cmd) => cmd.run(&ctx).await,
        Command::Plan(cmd) => cmd.run(&ctx).await,
        Command::Agent(cmd) => cmd.run(&ctx).await,
        Command::Skill(cmd) => cmd.run(&ctx).await,
        Command::Workflow(cmd) => cmd.run(&ctx).await,
        Command::Memory(cmd) => cmd.run(&ctx).await,
        Command::Mcp(cmd) => cmd.run(&ctx).await,
        Command::Plugin(cmd) => cmd.run(&ctx).await,
        Command::Config(cmd) => cmd.run(&ctx).await,
    }
}

fn init_tracing(level: LogLevel) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!("harness={level},warn", level = level.as_filter()))
    });

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
