//! Serving the built UI.
//!
//! Two sources, tried in order. The embedded copy exists so a release is a
//! single file that cannot be deployed without its assets; the on-disk copy
//! exists so rebuilding a view does not also recompile Rust. The embedded copy
//! wins when both are present, because a binary that silently prefers whatever
//! happens to sit next to it is not the binary that was built.
//!
//! Nothing here is authoritative about routes: the front end uses history mode
//! and refers to its bundles by absolute path, so this is mounted at the root
//! and answers every path the API did not claim with the shell.

use std::path::Path;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use harness_core::path::resolve_within;

use crate::state::ServerState;

/// Where the UI build lands, relative to the workspace root.
const DIST_DIR: &str = "web/dist";

/// Hashed asset names change whenever their contents do, so a copy fetched once
/// stays correct for as long as the URL exists.
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";

/// `index.html` names the hashed bundles. A cached copy would keep a browser
/// asking for files a later build has already deleted, so it must be
/// revalidated on every navigation.
const NO_CACHE: &str = "no-cache";

/// Shown when the workspace has no UI at all. The API is untouched by a missing
/// front end, and saying so is more useful than a bare 404 that reads as a
/// broken server.
const UI_MISSING_PAGE: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>harness ui is not built</title>
    <style>
      body { background: #14161a; color: #d7dae0; font: 15px/1.6 ui-monospace, monospace; margin: 0; padding: 3rem; }
      h1 { font-size: 1.1rem; }
      a { color: #4b9ddd; }
      code { background: #1f232a; padding: .15rem .35rem; border-radius: 4px; }
      pre { background: #1f232a; padding: 1rem; border-radius: 6px; overflow-x: auto; }
    </style>
  </head>
  <body>
    <h1>the harness UI has not been built</h1>
    <p>Build it, then reload this page:</p>
    <pre>cd web
pnpm install
pnpm build</pre>
    <p>If this is a release build, rebuild with the <code>embed-ui</code> feature
    instead, so the UI is compiled into the binary.</p>
    <p>The API is still available — try <a href="/api/health">/api/health</a>.</p>
  </body>
</html>
"#;

/// Answers one request the API did not match.
pub(crate) async fn serve(state: &ServerState, request_path: &str) -> Response {
    let root = state.workspace_root();
    let relative = relative_path(request_path);

    match locate(root, relative).await {
        Found::File(bytes) => asset(bytes, relative),
        // A deep link, a typo, or a workspace with no UI at all: all of them
        // get the shell, which is what lets `/agents` work on a fresh load.
        Found::Missing => shell(root).await,
        // A `..` that resolved outside the dist root. Refused rather than
        // answered with the shell, so an escape attempt is not indistinguishable
        // from an ordinary route.
        Found::Escaped => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "not found",
        )
            .into_response(),
    }
}

/// The request path as a path inside the dist root. `/` means the shell.
fn relative_path(request_path: &str) -> &str {
    let trimmed = request_path.trim_start_matches('/');
    if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    }
}

/// Where one request's bytes came from.
enum Found {
    File(Vec<u8>),
    Missing,
    Escaped,
}

/// The bytes for `relative`: the compiled-in copy first, then the workspace.
async fn locate(root: &Path, relative: &str) -> Found {
    if let Some(bytes) = embedded(relative) {
        return Found::File(bytes);
    }

    let dist = root.join(DIST_DIR);
    let path = match resolve_within(&dist, Path::new(relative)) {
        Ok(path) => path,
        Err(_) => return Found::Escaped,
    };

    match tokio::fs::read(&path).await {
        Ok(bytes) => Found::File(bytes),
        // A missing file, an unreadable one and a directory all mean the same
        // thing here: there is nothing to serve at this path.
        Err(err) => {
            tracing::debug!("no UI asset at {}: {err}", path.display());
            Found::Missing
        }
    }
}

/// One asset, with the caching policy its name implies.
fn asset(bytes: Vec<u8>, relative: &str) -> Response {
    let cache = if relative.starts_with("assets/") {
        IMMUTABLE_CACHE
    } else {
        // Everything else is either the shell or a file without a content hash,
        // and neither can be trusted to be current after a rebuild.
        NO_CACHE
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type(relative)),
            (header::CACHE_CONTROL, cache),
        ],
        bytes,
    )
        .into_response()
}

/// The shell: the built `index.html`, or instructions for building one.
async fn shell(root: &Path) -> Response {
    if let Some(bytes) = embedded("index.html") {
        return asset(bytes, "index.html");
    }
    if let Ok(bytes) = tokio::fs::read(root.join(DIST_DIR).join("index.html")).await {
        return asset(bytes, "index.html");
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, NO_CACHE),
        ],
        UI_MISSING_PAGE,
    )
        .into_response()
}

/// Content type by extension.
///
/// The default is deliberately `application/octet-stream`: a browser refuses to
/// execute a script served under the wrong type, so naming the unknown cases
/// honestly surfaces a bad reference instead of hiding it behind a guess.
fn content_type(relative: &str) -> &'static str {
    match Path::new(relative).extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json") | Some("map") => "application/json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// One file from the compiled-in copy of `web/dist`, when the feature is on.
///
/// The feature is off by default because `rust-embed` reads the folder while
/// compiling: enabling it makes a Rust build fail until the UI has been built.
#[cfg(feature = "embed-ui")]
fn embedded(relative: &str) -> Option<Vec<u8>> {
    EmbeddedUi::get(relative).map(|file| file.data.into_owned())
}

#[cfg(feature = "embed-ui")]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist"]
struct EmbeddedUi;

#[cfg(not(feature = "embed-ui"))]
fn embedded(_relative: &str) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(DIST_DIR)).expect("the dist directory is writable");
        (dir, root)
    }

    #[tokio::test]
    async fn a_parent_directory_escape_is_refused() {
        let (_dir, root) = fixture();
        std::fs::write(root.join("secret.txt"), "top secret").expect("the file is writable");

        assert!(matches!(
            locate(&root, "../secret.txt").await,
            Found::Escaped
        ));
        assert!(matches!(
            locate(&root, "nested/../../secret.txt").await,
            Found::Escaped
        ));
    }

    #[tokio::test]
    async fn a_path_inside_the_dist_root_is_not_an_escape() {
        let (_dir, root) = fixture();
        std::fs::write(root.join(DIST_DIR).join("app.js"), "console.log(1)").expect("the asset");

        match locate(&root, "app.js").await {
            Found::File(bytes) => assert_eq!(bytes, b"console.log(1)"),
            _ => panic!("an existing asset must be found"),
        }
        assert!(matches!(locate(&root, "agents").await, Found::Missing));
    }

    #[test]
    fn content_types_cover_the_kinds_the_build_emits() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("assets/index-abc.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("assets/index-abc.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("logo.svg"), "image/svg+xml");
        assert_eq!(content_type("logo.png"), "image/png");
        assert_eq!(content_type("favicon.ico"), "image/x-icon");
        assert_eq!(content_type("data.json"), "application/json");
        assert_eq!(content_type("index.js.map"), "application/json");
        assert_eq!(content_type("font.woff2"), "font/woff2");
        assert_eq!(content_type("LICENSE"), "application/octet-stream");
    }
}
