//! `dhs web`: start the server and put the browser UI in front of the user.
//!
//! This is `serve` plus two conveniences — a banner with the URL, and opening
//! the default browser once the listener accepts connections. The server setup
//! itself is not duplicated: [`crate::commands::serve::resolve_config`] and
//! [`crate::commands::serve::open_state`] do the work, so the two commands
//! cannot drift.
//!
//! Like `serve`, this binds `127.0.0.1` unless something asks for more. The bus
//! can drive the `shell` tool, so reaching it from a phone is a deliberate
//! `--host 0.0.0.0`, never something a user gets by typing `dhs web`.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::process::Stdio;
use std::time::Duration;

use crate::commands::serve::{self, LOOPBACK};
use crate::commands::WebCommand;
use crate::context::AppContext;

/// How long to wait for the socket before giving up on opening a browser. The
/// server binds immediately after this task starts, so this only covers a
/// machine under load; a failure here downgrades to a printed URL.
const LISTENER_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn execute(command: WebCommand, ctx: &AppContext) -> anyhow::Result<()> {
    let host = resolve_host(&command, ctx)?;
    let config = serve::resolve_config(
        ctx,
        Some(&host),
        command.port,
        command.provider.as_deref(),
        command.effort.as_deref(),
    )?;

    let port = config.server.port;
    let browse = browse_host(&host);
    // Carried into the URL the browser is opened at, because the WebSocket
    // handshake a page makes cannot set a header.
    let token = config.server.token.clone();

    warn_if_ui_missing(ctx);

    // The state is built before the spawn so a workflow registry that cannot be
    // read fails the command outright, rather than surfacing as a server task
    // that exited a moment after the banner was printed.
    let state = serve::open_state(ctx, config).await?;

    // The banner names addresses, so it waits for the bind to have happened: a
    // failed bind should produce the error and nothing else, not a URL that
    // never answered. Racing the serve future is what keeps that immediate —
    // waiting on the listener alone would stall for the whole timeout first.
    let mut server = tokio::spawn(state.serve());
    tokio::select! {
        finished = &mut server => return joined(finished),
        bound = wait_for_listener(&browse, port) => {
            if !bound {
                tracing::warn!(
                    "no listener on {browse}:{port} after {}s",
                    LISTENER_TIMEOUT.as_secs()
                );
            }
        }
    }

    serve::print_network_notice(&host, port, token.as_deref());
    print_startup(&host, &browse, port, token.as_deref());

    if !command.no_open {
        spawn_browser_opener(browse_url(&browse, port, token.as_deref()), browse, port);
    }

    joined(server.await)
}

/// The address `dhs web` binds, highest precedence first:
///
/// 1. `--host`, when given;
/// 2. `server.host` from the config, when it differs from the built-in default.
///    A config that names a host is a deliberate choice, and overriding it
///    would be a surprise;
/// 3. `127.0.0.1`, so a bare `dhs web` is reachable from this machine only.
///
/// The network bind is opt-in because of what this server is: its bus can drive
/// the `shell` tool, so `dhs web` must not hand command execution to a network
/// by default. `--host 0.0.0.0` is how a user asks for a phone or a second
/// laptop, and it is also what makes the server generate a token.
///
/// `--local` short-circuits all of it and refuses to share a command line with
/// `--host`: the two ask for opposite things, and letting one win silently
/// would leave the user believing the other took effect.
fn resolve_host(command: &WebCommand, ctx: &AppContext) -> anyhow::Result<String> {
    if command.local {
        if let Some(host) = command.host.as_deref() {
            anyhow::bail!(
                "--local pins the bind to {LOOPBACK} while --host asks for {host}; \
                 they contradict each other, drop one"
            );
        }
        return Ok(LOOPBACK.to_string());
    }

    if let Some(host) = command.host.as_deref() {
        return Ok(host.to_string());
    }

    let configured = ctx.config.server.host.as_str();
    if configured != harness_core::ServerConfig::default().host {
        return Ok(configured.to_string());
    }

    Ok(LOOPBACK.to_string())
}

