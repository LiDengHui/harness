//! `web_fetch`: HTTP GET of a single URL.
//!
//! The tool is a network request the model chooses, which makes it the one
//! place a prompt-injected instruction turns into traffic: "fetch
//! `http://169.254.169.254/latest/meta-data/...`" is a credential read, and
//! `http://127.0.0.1:8787` is this harness's own bus. So the URL's host is
//! resolved and the addresses it names are checked before anything is sent;
//! see [`blocked_reason`] for what is refused and why.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use harness_core::{object_schema, HarnessError, Result, ToolSpec};
use reqwest::Url;
use serde_json::{json, Value};

use crate::args::Args;
use crate::builtin::truncate_payload;
use crate::{Tool, ToolContext, ToolOutput};

const DEFAULT_MAX_BYTES: usize = 200_000;

pub struct WebFetch;

/// One connection pool per address policy, built lazily.
///
/// Two pools rather than one because the resolver that enforces the address
/// rule belongs to the client, and a caller that has explicitly opted into
/// private hosts has to get a pool without it.
fn client(allow_private: bool) -> Result<&'static reqwest::Client> {
    static STRICT: OnceLock<std::result::Result<reqwest::Client, String>> = OnceLock::new();
    static OPEN: OnceLock<std::result::Result<reqwest::Client, String>> = OnceLock::new();

    let pool = if allow_private { &OPEN } else { &STRICT };
    pool.get_or_init(|| build_client(allow_private))
        .as_ref()
        .map_err(|err| HarnessError::Tool(format!("web_fetch: no HTTP client available: {err}")))
}

fn build_client(allow_private: bool) -> std::result::Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder();
    let builder = if allow_private {
        builder
    } else {
        builder.dns_resolver(Arc::new(PublicOnlyResolver))
    };
    builder.build().map_err(|err| err.to_string())
}

/// A resolver that drops every address `web_fetch` refuses to reach.
///
/// The pre-flight check below runs before the request, but the host name is
/// resolved again when the connection is made. A name the attacker controls can
/// answer with a public address for the check and `127.0.0.1` for the
/// connection — the classic DNS-rebinding shape — so the rule is applied at
/// resolution time as well, where it also covers every redirect the client
/// follows on its own.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(resolve_public(name))
    }
}

async fn resolve_public(
    name: reqwest::dns::Name,
) -> std::result::Result<reqwest::dns::Addrs, Box<dyn std::error::Error + Send + Sync>> {
    let host = name.as_str().to_string();
    let allowed: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
        .await?
        .filter(|addr| blocked_reason(addr.ip()).is_none())
        .collect();

    if allowed.is_empty() {
        return Err(format!(
            "`{host}` resolves only to addresses web_fetch refuses to reach \
             (loopback, private, link-local or cloud-metadata addresses)"
        )
        .into());
    }
    Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
}

/// Why an address may not be fetched, in the words the model is shown.
///
/// The message names the class rather than only refusing, so the model can tell
/// "this is a service on the user's own machine" from "this host is down" and
/// does not simply retry the same URL.
fn blocked_reason(ip: IpAddr) -> Option<&'static str> {
    // An IPv4-mapped IPv6 address is the same host as the IPv4 address it
    // carries, so `::ffff:127.0.0.1` has to be judged as `127.0.0.1`.
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };

    match ip {
        IpAddr::V4(v4) => blocked_reason_v4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return Some("a loopback address, which reaches services on this machine");
            }
            if v6.is_unspecified() {
                return Some("the unspecified address");
            }
            if v6.is_multicast() {
                return Some("a multicast address");
            }
            let first = v6.segments()[0];
            if first & 0xfe00 == 0xfc00 {
                return Some("a unique-local address, which reaches services on the local network");
            }
            if first & 0xffc0 == 0xfe80 {
                return Some("a link-local address, which is where cloud metadata services live");
            }
            None
        }
    }
}

