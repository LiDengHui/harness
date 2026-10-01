//! The REST surface: process health, what the registry found, what extensions
//! are available, and read-only access to the memory DAG.
//!
//! These routes exist for the things a WebSocket is bad at: a load balancer's
//! health check, a UI that wants the agent list before it opens a socket, and a
//! client that needs to re-read a session after falling behind.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path as AxumPath, Query, State};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use harness_core::{
    HarnessError, HeuristicEstimator, McpServerConfig, McpTransportKind, Message, SessionId,
    PROTOCOL_VERSION,
};
use harness_llm::ChatRequest;
use harness_workflows::WorkflowSpec;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::state::ServerState;

/// Where plugin manifests live, relative to the workspace root.
const PLUGIN_DIR: &str = "plugins";

/// Where a workflow authored through the API is written, relative to the
/// workspace root. The registry also reads `.harness/workflows`, but a new
/// workflow goes beside the checked-in ones so it is reviewable and shareable.
const WORKFLOW_DIR: &str = "workflows";

/// Instructions for the workflow author.
///
/// The reply is parsed as a JSON object rather than as Markdown, because the
/// server needs the fields individually — the id names the file and `when` is
/// what the selector reads — and asking for the document directly would mean
/// parsing frontmatter out of whatever the model decided to emit.
const AUTHOR_SYSTEM_PROMPT: &str = "\
你是一个工作流作者。根据用户给出的描述，写出一个可复用的工作流定义。

规则：
- 只回复一个 JSON 对象，不要有其他文字，不要 Markdown 代码块。
- 字段：
  - id: 小写英文短横线 slug，唯一且能概括这个工作流，例如 db-migration-plan
  - name: 简短名称
  - description: 一句话说明这个工作流做什么
  - when: 什么任务应该用它。这是选择器判断的唯一依据，必须具体，不要写成万能描述
  - guidance: 给整次运行的总指导（Markdown 文本）
  - stages: 至少一个阶段，每个阶段是一个对象：
    - id: 阶段 slug，工作流内唯一
    - objective: 这个阶段要达到什么、怎样算完成
    - agent: 可选，运行它的 .agent.md id
    - depends_on: 可选，必须先完成的阶段 id 列表
    - verify: 可选，验证命令数组，例如 [[\"cargo\", \"test\"]]
- stages 必须构成合法的有向无环图：depends_on 只能引用已存在的阶段 id，且不能有环。";

/// Ceiling on the author's reply: one document's worth of JSON.
const AUTHOR_MAX_TOKENS: u32 = 2_048;

/// Routes without their state applied, so [`crate::state::ServerState::router`]
/// can attach the WebSocket route to the same state type.
pub(crate) fn routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/ui", get(ui))
        .route("/api/agents", get(agents))
        .route("/api/extensions", get(extensions))
        .route("/api/workflows", get(workflows).post(create_workflow))
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/{id}/history", get(session_history))
        .route("/api/memory/stats", get(memory_stats))
        .fallback(fallback)
}

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "protocol": PROTOCOL_VERSION,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Proof that a request carried the token this server requires, when one is
/// configured.
///
/// An extractor rather than a middleware layer: the state a layer would need is
/// not available where these routes are built, and naming it in each handler's
/// signature keeps the requirement visible at the handler itself.
///
/// [`health`] is deliberately left out. A load balancer's probe has to answer
/// without a secret, and it reports nothing but a version. The static UI is out
/// for a different reason: it is the app's code rather than the user's data, and
/// its bundle requests carry no query string, so gating it would leave a blank
/// page.
pub(crate) struct Authorized;

impl FromRequestParts<Arc<ServerState>> for Authorized {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<ServerState>,
    ) -> std::result::Result<Self, Self::Rejection> {
        // Parsed with the same extractor the WebSocket upgrade uses, so the two
        // doors accept exactly the same spellings of the token.
        let query = Query::<crate::ws::UpgradeParams>::try_from_uri(&parts.uri)
            .ok()
            .and_then(|Query(params)| params.token);

        match crate::ws::token_allowed(state, &parts.headers, query.as_deref()) {
            Ok(()) => Ok(Authorized),
            Err(reason) => {
                tracing::warn!("refused an API request: {reason}");
                Err(crate::ws::refusal(StatusCode::UNAUTHORIZED, &reason))
            }
        }
    }
}