/// The URL to open in a browser, carrying the token when there is one.
///
/// The page has to be able to hand the token on: a WebSocket handshake made by
/// a browser cannot set a header, so the query string is the only channel the
/// UI can read it from.
fn browse_url(browse: &str, port: u16, token: Option<&str>) -> String {
    match token {
        Some(token) => format!("http://{browse}:{port}/?token={}", url_encode(token)),
        None => format!("http://{browse}:{port}"),
    }
}

/// Percent-encodes everything outside the unreserved set.
///
/// A generated token is hex and needs none of this, but `server.token` may be
/// set by hand, and a value carrying `&` would otherwise split the query the
/// page reads — or, on Windows, become a second command on the `cmd /C start`
/// line the opener builds. The server decodes the query, so the value that
/// arrives is the value that was configured.
fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// A host that a browser on this machine can reach. `0.0.0.0` and `::` mean
/// "every interface" to the binder but are not useful destinations to navigate
/// to, so the loopback address stands in for them.
fn browse_host(host: &str) -> String {
    if host == "0.0.0.0" || host == "::" {
        "127.0.0.1".to_string()
    } else {
        host.to_string()
    }
}

/// The local address this machine would use to reach the wider network.
///
/// A UDP `connect` only picks an outbound interface — it sends nothing — so this
/// works offline, without DNS, and without the remote being reachable. `None` is
/// an ordinary outcome on a machine with no network, not an error.
fn lan_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(addr) if !addr.ip().is_loopback() => Some(*addr.ip()),
        _ => None,
    }
}

/// Prints where the server can be reached.
///
/// The LAN line is omitted on a loopback bind: an address that was never bound
/// is worse than no address, because the user would carry it to another device
/// and get a timeout with nothing to explain it.
fn print_startup(host: &str, browse: &str, port: u16, token: Option<&str>) {
    eprintln!("dhs web listening on {}", browse_url(browse, port, token));

    if !serve::is_loopback(host) {
        match lan_ipv4() {
            Some(ip) if ip.to_string() != browse => {
                eprintln!(
                    "  from other devices: {}",
                    browse_url(&ip.to_string(), port, token)
                );
            }
            Some(_) => {}
            None => {
                eprintln!("  could not detect this machine's LAN address; find it with `ipconfig`");
            }
        }
    }

    eprintln!("  stop with Ctrl-C");
}

/// `serve` is spawned so the CLI can watch the bind; both ways it can end — the
/// server's own error and a panic inside the task — have to reach the user.
fn joined(result: Result<harness_core::Result<()>, tokio::task::JoinError>) -> anyhow::Result<()> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(anyhow::anyhow!("{err}")),
        Err(err) => Err(anyhow::anyhow!("server task failed: {err}")),
    }
}

/// Says out loud what the user is about to see, instead of letting them guess
/// why the browser shows an instruction page.
///
/// This is a warning rather than an error because the API and the WebSocket bus
/// are unaffected by a missing front end — refusing to start would take away
/// the half that does work.
fn warn_if_ui_missing(ctx: &AppContext) {
    if cfg!(feature = "embed-ui") {
        return;
    }
    if ctx.workspace_root.join("web/dist").is_dir() {
        return;
    }

    eprintln!(
        "warning: this build has no embedded UI and there is no `web/dist` under {}",
        ctx.workspace_root.display()
    );
    eprintln!("  the browser will get an instruction page, not the app; the API still works");
    eprintln!("  fix it either way:");
    eprintln!("    cd web && pnpm build");
    eprintln!("    cargo install --path crates/harness-cli --features embed-ui --bin dhs");
}

fn spawn_browser_opener(url: String, host: String, port: u16) {
    tokio::spawn(async move {
        if !wait_for_listener(&host, port).await {
            tracing::warn!("server did not accept connections in time; open {url} manually");
            return;
        }
        if let Err(err) = open_browser(&url) {
            tracing::warn!("could not open a browser ({err}); open {url} manually");
        }
    });
}