fn blocked_reason_v4(v4: Ipv4Addr) -> Option<&'static str> {
    if v4.is_loopback() {
        return Some("a loopback address, which reaches services on this machine");
    }
    if v4.is_private() {
        return Some("a private address, which reaches services on the local network");
    }
    if v4.is_link_local() {
        return Some(
            "a link-local address, which is where cloud metadata services such as 169.254.169.254 live",
        );
    }
    if v4.is_unspecified() {
        return Some("the unspecified address");
    }
    if v4.is_broadcast() {
        return Some("the broadcast address");
    }
    if v4.is_multicast() {
        return Some("a multicast address");
    }
    if v4.is_documentation() {
        return Some("a documentation address, which is never a real service");
    }
    let [first, second, ..] = v4.octets();
    if first == 100 && (64..128).contains(&second) {
        return Some("a carrier-grade NAT address, which is not a public host");
    }
    if first == 198 && (18..20).contains(&second) {
        return Some("a benchmarking address, which is never a real service");
    }
    if first >= 240 {
        return Some("a reserved address");
    }
    None
}

/// The host a URL names, as either a literal address or a name to resolve.
enum HostSpec<'a> {
    Literal(IpAddr),
    Name(&'a str),
}

fn host_spec(host: &str) -> HostSpec<'_> {
    // `Url::host_str` keeps the brackets on an IPv6 literal.
    if let Some(inner) = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        if let Ok(v6) = inner.parse::<Ipv6Addr>() {
            return HostSpec::Literal(IpAddr::V6(v6));
        }
    }
    match host.parse::<IpAddr>() {
        Ok(ip) => HostSpec::Literal(ip),
        Err(_) => HostSpec::Name(host),
    }
}

/// The first address a URL reaches that `web_fetch` refuses, if any.
///
/// Every answer is checked, not just the first: a name that resolves to a
/// public address *and* a loopback one is exactly what a rebinding attack
/// looks like from here, and the public answer is no reason to trust the other.
async fn refused_address(host: &str, port: u16) -> Result<Option<(IpAddr, &'static str)>> {
    match host_spec(host) {
        HostSpec::Literal(ip) => Ok(blocked_reason(ip).map(|reason| (ip, reason))),
        HostSpec::Name(name) => {
            let addresses = tokio::net::lookup_host((name, port)).await.map_err(|err| {
                HarnessError::Tool(format!(
                    "web_fetch: could not resolve `{name}`: {err}. Check the host name, then retry."
                ))
            })?;
            for address in addresses {
                if let Some(reason) = blocked_reason(address.ip()) {
                    return Ok(Some((address.ip(), reason)));
                }
            }
            Ok(None)
        }
    }
}