/// What the browser UI starts in: the house language, already resolved to a tag
/// the built UI can render, and the reasoning levels it may offer.
///
/// A plain read of the loaded configuration — the front end asks for this on
/// every cold boot, so it must not touch disk or open a session.
async fn ui(_: Authorized, State(state): State<Arc<ServerState>>) -> Json<Value> {
    Json(json!({
        "language": state.config().ui.resolved_language(),
        // Served rather than hardcoded in the front end, so the picker and the
        // validator cannot drift from the set the server actually accepts.
        "thinking": {
            "efforts": harness_core::SUPPORTED_THINKING_EFFORTS,
            "default": state.config().thinking.resolved_effort(),
        },
    }))
}

async fn agents(_: Authorized, State(state): State<Arc<ServerState>>) -> Json<Value> {
    let specs: Vec<Value> = state
        .registry()
        .list()
        .into_iter()
        .map(|spec| {
            json!({
                "id": spec.id,
                "name": spec.name,
                "description": spec.description,
                "model": spec.model,
                "tools": spec.tools,
                "skills": spec.skills,
                "max_tokens": spec.max_tokens,
                // Canonicalised, so the UI never shows a level the server
                // would refuse to send.
                "thinking_effort": spec
                    .thinking_effort
                    .as_deref()
                    .and_then(harness_core::resolve_thinking_effort),
                "source_path": spec.source_path.display().to_string(),
            })
        })
        .collect();
    Json(Value::Array(specs))
}

async fn extensions(
    _: Authorized,
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, ApiError> {
    // The estimator is stateless and its defaults are the ones the CLI prints,
    // so the numbers the UI shows match `harness skill list`.
    let estimator = HeuristicEstimator::default();
    let skills: Vec<Value> = state
        .skills()
        .list()
        .into_iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "model": spec.model,
                "allowed_tools": spec.allowed_tools,
                "metadata_tokens": spec.metadata_tokens(&estimator),
                "body_tokens": spec.body_tokens(&estimator),
                "source_path": spec.source_path.display().to_string(),
            })
        })
        .collect();

    let mcp: Vec<Value> = state
        .config()
        .mcp
        .servers
        .iter()
        .map(|(name, server)| {
            json!({
                "name": name,
                "kind": mcp_kind(server),
                "enabled": server.enabled,
                "lazy": server.lazy,
                "target": server
                    .url
                    .clone()
                    .or_else(|| server.command.clone())
                    .unwrap_or_default(),
                // What this server would expose is only knowable by asking it,
                // and an HTTP handler must not open a connection to a
                // third-party process on behalf of a page load. An empty list
                // is the honest answer; `harness mcp list` is the command that
                // connects and prints the real names.
                "tools": Vec::<String>::new(),
            })
        })
        .collect();

    Ok(Json(json!({
        "skills": skills,
        "mcp": mcp,
        "plugins": plugin_views(state.workspace_root())?,
    })))
}

/// The workflows this workspace declares.
///
/// A bare array, like `/api/agents`. An absent registry answers with an empty
/// list rather than a 404: the route exists, this server just has no workflows
/// wired in, and the picker's documented fallback is to offer automatic
/// selection alone.
async fn workflows(_: Authorized, State(state): State<Arc<ServerState>>) -> Json<Value> {
    let workflows: Vec<Value> = state
        .workflow_registry()
        .map(|registry| registry.list().into_iter().map(workflow_view).collect())
        .unwrap_or_default();
    Json(Value::Array(workflows))
}