/// Polls the socket until the server accepts, so the browser does not race the
/// bind and land on a connection-refused page.
async fn wait_for_listener(host: &str, port: u16) -> bool {
    let target = format!("{host}:{port}");
    let deadline = tokio::time::Instant::now() + LISTENER_TIMEOUT;

    loop {
        if tokio::net::TcpStream::connect(&target).await.is_ok() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Hands the URL to the platform opener. The URL is passed as an argument, never
/// interpolated into a shell string, so nothing in it can become a command.
fn open_browser(url: &str) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "windows") {
        let mut command = std::process::Command::new("cmd");
        // `start` reads the first quoted token as a window title, so the empty
        // title keeps it from swallowing the URL.
        command.args(["/C", "start", "", url]);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_host(host: &str) -> AppContext {
        let mut config = harness_core::Config::default();
        config.server.host = host.to_string();
        AppContext {
            workspace_root: std::path::PathBuf::from("."),
            config,
        }
    }

    fn command(host: Option<&str>, local: bool) -> WebCommand {
        WebCommand {
            host: host.map(str::to_string),
            port: None,
            provider: None,
            effort: None,
            no_open: true,
            local,
        }
    }

    /// A bare `dhs web` stays on this machine. It used to bind `0.0.0.0`, which
    /// handed the `shell` tool to every host that could route to the port; the
    /// network bind is now something a user asks for.
    #[test]
    fn a_bare_web_command_binds_loopback_only() {
        let host = resolve_host(&command(None, false), &ctx_with_host("127.0.0.1")).unwrap();
        assert_eq!(host, LOOPBACK);
    }

    #[test]
    fn a_deliberate_config_host_outranks_the_default() {
        let host = resolve_host(&command(None, false), &ctx_with_host("192.168.1.7")).unwrap();
        assert_eq!(host, "192.168.1.7");
    }

    #[test]
    fn the_flag_outranks_the_config() {
        let host = resolve_host(
            &command(Some("10.0.0.4"), false),
            &ctx_with_host("192.168.1.7"),
        )
        .unwrap();
        assert_eq!(host, "10.0.0.4");
    }

    #[test]
    fn local_pins_loopback_over_the_config() {
        let host = resolve_host(&command(None, true), &ctx_with_host("192.168.1.7")).unwrap();
        assert_eq!(host, LOOPBACK);
    }

    #[test]
    fn local_and_host_together_are_refused() {
        let err =
            resolve_host(&command(Some("0.0.0.0"), true), &ctx_with_host("127.0.0.1")).unwrap_err();
        assert!(err.to_string().contains("--local"), "{err}");
        assert!(err.to_string().contains("--host"), "{err}");
    }

    #[test]
    fn loopback_binds_are_recognized_and_nothing_else_is() {
        assert!(serve::is_loopback("127.0.0.1"));
        assert!(serve::is_loopback("127.0.0.53"));
        assert!(serve::is_loopback("::1"));
        assert!(serve::is_loopback("localhost"));
        assert!(serve::is_loopback("LOCALHOST"));
        assert!(!serve::is_loopback("0.0.0.0"));
        assert!(!serve::is_loopback("192.168.1.7"));
        assert!(!serve::is_loopback("example.internal"));
    }

    #[test]
    fn the_wildcard_binds_are_browsed_as_loopback() {
        assert_eq!(browse_host("0.0.0.0"), LOOPBACK);
        assert_eq!(browse_host("::"), LOOPBACK);
        assert_eq!(browse_host("192.168.1.7"), "192.168.1.7");
    }

    /// The token has to reach the page, and the only channel a browser can use
    /// for a WebSocket handshake is the query string.
    #[test]
    fn the_browser_url_carries_the_token_only_when_there_is_one() {
        assert_eq!(browse_url("127.0.0.1", 8787, None), "http://127.0.0.1:8787");
        assert_eq!(
            browse_url("127.0.0.1", 8787, Some("abc123")),
            "http://127.0.0.1:8787/?token=abc123"
        );
        // A hand-set token must not be able to split the query, or the command
        // line the Windows opener builds.
        assert_eq!(
            browse_url("127.0.0.1", 8787, Some("a&b c")),
            "http://127.0.0.1:8787/?token=a%26b%20c"
        );
    }
}
