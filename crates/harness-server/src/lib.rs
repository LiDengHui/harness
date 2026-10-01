//! axum HTTP + WebSocket harness server.
//!
//! The server is a thin shell around the same kernel the CLI runs: one memory
//! store, one agent registry, and a session per conversation. What it adds is
//! concurrency — many sockets, each driving its own agent loop — and the
//! backpressure discipline that keeps one unresponsive client from becoming
//! everyone's problem.
//!
//! Layout:
//!   * [`state`] — shared state and the session table.
//!   * [`api`] — the REST routes.
//!   * [`assets`] — the built UI, embedded or read from the workspace.
//!   * [`ws`] — the socket: reader loop, writer task, run task, event bridge.
//!   * [`backpressure`] — the policy applied when a subscriber stops reading.

pub mod api;
pub mod assets;
pub mod backpressure;
pub mod state;
pub mod ws;

pub use backpressure::{BackpressurePolicy, Overflow};
pub use state::{ProviderFactory, ServerState, SessionRecord};

use std::path::PathBuf;
use std::sync::Arc;

use harness_core::{Config, HarnessError, Result};
use tokio::net::TcpListener;

/// Binds, prints the listening URL, and serves until Ctrl-C.
///
/// The URL goes to stderr: stdout belongs to whatever the caller is piping, and
/// a server that mixes its banner into it would corrupt a `--json` consumer.
pub async fn serve(config: Config, workspace_root: PathBuf) -> Result<()> {
    let state = Arc::new(ServerState::new(config, workspace_root).await?);
    let listener = TcpListener::bind(state.listen_address())
        .await
        .map_err(HarnessError::Io)?;
    let bound = listener.local_addr().map_err(HarnessError::Io)?;

    eprintln!("harness server listening on http://{bound}");
    eprintln!("  REST  http://{bound}/api/health");
    eprintln!("  bus   ws://{bound}/ws");

    axum::serve(listener, Arc::clone(&state).router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|err| HarnessError::Other(format!("server stopped: {err}")))
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::error!("failed to listen for Ctrl-C: {err}");
    }
    tracing::info!("shutting down");
}