/// One workflow as the list and the create reply both report it.
///
/// `when` and `description` travel with the entry because they are the only
/// text a reader can choose between; the stages and the guidance are the run's
/// business and would only bloat the picker.
fn workflow_view(spec: &WorkflowSpec) -> Value {
    json!({
        "id": spec.id,
        "name": spec.name,
        "when": spec.when,
        "description": spec.description,
        "source_path": spec.source_path.display().to_string(),
    })
}

/// The body of `POST /api/workflows`.
#[derive(Debug, Deserialize)]
struct CreateWorkflow {
    description: String,
}

/// Authors a workflow from a description and saves it.
///
/// The model's reply is parsed and validated before anything is written:
/// [`harness_workflows::WorkflowRegistry::save_new`] renders the document and
/// reads it back through the loader before touching disk, so a malformed reply
/// is an error and never a half-written file.
async fn create_workflow(
    _: Authorized,
    State(state): State<Arc<ServerState>>,
    Json(body): Json<CreateWorkflow>,
) -> Result<Json<Value>, ApiError> {
    let description = body.description.trim();
    if description.is_empty() {
        return Err(ApiError::bad_request(
            "the description must not be empty",
            "a workflow is authored from a non-empty description",
        ));
    }

    let registry = state.workflow_registry().ok_or_else(|| {
        ApiError::bad_request(
            "workflows are not available on this server",
            "no workflow registry is installed",
        )
    })?;

    let spec = author_workflow(&state, description).await?;
    let dir = state.workspace_root().join(WORKFLOW_DIR);
    let path = registry
        .save_new(&spec, &dir)
        .map_err(|err| ApiError::internal("the workflow could not be saved", err))?;

    // The saved path is what the registry will report from now on, so the reply
    // carries it rather than the placeholder the spec was built with.
    let mut created = spec;
    created.source_path = path;

    // Swap the registry in memory for one that includes the new file. Without
    // this the workflow exists on disk and the selector cannot choose it until
    // the next restart. Best effort: a failed reload keeps the loaded registry.
    state.reload_workflows();

    Ok(Json(workflow_view(&created)))
}

/// Asks the configured model for a workflow and reads it back as a spec.
///
/// A reply that is not a JSON object, or is a JSON object that cannot be read
/// as a workflow, is an error rather than a guess: the client asked for a
/// workflow to be created, and inventing one would save a procedure nobody
/// wrote.
async fn author_workflow(state: &ServerState, description: &str) -> Result<WorkflowSpec, ApiError> {
    let (provider, model) = state
        .authoring_provider()
        .map_err(|err| ApiError::internal("no provider is available to author a workflow", err))?;

    let mut request = ChatRequest::new(
        model,
        vec![
            Message::system(AUTHOR_SYSTEM_PROMPT),
            Message::user(format!("请为下面的任务描述写一个工作流：\n\n{description}")),
        ],
    );
    request.temperature = Some(0.2);
    request.max_tokens = Some(AUTHOR_MAX_TOKENS);

    // The sink must outlive the call: a provider that cannot emit its deltas
    // treats a closed channel as a failure.
    let (events, _rx) = mpsc::unbounded_channel();
    let reply = provider
        .stream(request, events)
        .await
        .map_err(|err| ApiError::authoring("the model could not author a workflow", err))?;

    let json = first_json_object(reply.message.text()).ok_or_else(|| {
        ApiError::authoring(
            "the model's reply was not a workflow",
            format!(
                "no JSON object in the reply:\n{}",
                reply.message.text().trim()
            ),
        )
    })?;

    let mut spec: WorkflowSpec = serde_json::from_str(json).map_err(|err| {
        ApiError::authoring(
            "the model's reply was not a workflow",
            format!("the JSON could not be read as a workflow: {err}"),
        )
    })?;

    // Trimmed before saving so the id names the file exactly and the two texts
    // the selector reads are not padded with whitespace. The required fields are
    // not checked here: `save_new` refuses an empty `when`, `description` or
    // stage list, and doing it in one place keeps the rule in the loader.
    spec.id = spec.id.trim().to_string();
    spec.name = spec.name.trim().to_string();
    spec.description = spec.description.trim().to_string();
    spec.when = spec.when.trim().to_string();
    spec.guidance = spec.guidance.trim().to_string();
    for stage in &mut spec.stages {
        stage.id = stage.id.trim().to_string();
        stage.objective = stage.objective.trim().to_string();
    }

    Ok(spec)
}

