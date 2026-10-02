//! Fixtures shared by the server tests: a hermetic workspace, a mock-configured
//! server bound to a loopback port, and a socket with a few conveniences.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use harness_core::{
    AgentId, ClientEnvelope, ClientMessage, Config, McpServerConfig, Message, PermissionMode,
    ProviderConfig, ProviderKind, ServerEnvelope, ServerMessage, SessionId,
};
use harness_llm::{MockProvider, Provider, ScriptedTurn};
use harness_orchestrator::WorkflowSelector;
use harness_server::{BackpressurePolicy, ProviderFactory, ServerState};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// How long a test waits for a message that should already be on its way.
pub const RECV_TIMEOUT: Duration = Duration::from_secs(20);

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A throwaway workspace: a manifest for the mock script to read and one agent.
pub struct Fixture {
    dir: tempfile::TempDir,
    /// MCP servers a test wants to see in the configuration. Nothing connects
    /// to them: the server only ever reads `config.mcp`.
    mcp: Vec<(String, McpServerConfig)>,
}

impl Fixture {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn db_path(&self) -> PathBuf {
        self.dir.path().join("harness.db")
    }

    /// Declares an MCP server for the tests that inspect configuration.
    pub fn add_mcp_server(&mut self, name: &str, server: McpServerConfig) {
        self.mcp.push((name.to_string(), server));
    }

    /// The mock provider and a temp database, so no test needs a key or a network.
    pub fn config(&self) -> Config {
        let mut config = Config::default();
        config.harness.default_agent = "test".to_string();
        config.harness.max_iterations = 8;
        config.provider.default = "mock".to_string();
        config.providers.insert(
            "mock".to_string(),
            ProviderConfig {
                kind: ProviderKind::Mock,
                default_model: Some("mock-1".to_string()),
                ..Default::default()
            },
        );
        // Absolute, so the store is never written into the repository.
        config.memory.db_path = self.db_path();
        // The server default is ask-when-needed, but these tests have no UI to
        // answer a gate and use `shell` calls to observe abort and queueing. A
        // full-auto fixture keeps them about what they are about; the gate has
        // its own tests.
        config.permissions.mode = Some(PermissionMode::FullAuto);
        config.server.host = "127.0.0.1".to_string();
        config.server.port = 0;
        for (name, server) in &self.mcp {
            config.mcp.servers.insert(name.clone(), server.clone());
        }
        config
    }
}

pub fn workspace() -> Fixture {
    let dir = tempfile::tempdir().expect("a temporary workspace");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("the manifest is writable");
    std::fs::create_dir_all(dir.path().join("agents")).expect("the agents directory is writable");
    std::fs::write(
        dir.path().join("agents/test.agent.md"),
        "---\nname: Test Agent\ndescription: A fixture agent.\n---\nYou are a test agent.\n",
    )
    .expect("the agent file is writable");
    Fixture {
        dir,
        mcp: Vec::new(),
    }
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub state: Arc<ServerState>,
}

impl TestServer {
    pub fn ws_url(&self) -> String {
        format!("ws://{}/ws", self.addr)
    }

    pub fn http_url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

pub async fn start(fixture: &Fixture) -> TestServer {
    start_with(fixture, None, None).await
}

/// Binds `127.0.0.1:0`, so the port is assigned by the OS and the tests can run
/// concurrently.
pub async fn start_with(
    fixture: &Fixture,
    factory: Option<ProviderFactory>,
    policy: Option<BackpressurePolicy>,
) -> TestServer {
    let mut state = ServerState::new(fixture.config(), fixture.path().to_path_buf())
        .await
        .expect("the server state builds");
    if let Some(factory) = factory {
        state = state.with_provider_factory(factory);
    }
    if let Some(policy) = policy {
        state = state.with_policy(policy);
    }

    bind(state).await
}

/// [`start_with`] plus a workflow selector, for the tests that exercise
/// select-then-run.
pub async fn start_with_selector(
    fixture: &Fixture,
    factory: ProviderFactory,
    selector: Arc<dyn WorkflowSelector>,
) -> TestServer {
    let state = ServerState::new(fixture.config(), fixture.path().to_path_buf())
        .await
        .expect("the server state builds")
        .with_provider_factory(factory)
        .with_workflow_selector(selector);

    bind(state).await
}

/// Binds the state to a loopback port and serves it in the background.
async fn bind(state: ServerState) -> TestServer {
    let state = Arc::new(state);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is available");
    let addr = listener.local_addr().expect("the socket is bound");
    let app = Arc::clone(&state).router();
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            eprintln!("test server stopped: {err}");
        }
    });

    TestServer { addr, state }
}

/// Creates a session in the store without the server opening it, the way a
/// session from a previous process exists: a label and some persisted turns,
/// with nothing in the process's in-memory session map.
pub async fn seed_stored_session(state: &ServerState, label: &str, turns: &[Message]) -> SessionId {
    let session_id = state
        .memory()
        .create_session(Some(label))
        .await
        .expect("the session is created");
    for turn in turns {
        state
            .memory()
            .append(session_id, turn)
            .await
            .expect("the turn is appended");
    }
    session_id
}

/// A provider factory that hands every session the same script.
///
/// The mock provider counts calls per instance, so issuing a fresh instance per
/// session is what lets a continuing run pick up where the last one stopped.
pub fn scripting_factory(script: Vec<ScriptedTurn>) -> ProviderFactory {
    Arc::new(move |id, _config, model| {
        Ok(Arc::new(MockProvider::new(id, model, script.clone())) as Arc<dyn Provider>)
    })
}

/// A factory that hands every session one shared instance, so a test can read
/// back what that instance was asked for.
pub fn shared_factory(provider: Arc<dyn Provider>) -> ProviderFactory {
    Arc::new(move |_id, _config, _model| Ok(Arc::clone(&provider)))
}