/// `reqwest::Error`'s own message hides the reason; the model needs to see
/// "connection refused" rather than "error sending request".
fn describe(err: &(dyn std::error::Error + 'static)) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[async_trait]
impl Tool for WebFetch {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "web_fetch",
            "Fetch an http/https URL and return the body as text. Prefer this for a known URL; there is no web-search tool, and local files are read with `read_file`. A non-2xx status is an error whose body is still shown, and a body over `max_bytes` is truncated with the full size stated. Fails on a non-http scheme or a network error, which names the URL. A URL whose host resolves to a loopback, private, link-local or cloud-metadata address is refused, and the refusal says which; do not retry it with another spelling.",
            object_schema(
                json!({
                    "url": { "type": "string", "description": "Absolute http:// or https:// URL." },
                    "max_bytes": { "type": "integer", "description": "Truncate the body at this many bytes. Defaults to 200000." }
                }),
                &["url"],
            ),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let args = Args::new("web_fetch", args)?;
        let url = args.required_str("url")?;
        let max_bytes = args
            .optional_usize("max_bytes")?
            .unwrap_or(DEFAULT_MAX_BYTES);

        // Parsed rather than string-matched: `http://` as a prefix says nothing
        // about what follows it, and the host is what the address rule reads.
        let parsed = Url::parse(url.trim()).map_err(|err| {
            HarnessError::Tool(format!(
                "web_fetch: `{url}` is not an absolute URL ({err}); only http:// and https:// URLs are supported"
            ))
        })?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(HarnessError::Tool(format!(
                "web_fetch: only http:// and https:// URLs are supported, got `{}`",
                parsed.scheme()
            )));
        }
        let Some(host) = parsed.host_str() else {
            return Err(HarnessError::Tool(format!(
                "web_fetch: `{url}` names no host to fetch from"
            )));
        };
        let port = parsed.port_or_known_default().unwrap_or(80);

        let allow_private = ctx.config.web_fetch_allow_private_hosts;
        if !allow_private {
            if let Some((ip, reason)) = refused_address(host, port).await? {
                return Err(HarnessError::Tool(format!(
                    "web_fetch: refusing `{url}` — {ip} is {reason}. This tool reaches the \
                     public web only; ask the user before fetching a service on this machine \
                     or its network."
                )));
            }
        }

        ctx.emit_progress(format!("fetching {url}"));
        let response = client(allow_private)?.get(parsed).send().await.map_err(|err| {
            HarnessError::Tool(format!(
                "web_fetch: `{url}` failed: {}. Check the URL and your network connection, then retry.",
                describe(&err)
            ))
        })?;

        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        let body = response.bytes().await.map_err(|err| {
            HarnessError::Tool(format!(
                "web_fetch: reading `{url}` failed: {}. The connection dropped mid-body; retry, or fetch a smaller resource.",
                describe(&err)
            ))
        })?;
        let text = String::from_utf8_lossy(&body);
        let cut = truncate_payload(&text, max_bytes);
        let mut content = cut.content(&format!(
            "the body is larger than the {max_bytes}-byte `max_bytes` limit; raise `max_bytes`, or fetch a more specific URL."
        ));

        if !status.is_success() {
            let frame = format!(
                "[web_fetch: HTTP {} for `{url}`; the body below is the error response.]",
                status.as_u16()
            );
            content = if content.is_empty() {
                frame
            } else {
                format!("{frame}\n{content}")
            };
        }

        let metadata = json!({
            "status": status.as_u16(),
            "content_type": content_type,
            "bytes": cut.shown_bytes,
            "total_bytes": cut.total_bytes,
            "truncated": cut.truncated,
        });
        let output = if status.is_success() {
            ToolOutput::text(content)
        } else {
            ToolOutput::error(content)
        };
        Ok(output.with_metadata(metadata))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::builtin::test_support::context;
    use reqwest::dns::Resolve;

    /// A context that has opted into private hosts.
    ///
    /// The two tests below are about truncation and status framing, so they
    /// need a reachable server, and the only server a test can bind is on
    /// loopback. The address rule itself is what the tests after them cover.
    fn permissive_context(root: &std::path::Path) -> ToolContext {
        let config = harness_core::ToolsConfig {
            web_fetch_allow_private_hosts: true,
            ..harness_core::ToolsConfig::default()
        };
        ToolContext::new(root.to_path_buf(), config)
    }

    #[tokio::test]
    async fn only_http_urls_are_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        for url in ["file:///etc/passwd", "ftp://example.com/x", "not a url", ""] {
            let err = WebFetch
                .call(json!({ "url": url }), &ctx)
                .await
                .unwrap_err();
            assert!(matches!(err, HarnessError::Tool(_)), "{url}: {err}");
            assert!(err.to_string().contains("http://"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn url_is_required_and_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let err = WebFetch.call(json!({}), &ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("missing required argument `url`"),
            "{err}"
        );

        let err = WebFetch
            .call(json!({ "url": 7, "max_bytes": "lots" }), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("argument `url`"), "{err}");
    }

    /// Serves one canned response on a loopback port and returns its URL.
    async fn serve(status: &'static str, body: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let address = listener.local_addr().expect("local address");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0u8; 1024];
                let _ = socket.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{address}/")
    }

    #[tokio::test]
    async fn a_large_body_is_truncated_with_a_marker() {
        let body = "z".repeat(500);
        let url = serve("200 OK", body.clone()).await;
        let tmp = tempfile::tempdir().unwrap();
        let ctx = permissive_context(tmp.path());

        let out = WebFetch
            .call(json!({ "url": url, "max_bytes": 50 }), &ctx)
            .await
            .unwrap();

        assert!(!out.is_error);
        assert_eq!(out.metadata["status"], 200);
        assert_eq!(out.metadata["truncated"], true);
        assert_eq!(out.metadata["total_bytes"], 500);
        assert!(out.content.starts_with(&"z".repeat(50)), "{}", out.content);
        assert!(
            out.content
                .contains("[output truncated: showed 50 of 500 bytes"),
            "{}",
            out.content
        );
        assert!(out.content.contains("max_bytes"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_non_success_status_frames_the_body_and_names_the_url() {
        let url = serve("404 Not Found", "nope".to_string()).await;
        let tmp = tempfile::tempdir().unwrap();
        let ctx = permissive_context(tmp.path());

        let out = WebFetch
            .call(json!({ "url": url.clone() }), &ctx)
            .await
            .unwrap();

        assert!(out.is_error);
        assert_eq!(out.metadata["status"], 404);
        assert!(out.content.contains("HTTP 404"), "{}", out.content);
        assert!(out.content.contains(&url), "{}", out.content);
        assert!(out.content.contains("nope"), "{}", out.content);
    }

    /// Every class the rule refuses, through the tool rather than the helper, so
    /// the refusal the model sees is the thing under test.
    #[tokio::test]
    async fn loopback_private_link_local_and_metadata_hosts_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = context(tmp.path());

        let cases = [
            ("http://127.0.0.1:8787/api/health", "loopback"),
            ("http://127.0.0.53/", "loopback"),
            ("http://[::1]:8787/", "loopback"),
            ("http://[::ffff:127.0.0.1]/", "loopback"),
            ("http://localhost/", "loopback"),
            ("http://10.0.0.5/", "private"),
            ("http://172.16.4.4/", "private"),
            ("http://192.168.1.1/", "private"),
            (
                "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
                "link-local",
            ),
            ("http://[fd00:ec2::254]/", "unique-local"),
            ("http://[fe80::1]/", "link-local"),
            ("http://0.0.0.0:8787/", "unspecified"),
            ("http://100.64.0.1/", "carrier-grade NAT"),
        ];

        for (url, expected) in cases {
            let err = WebFetch
                .call(json!({ "url": url }), &ctx)
                .await
                .unwrap_err();
            let message = err.to_string();
            assert!(matches!(err, HarnessError::Tool(_)), "{url}: {message}");
            assert!(message.contains("refusing"), "{url}: {message}");
            assert!(message.contains(expected), "{url}: {message}");
        }
    }

    /// The rule is about the address, not the spelling, so an ordinary public
    /// address passes it — and a literal one needs no DNS to be judged.
    #[test]
    fn public_addresses_pass_the_rule() {
        for literal in [
            "93.184.216.34",
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
            "2a00:1450:4001:800::200e",
        ] {
            let ip: IpAddr = literal.parse().expect("a literal address");
            assert_eq!(blocked_reason(ip), None, "{ip} must not be refused");
        }
    }

    /// The pre-flight check is not the whole defence: the client resolves the
    /// name again when it connects, so the resolver has to refuse too.
    #[tokio::test]
    async fn the_resolver_drops_answers_the_rule_refuses() {
        let name: reqwest::dns::Name = "127.0.0.1".parse().expect("a name");
        let refused = PublicOnlyResolver.resolve(name).await;
        assert!(
            refused.is_err(),
            "a loopback answer must not reach the connection layer"
        );

        let name: reqwest::dns::Name = "localhost".parse().expect("a name");
        let refused = PublicOnlyResolver.resolve(name).await;
        assert!(
            refused.is_err(),
            "a name that only resolves to loopback must not reach the connection layer"
        );
    }
}