/// The first balanced `{...}` in `reply`, ignoring braces inside strings.
///
/// Models wrap JSON in prose or a fenced block often enough that the workflow
/// selector has the same helper. That one is private to `harness-workflows`,
/// so this keeps its own copy rather than widening that crate's API for twenty
/// lines.
fn first_json_object(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, byte) in reply.as_bytes()[start..].iter().enumerate() {
        if in_string {
            match byte {
                b'\\' if !escaped => escaped = true,
                b'"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    // `{` and `}` are ASCII, so these offsets are char boundaries.
                    return Some(&reply[start..start + offset + 1]);
                }
            }
            _ => {}
        }
    }

    None
}

/// The transport as the UI labels it.
///
/// A server naming both transports or neither cannot be reached at all. The CLI
/// calls that `invalid` and the UI renders it as a broken entry, which is more
/// useful than picking one of the two and hiding the misconfiguration.
fn mcp_kind(server: &McpServerConfig) -> &'static str {
    match server.transport_kind() {
        Ok(McpTransportKind::Stdio) => "stdio",
        Ok(McpTransportKind::Http) => "http",
        Err(_) => "invalid",
    }
}

/// The manifest fields the UI shows.
///
/// A private copy of `harness_sandbox::PluginManifest` rather than an import:
/// the sandbox crate carries wasmtime, and four strings do not justify linking a
/// WebAssembly runtime into every server build. The duplication is pinned by a
/// test that reads this repository's own plugin manifests and compares them with
/// what this view produces.
#[derive(Debug, Deserialize)]
struct PluginManifestView {
    name: String,
    version: String,
    entry: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

/// Every plugin declared under `<workspace_root>/plugins`, in name order.
///
/// A missing directory means no plugins, which is a normal workspace rather than
/// an error. A manifest that cannot be parsed is an error, for the same reason a
/// malformed skill fails the whole skill load: a half-read declaration would be
/// reported as a plugin with missing fields.
fn plugin_views(root: &Path) -> Result<Vec<Value>, ApiError> {
    let dir = root.join(PLUGIN_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(ApiError::extension(
                "the plugins directory could not be read",
                err,
            ))
        }
    };

    let mut found = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|err| ApiError::extension("the plugins directory could not be read", err))?
            .path();
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(name) = stem.strip_suffix(".plugin") else {
            continue;
        };

        let text = std::fs::read_to_string(&path)
            .map_err(|err| ApiError::extension("a plugin manifest could not be read", err))?;
        let manifest: PluginManifestView = toml::from_str(&text).map_err(|err| {
            ApiError::extension(
                "a plugin manifest is not valid",
                format!("{}: {err}", path.display()),
            )
        })?;

        found.push((manifest, module_path(&dir, name, root)));
    }

    found.sort_by(|(a, _), (b, _)| a.name.cmp(&b.name));

    Ok(found
        .into_iter()
        .map(|(manifest, module)| {
            json!({
                "name": manifest.name,
                "version": manifest.version,
                "entry": manifest.entry,
                "capabilities": manifest.capabilities,
                "module": module,
            })
        })
        .collect())
}

/// The module beside a manifest, as a workspace-relative path.
///
/// The module is not referenced by the manifest — the CLI looks for a sibling
/// with the same stem — so a manifest whose module is missing still gets the
/// conventional `.wat` path rather than dropping the plugin from the listing.
fn module_path(dir: &Path, name: &str, root: &Path) -> String {
    let module = ["wat", "wasm"]
        .iter()
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| dir.join(format!("{name}.wat")));

    module
        .strip_prefix(root)
        .unwrap_or(&module)
        .to_string_lossy()
        .replace('\\', "/")
}