/// One model turn that calls `name` with `arguments`, then one text turn.
pub fn tool_then_text(name: &str, arguments: Value) -> ProviderFactory {
    scripting_factory(vec![
        ScriptedTurn::ToolCall {
            name: name.to_string(),
            arguments,
        },
        ScriptedTurn::Text("(scripted) done".to_string()),
    ])
}

pub async fn connect(server: &TestServer) -> Socket {
    let (socket, _response) = tokio_tungstenite::connect_async(server.ws_url())
        .await
        .expect("the socket connects");
    socket
}

pub async fn send(socket: &mut Socket, message: ClientMessage) {
    let envelope = ClientEnvelope::new(message);
    write_envelope(socket, envelope).await;
}

/// Sends a frame addressed to a specific session, which need not be the one
/// this connection opened.
pub async fn send_to(socket: &mut Socket, session_id: SessionId, message: ClientMessage) {
    let mut envelope = ClientEnvelope::new(message);
    envelope.session_id = Some(session_id);
    write_envelope(socket, envelope).await;
}

async fn write_envelope(socket: &mut Socket, envelope: ClientEnvelope) {
    // Encoded through `Value` for the same reason the server does it: a
    // `subscribe` message repeats the envelope's `session_id` key, and a direct
    // `to_string` would write it twice.
    let value = serde_json::to_value(&envelope).expect("encodable");
    let encoded = serde_json::to_string(&value).expect("encodable");
    socket
        .send(WsMessage::text(encoded))
        .await
        .expect("the frame is written");
}

/// Reads the next server text frame, skipping the transport-level ones. `None`
/// means the server closed the connection.
pub async fn next_envelope(socket: &mut Socket) -> Option<ServerEnvelope> {
    loop {
        match tokio::time::timeout(RECV_TIMEOUT, socket.next()).await {
            Ok(Some(Ok(WsMessage::Text(text)))) => return Some(decode(text.as_str())),
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(err))) => panic!("the socket failed: {err}"),
            Ok(None) => return None,
            Err(_) => panic!("no message arrived within {RECV_TIMEOUT:?}"),
        }
    }
}

/// Decodes one frame.
///
/// `session_started` is rebuilt from the JSON rather than handed to
/// `ServerEnvelope`'s derived `Deserialize`: the message repeats the envelope's
/// own `session_id` and `agent_id`, and serde's flatten deserializer gives those
/// keys to the envelope and then reports them missing on the message. No JSON
/// shape satisfies both halves, so the frame the server sends is well formed but
/// unrepresentable in `harness-core`'s envelope type.
fn decode(text: &str) -> ServerEnvelope {
    let value: Value = serde_json::from_str(text).expect("a JSON frame");
    match serde_json::from_value::<ServerEnvelope>(value.clone()) {
        Ok(envelope) => envelope,
        Err(err) => {
            assert_eq!(
                value["type"], "session_started",
                "the server sent a frame the tests cannot read: {err}"
            );
            let session_id: SessionId =
                serde_json::from_value(value["session_id"].clone()).expect("a session id");
            let agent_id: AgentId =
                serde_json::from_value(value["agent_id"].clone()).expect("an agent id");
            ServerEnvelope::new(
                session_id,
                agent_id.clone(),
                ServerMessage::SessionStarted {
                    session_id,
                    agent_id,
                    model: value["model"].as_str().unwrap_or_default().to_string(),
                },
            )
        }
    }
}

/// Reads frames until one satisfies `predicate`.
pub async fn next_envelope_matching(
    socket: &mut Socket,
    predicate: impl Fn(&ServerEnvelope) -> bool,
) -> ServerEnvelope {
    loop {
        let envelope = next_envelope(socket)
            .await
            .expect("the server closed early");
        if predicate(&envelope) {
            return envelope;
        }
    }
}

/// The wire tag of a message, as it appears in the JSON.
pub fn kind(envelope: &ServerEnvelope) -> String {
    let value = serde_json::to_value(&envelope.message).expect("serializable");
    value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn error_code(envelope: &ServerEnvelope) -> Option<&str> {
    match &envelope.message {
        ServerMessage::Error { code, .. } => Some(code.as_str()),
        _ => None,
    }
}

/// Polls a predicate that a background task satisfies shortly after `done`.
pub async fn wait_until(what: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Polls the session's stored history until `predicate` accepts it.
pub async fn wait_for_history(
    state: &ServerState,
    session_id: SessionId,
    what: &str,
    predicate: impl Fn(&[Message]) -> bool,
) -> Vec<Message> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let history = state
            .memory()
            .history(session_id, None)
            .await
            .expect("the session is readable");
        if predicate(&history) {
            return history;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {what}; history held {} messages",
                history.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The session id carried by a run's first envelope.
pub fn session_id_of(envelope: &ServerEnvelope) -> SessionId {
    envelope.session_id
}

pub fn steer(text: &str) -> ClientMessage {
    ClientMessage::SteeringMessage {
        text: text.to_string(),
        priority: Default::default(),
    }
}

pub fn user_message(text: &str) -> ClientMessage {
    ClientMessage::UserMessage {
        text: text.to_string(),
        agent_id: None,
        effort: None,
        permission_mode: None,
    }
}

/// A user message that names a reasoning level for its run only.
pub fn user_message_with_effort(text: &str, effort: &str) -> ClientMessage {
    ClientMessage::UserMessage {
        text: text.to_string(),
        agent_id: None,
        effort: Some(effort.to_string()),
        permission_mode: None,
    }
}

pub fn read_file_on_manifest() -> Value {
    json!({ "path": "Cargo.toml" })
}
