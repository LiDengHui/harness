//! The UI shell and the assets it points at, over a real loopback connection.
//!
//! The disk-source tests are skipped when `embed-ui` is on: the compiled-in copy
//! then outranks the workspace by design, so a fixture `web/dist` would never be
//! read and the assertions below would be about the wrong source.

mod support;

use support::{start, workspace};

#[cfg(not(feature = "embed-ui"))]
use support::Fixture;

#[cfg(not(feature = "embed-ui"))]
const SHELL: &str = "<!doctype html><html><body><div id=\"app\"></div></body></html>";

/// Lays down a `web/dist` that looks like a real build: an absolute-path shell
/// and one content-hashed bundle under `/assets/`.
#[cfg(not(feature = "embed-ui"))]
fn build_ui(fixture: &Fixture) {
    let dist = fixture.path().join("web/dist");
    std::fs::create_dir_all(dist.join("assets")).expect("the dist tree is writable");
    std::fs::write(dist.join("index.html"), SHELL).expect("the shell is writable");
    std::fs::write(dist.join("assets/index-abc123.js"), "console.log('ui');")
        .expect("the asset is writable");
}

async fn fetch(server: &support::TestServer, path: &str) -> reqwest::Response {
    reqwest::get(server.http_url(path))
        .await
        .expect("the request completes")
}

fn header(response: &reqwest::Response, name: reqwest::header::HeaderName) -> String {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

#[cfg(not(feature = "embed-ui"))]
#[tokio::test]
async fn the_root_serves_the_shell_and_refuses_to_cache_it() {
    let fixture = workspace();
    build_ui(&fixture);
    let server = start(&fixture).await;

    let response = fetch(&server, "/").await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        header(&response, reqwest::header::CONTENT_TYPE),
        "text/html; charset=utf-8"
    );
    assert_eq!(
        header(&response, reqwest::header::CACHE_CONTROL),
        "no-cache"
    );
    assert_eq!(response.text().await.expect("a body"), SHELL);
}

#[cfg(not(feature = "embed-ui"))]
#[tokio::test]
async fn a_deep_link_is_answered_with_the_shell() {
    let fixture = workspace();
    build_ui(&fixture);
    let server = start(&fixture).await;

    for path in ["/agents", "/extensions", "/some/deep/link"] {
        let response = fetch(&server, path).await;
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(
            header(&response, reqwest::header::CONTENT_TYPE),
            "text/html; charset=utf-8",
            "{path}"
        );
        assert_eq!(response.text().await.expect("a body"), SHELL, "{path}");
    }
}

#[cfg(not(feature = "embed-ui"))]
#[tokio::test]
async fn a_hashed_asset_is_served_with_its_type_and_cached_forever() {
    let fixture = workspace();
    build_ui(&fixture);
    let server = start(&fixture).await;

    let response = fetch(&server, "/assets/index-abc123.js").await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        header(&response, reqwest::header::CONTENT_TYPE),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        header(&response, reqwest::header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(response.text().await.expect("a body"), "console.log('ui');");
}

#[tokio::test]
async fn an_api_path_is_never_answered_with_the_shell() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let response = fetch(&server, "/api/nope").await;
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        header(&response, reqwest::header::CONTENT_TYPE),
        "application/json"
    );

    let body: serde_json::Value = response.json().await.expect("a JSON body");
    assert!(
        body["error"]
            .as_str()
            .expect("an error string")
            .contains("/api/nope"),
        "{body}"
    );
}

#[cfg(not(feature = "embed-ui"))]
#[tokio::test]
async fn a_workspace_without_a_ui_explains_how_to_build_one() {
    let fixture = workspace();
    let server = start(&fixture).await;

    let response = fetch(&server, "/").await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        header(&response, reqwest::header::CONTENT_TYPE),
        "text/html; charset=utf-8"
    );

    let body = response.text().await.expect("a body");
    assert!(body.contains("pnpm build"), "{body}");
    assert!(body.contains("/api/health"), "{body}");
}

/// The other half of "two sources, tried in order": with the UI compiled in, a
/// workspace that has no `web/dist` still gets a real shell.
#[cfg(feature = "embed-ui")]
#[tokio::test]
async fn the_embedded_copy_answers_when_the_workspace_has_no_ui() {
    let fixture = workspace();
    let server = start(&fixture).await;

    for path in ["/", "/agents"] {
        let response = fetch(&server, path).await;
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(
            header(&response, reqwest::header::CONTENT_TYPE),
            "text/html; charset=utf-8",
            "{path}"
        );
        assert_eq!(
            header(&response, reqwest::header::CACHE_CONTROL),
            "no-cache",
            "{path}"
        );
        let body = response.text().await.expect("a body");
        assert!(body.contains("/assets/index-"), "{path}: {body}");
    }
}