async fn sessions(
    _: Authorized,
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, ApiError> {
    let sessions = state.memory().sessions().await.map_err(ApiError::memory)?;
    Ok(Json(json!(sessions)))
}

/// One session's full conversation, for replay.
///
/// The summary travels beside the messages so a client that opened a deep link
/// gets both without a second request. The chain is the session's effective
/// history — inherited ancestors included — and it carries tool calls and tool
/// results as well as the text, because a replayed run is unreadable without the
/// tool turns that produced it.
async fn session_history(
    _: Authorized,
    State(state): State<Arc<ServerState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let session_id: SessionId = id.parse().map_err(|_| ApiError::unknown_session(&id))?;
    let Some(session) = state
        .memory()
        .session(session_id)
        .await
        .map_err(ApiError::memory)?
    else {
        return Err(ApiError::unknown_session(&id));
    };

    let messages = state
        .memory()
        .history(session_id, None)
        .await
        .map_err(ApiError::memory)?;

    Ok(Json(json!({ "session": session, "messages": messages })))
}

async fn memory_stats(
    _: Authorized,
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, ApiError> {
    let stats = state.memory().stats().await.map_err(ApiError::memory)?;
    Ok(Json(json!(stats)))
}

/// Answers every request no route matched.
///
/// The API keeps its JSON 404, because a client parsing a response body must
/// never be handed HTML; everything else is a deep link into the single-page
/// UI, which the asset handler answers with the shell so `/agents` survives a
/// reload.
async fn fallback(State(state): State<Arc<ServerState>>, method: Method, uri: Uri) -> Response {
    if is_api_path(uri.path()) {
        not_found(method, uri).await
    } else {
        crate::assets::serve(&state, uri.path()).await
    }
}

fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

async fn not_found(method: Method, uri: Uri) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("no route for {method} {uri}") })),
    )
        .into_response()
}

/// A failure a client may see.
///
/// The body carries a fixed sentence and the detail goes to the log instead: an
/// error string from the provider or memory layer can quote a URL, a filesystem
/// path or a header, and none of that belongs in an HTTP response from a server
/// that might be facing a network.
#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    public: &'static str,
    detail: String,
}

