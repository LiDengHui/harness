//! `harness serve`: the long-running HTTP API and WebSocket bus.
//!
//! The CLI's only job here is to fix the configuration — everything a session
//! needs is resolved per connection inside the server, so a config change does
//! not require touching this file.

use std::net::IpAddr;
use std::sync::Arc;

use harness_orchestrator::WorkflowSelector;
use harness_server::ServerState;

use crate::commands::ServeCommand;
use crate::context::AppContext;
use crate::workflows::{load_registry, RegistrySelector};

/// The loopback address both commands pin when asked to stay local.
pub(crate) const LOOPBACK: &str = "127.0.0.1";

/// Bytes of entropy in a generated token.
const TOKEN_BYTES: usize = 16;

pub(crate) async fn execute(command: ServeCommand, ctx: &AppContext) -> anyhow::Result<()> {
    let config = resolve_config(
        ctx,
        command.host.as_deref(),
        command.port,
        command.provider.as_deref(),
        None,
        command.permission.as_deref(),
    )?;

    print_network_notice(
        &config.server.host,
        config.server.port,
        config.server.token.as_deref(),
    );

    open_state(ctx, config)
        .await?
        .serve()
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))
}

/// Builds the server state with the workspace's workflows installed.
///
/// The registry is loaded here rather than inside the server because the
/// workflow crate is deliberately not a dependency of the server's state: the
/// [`RegistrySelector`] adapter is what bridges the two. Installing both the
/// selector and the registry keeps the list a client picks from and the set the
/// selector chooses among backed by one load, so they cannot disagree.
///
/// A malformed workflow file fails here, at startup, exactly as a malformed
/// `.agent.md` does: a registry that cannot be read is not an empty one.
pub(crate) async fn open_state(
    ctx: &AppContext,
    config: harness_core::Config,
) -> anyhow::Result<Arc<ServerState>> {
    let registry = load_registry(&ctx.workspace_root).map_err(|err| anyhow::anyhow!("{err}"))?;
    let selector: Arc<dyn WorkflowSelector> =
        Arc::new(RegistrySelector::new(Arc::clone(&registry)));

    // The registry is read once at boot, so a workflow created through
    // `POST /api/workflows` would be saved and still unchoosable. The loader
    // rebuilds both from disk on demand; the server only knows the traits.
    let workspace_root = ctx.workspace_root.clone();
    let loader = Arc::new(move || -> harness_core::Result<_> {
        let registry = load_registry(&workspace_root)?;
        let selector: Arc<dyn WorkflowSelector> =
            Arc::new(RegistrySelector::new(Arc::clone(&registry)));
        Ok((registry, selector))
    });

    let state = ServerState::new(config, ctx.workspace_root.clone())
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?
        .with_workflow_registry(Arc::clone(&registry))
        .with_workflow_selector(selector)
        .with_workflow_loader(loader);

    Ok(Arc::new(state))
}

/// Layers the flag overrides onto the loaded config.
///
/// Shared with `dhs web` so both entry points bind the same address for the same
/// flags; both then start the server through [`open_state`], so they cannot
/// disagree about the address or about the workflows installed on it.
pub(crate) fn resolve_config(
    ctx: &AppContext,
    host: Option<&str>,
    port: Option<u16>,
    provider: Option<&str>,
    effort: Option<&str>,
    permission: Option<&str>,
) -> anyhow::Result<harness_core::Config> {
    let mut config = ctx.config.clone();

    if let Some(host) = host {
        config.server.host = host.to_string();
    }
    if let Some(port) = port {
        config.server.port = port;
    }

    // `--provider` moves the *default* rather than adding an override: an agent
    // file that names its own model still wins, which is the precedence the
    // router documents. Resolving it here makes a typo fail before the socket
    // opens instead of on the first user message.
    if let Some(requested) = provider {
        let (id, _) = config
            .provider(Some(requested))
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        config.provider.default = id;
    }

    // `--effort` moves the configured default the same way: a message or an
    // agent file that names a level still outranks it. Validated here so an
    // unrecognised level is refused at startup rather than sent to the gateway
    // on every message, where it would fail opaquely.
    if let Some(requested) = effort {
        config.thinking.effort = supported_effort(requested)?.to_string();
    }

    // `--permission` moves the configured default the same way `--effort` does:
    // a message or an agent file that names a tier still outranks it. Validated
    // here so an unrecognised tier is refused at startup rather than silently
    // falling back on every message.
    if let Some(requested) = permission {
        config.permissions.mode = Some(supported_permission(requested)?);
    }

    // A bind that anything on the network can reach must not also be open to
    // it. The token is generated here rather than inside the server so both
    // commands get one from the same place, and only for a host that is not
    // loopback: a local run is unchanged, and a token on the default bind would
    // only break the browser UI for no gain.
    if !is_loopback(&config.server.host) && config.server.token.is_none() {
        config.server.token = Some(generate_token()?);
    }

    Ok(config)
}

/// Whether a bind on this host is reachable only from this machine. A hostname
/// other than `localhost` counts as exposed: the answer is uncertain and a name
/// can resolve anywhere.
pub(crate) fn is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// A random token, so an exposed bind is not an open one.
///
/// `getrandom` is the OS entropy source rather than a counter or a timestamp:
/// the token is the only thing between a routed network and a bus that can run
/// commands on this machine, so it has to be unguessable, not merely unique.
/// Failing to read it stops the command — serving without a token would
/// silently undo the protection the user asked for.
fn generate_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|err| anyhow::anyhow!("could not read OS entropy for the server token: {err}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Says out loud what a non-loopback bind means, and hands over the token that
/// keeps it from being an open door.
///
/// Printed by both `serve` and `web` so a user who reaches the port from
/// another machine knows what to paste, and a user who did not mean to expose
/// the server gets one line telling them how to undo it.
pub(crate) fn print_network_notice(host: &str, port: u16, token: Option<&str>) {
    if is_loopback(host) {
        return;
    }

    eprintln!("warning: binding {host} exposes this server to the network");
    eprintln!(
        "  anyone who can reach port {port} can drive the WebSocket bus — including the `shell` tool,"
    );
    // Flag-neutral: this notice is shared with `dhs serve`, which has no
    // `--local`.
    eprintln!(
        "  which runs commands on this machine; keep the bind on {LOOPBACK} to leave it local"
    );
    match token {
        Some(token) => {
            eprintln!("  a token is required for the API and the bus; this run's token is:");
            eprintln!("    {token}");
            eprintln!(
                "  send it as `?token=`, an `X-Harness-Token` header, or `Authorization: Bearer`"
            );
        }
        // Only reachable for a bind that came from a caller other than these
        // commands, which always generate one.
        None => eprintln!("  warning: no token is configured, so this bus is open to the network"),
    }
}

/// Canonicalises a reasoning level from a flag, refusing an unrecognised one.
fn supported_effort(requested: &str) -> anyhow::Result<&'static str> {
    harness_core::resolve_thinking_effort(requested).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported thinking effort `{requested}`; supported: {}",
            harness_core::SUPPORTED_THINKING_EFFORTS.join(", ")
        )
    })
}

/// Canonicalises a permission tier from a flag, refusing an unrecognised one.
fn supported_permission(requested: &str) -> anyhow::Result<harness_core::PermissionMode> {
    harness_core::PermissionMode::parse(requested).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported permission mode `{requested}`; supported: always_ask, ask_when_needed, full_auto"
        )
    })
}