impl ApiError {
    fn memory(err: HarnessError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            public: "memory query failed",
            detail: err.to_string(),
        }
    }

    /// A session id that names nothing, or does not name a session at all.
    ///
    /// A client-visible 404 rather than the 500 a failed lookup would otherwise
    /// be: a malformed id and a deleted session are the same answer to a reader,
    /// and neither is a fault in this server.
    fn unknown_session(id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            public: "no such session",
            detail: format!("`{id}` does not name a session"),
        }
    }

    /// A failure while reading an extension declaration off disk.
    fn extension(public: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            public,
            detail: detail.to_string(),
        }
    }

    /// A request that cannot be served as sent.
    fn bad_request(public: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            public,
            detail: detail.to_string(),
        }
    }

    /// The model that was asked to produce something produced nothing usable.
    ///
    /// A gateway rather than a server error: this server did its part, and the
    /// upstream reply is what failed to satisfy the contract.
    fn authoring(public: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            public,
            detail: detail.to_string(),
        }
    }

    /// A server-side failure with no more specific status.
    fn internal(public: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            public,
            detail: detail.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // A client mistake is not a server fault. Logging a stale link at error
        // level would make a routine 404 look like an outage in the log.
        if self.status.is_client_error() {
            tracing::debug!(status = %self.status, "{}: {}", self.public, self.detail);
        } else {
            tracing::error!(status = %self.status, "{}: {}", self.public, self.detail);
        }
        (self.status, Json(json!({ "error": self.public }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ProviderFactory;
    use harness_core::{Config, ProviderConfig, ProviderKind};
    use harness_llm::{MockProvider, Provider, ScriptedTurn};
    use harness_workflows::WorkflowRegistry;
    use std::path::PathBuf;

    /// This repository's root, as seen from the crate.
    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The repository's own `plugins/*.plugin.toml` files, in name order.
    fn repo_manifests() -> Vec<(PathBuf, String)> {
        let dir = repo_root().join(PLUGIN_DIR);
        let mut found: Vec<(PathBuf, String)> = std::fs::read_dir(&dir)
            .expect("this repository has a plugins directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".plugin.toml"))
            })
            .map(|path| {
                let text = std::fs::read_to_string(&path).expect("a readable manifest");
                (path, text)
            })
            .collect();
        found.sort();
        assert!(!found.is_empty(), "the repository declares no plugins");
        found
    }

    /// The point of the private view struct is that it reads the same manifests
    /// `harness plugin list` reads. This compares it against the raw TOML, so a
    /// field that the sandbox manifest renames would show up here.
    #[test]
    fn the_plugin_view_matches_the_manifests_in_this_repository() {
        let root = repo_root();
        let views = plugin_views(&root).expect("this repository's plugins parse");
        let manifests = repo_manifests();
        assert_eq!(views.len(), manifests.len(), "{views:#?}");

        for (path, text) in &manifests {
            let declared: toml::Value = toml::from_str(text).expect("the manifest is valid TOML");
            let table = declared.as_table().expect("a TOML table");
            let name = table["name"].as_str().expect("a declared name");

            let view = views
                .iter()
                .find(|view| view["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("no view for plugin `{name}` ({})", path.display()));

            assert_eq!(
                view["version"],
                table["version"].as_str().expect("a version")
            );
            assert_eq!(
                view["entry"],
                table["entry"].as_str().expect("an entry point")
            );
            let capabilities: Vec<&str> = table["capabilities"]
                .as_array()
                .expect("a capability list")
                .iter()
                .map(|value| value.as_str().expect("a capability name"))
                .collect();
            assert_eq!(
                view["capabilities"],
                json!(capabilities),
                "{}",
                path.display()
            );

            let module = view["module"].as_str().expect("a module path");
            assert!(module.starts_with("plugins/"), "{module}");
            assert!(
                root.join(module).is_file(),
                "{} does not exist",
                root.join(module).display()
            );
        }
    }

    /// Capabilities are reported exactly as the manifest spells them, which is
    /// the snake_case the sandbox enum serializes to.
    #[test]
    fn capabilities_keep_the_spelling_the_manifest_uses() {
        let views = plugin_views(&repo_root()).expect("this repository's plugins parse");

        let snoop = views
            .iter()
            .find(|view| view["name"] == "file_snoop")
            .expect("the file_snoop plugin");
        assert_eq!(snoop["capabilities"], json!(["file_read"]));
        assert_eq!(snoop["module"], "plugins/file_snoop.wat");

        let echo = views
            .iter()
            .find(|view| view["name"] == "echo")
            .expect("the echo plugin");
        assert_eq!(echo["capabilities"], json!([]));
    }

    #[test]
    fn a_workspace_without_plugins_has_none() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let views = plugin_views(dir.path()).expect("a missing directory is not an error");
        assert!(views.is_empty());
    }

    #[test]
    fn only_api_paths_keep_the_json_404() {
        assert!(is_api_path("/api"));
        assert!(is_api_path("/api/nope"));
        assert!(!is_api_path("/apiary"));
        assert!(!is_api_path("/agents"));
        assert!(!is_api_path("/"));
    }

    /// A workflow document the loader accepts, used to prove the list route
    /// reports what the registry holds.
    const SAMPLE_WORKFLOW: &str = "\
---
id: sample
name: Sample Procedure
description: A procedure used by the tests.
when: When a test needs a workflow.
stages:
  - id: first
    objective: Do the first thing.
---

Run the stage.
";

    /// A server whose only provider answers `reply`, with a registry loaded from
    /// `dir`. Mirrors the test fixture in `state.rs` so no key or network is
    /// needed.
    async fn workflow_state(dir: &Path, reply: &str) -> ServerState {
        let mut config = Config::default();
        config.harness.default_agent = "test".to_string();
        config.provider.default = "mock".to_string();
        config.providers.insert(
            "mock".to_string(),
            ProviderConfig {
                kind: ProviderKind::Mock,
                default_model: Some("mock-1".to_string()),
                ..Default::default()
            },
        );
        config.memory.db_path = dir.join("harness.db");

        // Owned before the closure so the factory is `'static`, which is what
        // `ProviderFactory` requires.
        let reply = reply.to_string();
        let factory: ProviderFactory = Arc::new(move |id, _config, model| {
            Ok(Arc::new(MockProvider::new(
                id,
                model,
                vec![ScriptedTurn::Text(reply.clone())],
            )) as Arc<dyn Provider>)
        });

        let registry = Arc::new(WorkflowRegistry::load(dir).expect("the registry loads"));

        ServerState::new(config, dir.to_path_buf())
            .await
            .expect("the state builds")
            .with_provider_factory(factory)
            .with_workflow_registry(registry)
    }

    #[tokio::test]
    async fn the_workflow_list_reports_the_registrys_entries() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        std::fs::create_dir_all(dir.path().join(WORKFLOW_DIR)).expect("the workflows directory");
        std::fs::write(
            dir.path().join(WORKFLOW_DIR).join("sample.workflow.md"),
            SAMPLE_WORKFLOW,
        )
        .expect("the workflow file");

        let state = Arc::new(workflow_state(dir.path(), "{}").await);
        let Json(value) = workflows(Authorized, State(state)).await;
        let list = value.as_array().expect("a list");

        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["id"], "sample");
        assert_eq!(list[0]["name"], "Sample Procedure");
        assert_eq!(list[0]["when"], "When a test needs a workflow.");
        assert_eq!(list[0]["description"], "A procedure used by the tests.");
        assert!(list[0]["source_path"]
            .as_str()
            .expect("a source path")
            .ends_with("sample.workflow.md"));
    }

    #[tokio::test]
    async fn a_server_with_no_registry_lists_nothing_rather_than_failing() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let state = Arc::new(
            ServerState::new(Config::default(), dir.path().to_path_buf())
                .await
                .expect("the state builds"),
        );

        let Json(value) = workflows(Authorized, State(state)).await;
        assert_eq!(value, json!([]));
    }

    #[tokio::test]
    async fn creating_a_workflow_authors_it_and_writes_the_file() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let reply = json!({
            "id": "db-migration-plan",
            "name": "Database migration plan",
            "description": "Plan a database migration in stages.",
            "when": "A schema change needs a staged, reviewable plan.",
            "guidance": "Work stage by stage; apply nothing before it is verified.",
            "stages": [
                { "id": "plan", "objective": "Write the migration plan.", "verify": [["cargo", "test"]] },
                { "id": "apply", "objective": "Apply the migration.", "depends_on": ["plan"] }
            ]
        })
        .to_string();
        let state = Arc::new(workflow_state(dir.path(), &reply).await);

        let Json(created) = create_workflow(
            Authorized,
            State(Arc::clone(&state)),
            Json(CreateWorkflow {
                description: "整理一次数据库迁移的分阶段计划".to_string(),
            }),
        )
        .await
        .expect("the workflow is created");

        assert_eq!(created["id"], "db-migration-plan");
        assert_eq!(
            created["when"],
            "A schema change needs a staged, reviewable plan."
        );
        assert_eq!(
            created["description"],
            "Plan a database migration in stages."
        );

        let path = dir
            .path()
            .join(WORKFLOW_DIR)
            .join("db-migration-plan.workflow.md");
        assert!(path.is_file(), "{} must exist", path.display());

        // The file loads through the same registry the selector chooses from, so
        // the workflow a client just created can actually be run.
        let reloaded = WorkflowRegistry::load(dir.path()).expect("the registry reloads");
        let spec = reloaded
            .get("db-migration-plan")
            .expect("the saved workflow is loadable");
        assert_eq!(spec.stages.len(), 2);
        assert_eq!(spec.stages[1].depends_on, vec!["plan".to_string()]);
    }

    #[tokio::test]
    async fn a_malformed_reply_is_an_error_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let state = Arc::new(workflow_state(dir.path(), "我觉得可以这样做").await);

        let result = create_workflow(
            Authorized,
            State(state),
            Json(CreateWorkflow {
                description: "随便写一个".to_string(),
            }),
        )
        .await;

        assert!(result.is_err(), "a reply with no JSON must be refused");
        assert!(
            !dir.path().join(WORKFLOW_DIR).exists(),
            "nothing may be written for a reply that is not a workflow"
        );
    }

    #[tokio::test]
    async fn a_reply_that_is_not_a_valid_workflow_writes_nothing() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        // Valid JSON, but no stages: the loader refuses it, so no file may
        // appear — a half-written procedure is worse than a refused one.
        let reply = json!({
            "id": "broken",
            "name": "Broken",
            "description": "d",
            "when": "w",
            "stages": []
        })
        .to_string();
        let state = Arc::new(workflow_state(dir.path(), &reply).await);

        let result = create_workflow(
            Authorized,
            State(state),
            Json(CreateWorkflow {
                description: "x".to_string(),
            }),
        )
        .await;

        assert!(result.is_err());
        assert!(!dir.path().join(WORKFLOW_DIR).exists());
    }

    /// The token protects the data routes too, not only the bus: conversation
    /// history is on this surface, and "exposed to the LAN" must not mean
    /// "readable from the LAN".
    #[tokio::test]
    async fn an_api_request_needs_the_configured_token() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let mut config = Config::default();
        config.memory.db_path = dir.path().join("harness.db");
        config.server.token = Some("s3cret".to_string());
        let state = Arc::new(
            ServerState::new(config, dir.path().to_path_buf())
                .await
                .expect("the state builds"),
        );

        let (mut anonymous, _) = axum::http::Request::builder()
            .uri("/api/agents")
            .body(())
            .expect("a request")
            .into_parts();
        assert!(
            Authorized::from_request_parts(&mut anonymous, &state)
                .await
                .is_err(),
            "a request without the token must be refused"
        );

        let (mut in_query, _) = axum::http::Request::builder()
            .uri("/api/agents?token=s3cret")
            .body(())
            .expect("a request")
            .into_parts();
        assert!(
            Authorized::from_request_parts(&mut in_query, &state)
                .await
                .is_ok(),
            "the token in the query must be accepted"
        );

        let (mut in_header, _) = axum::http::Request::builder()
            .uri("/api/agents")
            .header("x-harness-token", "s3cret")
            .body(())
            .expect("a request")
            .into_parts();
        assert!(
            Authorized::from_request_parts(&mut in_header, &state)
                .await
                .is_ok(),
            "the token in the header must be accepted"
        );
    }

    /// A server with no token configured — the loopback default — is unchanged.
    #[tokio::test]
    async fn an_api_request_is_open_when_no_token_is_configured() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let mut config = Config::default();
        config.memory.db_path = dir.path().join("harness.db");
        let state = Arc::new(
            ServerState::new(config, dir.path().to_path_buf())
                .await
                .expect("the state builds"),
        );

        let (mut parts, _) = axum::http::Request::builder()
            .uri("/api/agents")
            .body(())
            .expect("a request")
            .into_parts();
        assert!(Authorized::from_request_parts(&mut parts, &state)
            .await
            .is_ok());
    }

    #[test]
    fn the_first_json_object_is_extracted_from_prose() {
        assert_eq!(
            first_json_object("当然：\n```json\n{\"id\": \"a\"}\n```\n"),
            Some(r#"{"id": "a"}"#)
        );
        assert_eq!(
            first_json_object(r#"{"stages": [{"id": "a}b"}]}"#),
            Some(r#"{"stages": [{"id": "a}b"}]}"#),
            "braces inside strings do not close the object"
        );
        assert_eq!(first_json_object("no braces"), None);
    }
}
