//! End-to-end orchestration: a scripted provider drives the real executor, the
//! real agent loop, the real verifier and — where it matters — real git
//! worktrees.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::BoxFuture;
use harness_agent::{AgentRegistry, ControlHandle, ModelRouter};
use harness_core::{
    AgentEvent, AgentId, Config, HarnessError, Memory, Message, ProviderConfig, ProviderKind, Role,
    RoutedEvent, SessionInfo, SubtaskStatus, TokenUsage, ToolCall,
};
use harness_llm::{ChatRequest, Provider, ProviderEvent, ProviderResponse, ScriptedTurn};
use harness_memory::SqliteMemory;
use harness_orchestrator::{
    ExecutionReport, Executor, ExecutorConfig, Planner, SubtaskResult, TaskGraph, TaskNode,
    Verifier, VerifierConfig, WorktreeRef,
};
use serde_json::json;
use tokio::sync::{mpsc, Barrier};

// ---------------------------------------------------------------------------
// A provider whose reply is computed from the request, so one instance can serve
// several concurrent nodes with different behaviour.
// ---------------------------------------------------------------------------

type Responder = Arc<
    dyn Fn(ChatRequest) -> BoxFuture<'static, harness_core::Result<ProviderResponse>> + Send + Sync,
>;

struct TestProvider {
    id: String,
    model: String,
    responder: Responder,
    seen: Mutex<Vec<ChatRequest>>,
}

impl TestProvider {
    /// A provider that identifies itself as `mock` — the router's default
    /// provider — so a node that names no model resolves to the executor's own
    /// identity and runs.
    fn new(
        model: &str,
        responder: impl Fn(ChatRequest) -> BoxFuture<'static, harness_core::Result<ProviderResponse>>
            + Send
            + Sync
            + 'static,
    ) -> Arc<Self> {
        Self::named("mock", model, responder)
    }

    /// A provider whose identity is not the router's default, for the tests that
    /// exercise the executor's provider check.
    fn named(
        id: &str,
        model: &str,
        responder: impl Fn(ChatRequest) -> BoxFuture<'static, harness_core::Result<ProviderResponse>>
            + Send
            + Sync
            + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_string(),
            model: model.to_string(),
            responder: Arc::new(responder),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn constant(model: &str, reply: String) -> Arc<Self> {
        Self::constant_named("mock", model, reply)
    }

    fn constant_named(id: &str, model: &str, reply: String) -> Arc<Self> {
        Self::named(id, model, move |_| {
            let reply = reply.clone();
            Box::pin(async move { Ok(text_reply(reply)) })
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.seen.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Provider for TestProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn stream(
        &self,
        request: ChatRequest,
        _events: mpsc::UnboundedSender<ProviderEvent>,
    ) -> harness_core::Result<ProviderResponse> {
        self.seen.lock().expect("lock").push(request.clone());
        (self.responder)(request).await
    }
}

fn text_reply(text: String) -> ProviderResponse {
    ProviderResponse {
        message: Message::assistant(text),
        tool_calls: Vec::new(),
        usage: TokenUsage::new(10, 5),
        finish_reason: Some("stop".into()),
    }
}

fn tool_reply(name: &str, arguments: serde_json::Value) -> ProviderResponse {
    let call = ToolCall {
        id: format!("call_{name}"),
        name: name.to_string(),
        arguments,
    };
    ProviderResponse {
        message: Message::assistant_tool_calls(vec![call.clone()]),
        tool_calls: vec![call],
        usage: TokenUsage::new(10, 5),
        finish_reason: Some("tool_calls".into()),
    }
}

fn last_user_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::User)
        .map(|message| message.text().to_string())
        .unwrap_or_default()
}

fn node_label(objective: &str) -> String {
    objective
        .split_whitespace()
        .find(|word| word.starts_with("node-"))
        .unwrap_or("unknown")
        .to_string()
}

fn result_json(objective: &str, state: &str, evidence: &str, boundary: &str) -> String {
    json!({
        "objective": objective,
        "state": state,
        "evidence": evidence,
        "boundary": boundary,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

fn write_agents(dir: &Path) -> AgentRegistry {
    let agents = dir.join("agents");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::write(
        agents.join("default.agent.md"),
        "---\nname: default\ntools:\n  - read_file\n  - write_file\n  - list_dir\n---\n\nYou are a worker.\n",
    )
    .expect("default agent");
    std::fs::write(
        agents.join("reviewer.agent.md"),
        "---\nname: reviewer\ntools:\n  - read_file\n---\n\nYou review work.\n",
    )
    .expect("reviewer agent");
    // Names another provider than the one the test executor holds: the case the
    // executor must refuse up front rather than misroute.
    std::fs::write(
        agents.join("code-reviewer.agent.md"),
        "---\nname: code-reviewer\nmodel: deepseek/deepseek-chat\ntools:\n  - read_file\n---\n\nYou review code.\n",
    )
    .expect("code-reviewer agent");
    // Names the executor's own provider, so the node runs and its bare model
    // name is what reaches the provider.
    std::fs::write(
        agents.join("mock-worker.agent.md"),
        "---\nname: mock-worker\nmodel: mock/mock-1\ntools:\n  - read_file\n---\n\nYou work on the mock.\n",
    )
    .expect("mock-worker agent");
    AgentRegistry::from_dirs(&[agents]).expect("registry")
}

fn router() -> ModelRouter {
    let mut config = Config::default();
    config.provider.default = "mock".into();
    config.providers.insert(
        "mock".into(),
        ProviderConfig {
            kind: ProviderKind::Mock,
            default_model: Some("mock-1".into()),
            ..Default::default()
        },
    );
    config.providers.insert(
        "deepseek".into(),
        ProviderConfig {
            default_model: Some("deepseek-chat".into()),
            ..Default::default()
        },
    );
    ModelRouter::new(config)
}

async fn memory() -> Arc<dyn Memory> {
    Arc::new(SqliteMemory::in_memory().await.expect("in-memory memory"))
}

struct Harness {
    executor: Executor,
    events: mpsc::UnboundedSender<RoutedEvent>,
    events_rx: mpsc::UnboundedReceiver<RoutedEvent>,
    control: ControlHandle,
    channel: harness_agent::ControlChannel,
}

async fn harness(
    dir: &Path,
    provider: Arc<dyn Provider>,
    config: ExecutorConfig,
    verifier: Verifier,
) -> Harness {
    harness_with_memory(dir, provider, config, verifier, memory().await).await
}

/// [`harness`] over a caller-supplied store, so a test can create the parent
/// session before the executor is built and read the children afterwards.
async fn harness_with_memory(
    dir: &Path,
    provider: Arc<dyn Provider>,
    config: ExecutorConfig,
    verifier: Verifier,
    memory: Arc<dyn Memory>,
) -> Harness {
    let executor = Executor::new(
        config,
        write_agents(dir),
        router(),
        provider,
        Arc::clone(&memory),
        dir.to_path_buf(),
    )
    .with_verifier(verifier);
    let (events, events_rx) = mpsc::unbounded_channel();
    let (control, channel) = ControlHandle::channel();
    Harness {
        executor,
        events,
        events_rx,
        control,
        channel,
    }
}

impl Harness {
    async fn run(&mut self, graph: &TaskGraph) -> ExecutionReport {
        self.executor
            .execute(graph, &self.events, &mut self.channel)
            .await
            .expect("the run must not fail at the orchestration level")
    }

    fn emitted(&mut self) -> Vec<AgentEvent> {
        std::iter::from_fn(|| self.events_rx.try_recv().ok())
            .map(|routed| routed.event)
            .collect()
    }

    /// The same events, with the sub-agent lane each one carried.
    fn emitted_routed(&mut self) -> Vec<RoutedEvent> {
        std::iter::from_fn(|| self.events_rx.try_recv().ok()).collect()
    }
}

fn node(id: &str) -> TaskNode {
    TaskNode {
        id: id.into(),
        objective: format!("do {id}"),
        agent: None,
        depends_on: Vec::new(),
        files: Vec::new(),
        verify: Vec::new(),
    }
}

fn default_verifier() -> Verifier {
    Verifier::new(VerifierConfig::default())
}

// ---------------------------------------------------------------------------
// 1. A diamond schedules in layers, and its middle layer runs concurrently.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_diamond_runs_its_middle_layer_concurrently() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let barrier = Arc::new(Barrier::new(2));
    let windows: Arc<Mutex<Vec<(String, Instant, Instant)>>> = Arc::new(Mutex::new(Vec::new()));

    let provider = TestProvider::new("mock-1", {
        let barrier = Arc::clone(&barrier);
        let windows = Arc::clone(&windows);
        move |request| {
            let barrier = Arc::clone(&barrier);
            let windows = Arc::clone(&windows);
            Box::pin(async move {
                let objective = last_user_text(&request);
                let label = node_label(&objective);
                let started = Instant::now();

                // b and c hold each other at the barrier, so if they are not
                // genuinely concurrent this await never completes.
                if label == "node-b" || label == "node-c" {
                    barrier.wait().await;
                }

                windows
                    .lock()
                    .expect("lock")
                    .push((label.clone(), started, Instant::now()));

                Ok(text_reply(result_json(
                    &objective,
                    "done",
                    "the node ran",
                    "nothing else",
                )))
            })
        }
    });

    let mut graph = TaskGraph {
        nodes: vec![
            node("node-a"),
            node("node-b"),
            node("node-c"),
            node("node-d"),
        ],
        guidance: None,
    };
    graph.nodes[1].depends_on = vec!["node-a".into()];
    graph.nodes[2].depends_on = vec!["node-a".into()];
    graph.nodes[3].depends_on = vec!["node-b".into(), "node-c".into()];
    // A node run by a different agent than the entry agent must announce a handoff.
    graph.nodes[3].agent = Some("reviewer".into());

    let config = ExecutorConfig {
        max_parallel: 2,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, default_verifier()).await;

    let report = tokio::time::timeout(Duration::from_secs(20), harness.run(&graph))
        .await
        .expect("the run hung: b and c were not scheduled together");

    assert_eq!(report.succeeded, 4, "{:?}", report.nodes);
    assert_eq!(report.failed, 0);

    let windows = windows.lock().expect("lock").clone();
    let window = |label: &str| {
        windows
            .iter()
            .find(|(id, _, _)| id == label)
            .cloned()
            .unwrap_or_else(|| panic!("no window recorded for {label}: {windows:?}"))
    };

    let a = window("node-a");
    let b = window("node-b");
    let c = window("node-c");
    let d = window("node-d");

    assert!(
        b.1 < c.2 && c.1 < b.2,
        "b and c must overlap: b={b:?} c={c:?}"
    );
    assert!(
        a.2 <= b.1 && a.2 <= c.1,
        "a must finish before b and c start"
    );
    assert!(d.1 >= b.2 && d.1 >= c.2, "d must wait for b and c");

    let events = harness.emitted();
    let starts = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::SubtaskStart { .. }))
        .count();
    let ends = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::SubtaskEnd { .. }))
        .count();
    assert_eq!(starts, 4, "{events:?}");
    assert_eq!(ends, 4, "every started node needs a terminal event");

    // Every terminal frame names the graph node it closes, so a client can key a
    // checklist row on it. `subtask_id` is the worker's lane and is not the node.
    for id in ["node-a", "node-b", "node-c", "node-d"] {
        assert!(
            events.iter().any(|event| matches!(
                event,
                AgentEvent::SubtaskEnd { node_id, .. } if node_id == id
            )),
            "no terminal frame named `{id}`: {events:?}"
        );
    }

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::SubtaskEnd {
                status: SubtaskStatus::Succeeded,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::AgentHandoff { to, .. } if to.as_str() == "reviewer"
        )),
        "the reviewer node must announce a handoff: {events:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. A rejected result is retried exactly `max_retries` times, then accepted.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_result_is_retried_exactly_max_retries_times() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::constant(
        "mock-1",
        result_json("do node-a", "done", "ran the tests", "nothing else"),
    );

    // The adversarial reviewer rejects twice and then accepts: one rejection per
    // Reflexion attempt.
    let adversarial = Arc::new(harness_llm::MockProvider::new(
        "adv",
        "adv-1",
        vec![
            ScriptedTurn::Text(
                r#"{"valid": false, "issues": [{"severity": "error", "summary": "the new branch has no test", "evidence": "src/parse.rs has no #[test]"}]}"#
                    .into(),
            ),
            ScriptedTurn::Text(
                r#"{"valid": false, "issues": [{"severity": "error", "summary": "the new branch still has no test", "evidence": "src/parse.rs"}]}"#
                    .into(),
            ),
            ScriptedTurn::Text(r#"{"valid": true, "issues": []}"#.into()),
        ],
    ));
    let verifier = Verifier::new(VerifierConfig {
        adversarial_provider: Some(adversarial),
        adversarial_model: Some("adv-1".into()),
        ..VerifierConfig::default()
    });

    let config = ExecutorConfig {
        max_retries: 2,
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider.clone(), config, verifier).await;

    let graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    let outcome = report.get("node-a").expect("node-a outcome");
    assert_eq!(outcome.attempts, 3, "one attempt plus max_retries retries");
    assert_eq!(outcome.status, SubtaskStatus::Succeeded);
    assert!(outcome.verification.as_ref().expect("a report").valid);
    assert_eq!(report.succeeded, 1);

    let retry_saw_the_issues = provider.requests().iter().any(|request| {
        request
            .messages
            .iter()
            .any(|message| message.text().contains("the new branch has no test"))
    });
    assert!(
        retry_saw_the_issues,
        "the retry history must carry the verifier's issues"
    );
}

// ---------------------------------------------------------------------------
// 3. A failed node skips its dependents without counting them as failures.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dependent_of_a_failed_node_is_skipped() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::new("mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            if objective.contains("node-a") {
                // Not a four-field result: the node fails.
                Ok(text_reply("I could not finish this one.".to_string()))
            } else {
                Ok(text_reply(result_json(
                    &objective,
                    "done",
                    "ran",
                    "nothing else",
                )))
            }
        })
    });

    let config = ExecutorConfig {
        max_retries: 0,
        max_parallel: 2,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, default_verifier()).await;

    let mut graph = TaskGraph {
        nodes: vec![node("node-a"), node("node-b")],
        guidance: None,
    };
    graph.nodes[1].depends_on = vec!["node-a".into()];

    let report = harness.run(&graph).await;

    assert_eq!(
        report.get("node-a").expect("a").status,
        SubtaskStatus::Failed
    );
    assert_eq!(
        report.get("node-b").expect("b").status,
        SubtaskStatus::Skipped
    );
    assert_eq!(report.succeeded, 0);
    assert_eq!(report.failed, 1, "a skipped node is not a failed node");
    assert_eq!(report.skipped(), 1);

    // A node that never started was never announced, but it still needs a
    // terminal frame: a client that drew a row for it from the plan must be able
    // to reach `skipped` rather than leaving the row pending forever.
    let events = harness.emitted();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::SubtaskEnd { node_id, status, .. }
                if node_id == "node-b" && *status == SubtaskStatus::Skipped
        )),
        "a skipped node must report itself: {events:?}"
    );
    // Exactly one terminal frame per node, so a client cannot see it finish twice.
    let ends_for_b = events
        .iter()
        .filter(
            |event| matches!(event, AgentEvent::SubtaskEnd { node_id, .. } if node_id == "node-b"),
        )
        .count();
    assert_eq!(ends_for_b, 1, "{events:?}");
}

// ---------------------------------------------------------------------------
// 4. Worktree isolation: two concurrent nodes cannot see each other's file.
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be on PATH");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn hermetic_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();
    git(dir, &["init"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Harness Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("seed.txt"), "seed").expect("seed file");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "seed"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_nodes_cannot_see_each_others_files() {
    let repo = hermetic_repo();

    // Both nodes write `shared.txt` before either reads it back, so a shared
    // working root would make at least one of them read the other's content.
    let barrier = Arc::new(Barrier::new(2));
    let provider = TestProvider::new("mock-1", {
        let barrier = Arc::clone(&barrier);
        move |request| {
            let barrier = Arc::clone(&barrier);
            Box::pin(async move {
                let objective = last_user_text(&request);
                let label = node_label(&objective);
                let tool_results: Vec<String> = request
                    .messages
                    .iter()
                    .filter(|message| message.role == Role::Tool)
                    .map(|message| message.text().to_string())
                    .collect();

                match tool_results.len() {
                    0 => Ok(tool_reply(
                        "write_file",
                        json!({ "path": "shared.txt", "content": label }),
                    )),
                    1 => {
                        barrier.wait().await;
                        Ok(tool_reply("read_file", json!({ "path": "shared.txt" })))
                    }
                    _ => {
                        let evidence = tool_results.last().cloned().unwrap_or_default();
                        Ok(text_reply(result_json(
                            &objective, "done", &evidence, "none",
                        )))
                    }
                }
            })
        }
    });

    let config = ExecutorConfig {
        use_worktrees: true,
        max_parallel: 2,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(repo.path(), provider, config, default_verifier()).await;

    let graph = TaskGraph {
        nodes: vec![node("node-b"), node("node-c")],
        guidance: None,
    };
    let report = tokio::time::timeout(Duration::from_secs(30), harness.run(&graph))
        .await
        .expect("the run hung: the two nodes were not concurrent");

    assert_eq!(report.succeeded, 2, "{:?}", report.nodes);

    for id in ["node-b", "node-c"] {
        let other = if id == "node-b" { "node-c" } else { "node-b" };
        let evidence = report
            .get(id)
            .expect("outcome")
            .result
            .as_ref()
            .expect("a result")
            .evidence
            .clone();
        assert!(
            evidence.contains(id),
            "{id} must read back its own file, got: {evidence}"
        );
        assert!(
            !evidence.contains(other),
            "{id} must not see {other}'s file, got: {evidence}"
        );
    }

    // Both nodes succeeded, so both of their worktrees were kept: this test
    // removes what it made rather than leaving checkouts in the shared base.
    for id in ["node-b", "node-c"] {
        let kept = report
            .get(id)
            .and_then(|outcome| outcome.worktree.as_ref())
            .expect("a successful node under isolation keeps its worktree");
        remove_kept_worktree(repo.path(), kept);
    }
}

// ---------------------------------------------------------------------------
// 5. An abort stops scheduling and returns promptly.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abort_stops_scheduling_further_nodes() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::new("mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            if objective.contains("node-a") {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Ok(text_reply(result_json(
                &objective,
                "done",
                "ran",
                "nothing else",
            )))
        })
    });

    let config = ExecutorConfig {
        max_parallel: 2,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, default_verifier()).await;

    let mut graph = TaskGraph {
        nodes: vec![node("node-a"), node("node-b"), node("node-c")],
        guidance: None,
    };
    graph.nodes[1].depends_on = vec!["node-a".into()];
    graph.nodes[2].depends_on = vec!["node-b".into()];

    let control = harness.control.clone();
    let trigger = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        control.abort(Some("the user pressed stop".into()));
    });

    let started = Instant::now();
    let report = tokio::time::timeout(Duration::from_secs(15), harness.run(&graph))
        .await
        .expect("the abort did not stop the run");
    let _ = trigger.await;

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the run took {:?} after an abort",
        started.elapsed()
    );
    assert_eq!(
        report.get("node-a").expect("a").status,
        SubtaskStatus::Failed
    );
    assert_eq!(
        report.get("node-b").expect("b").status,
        SubtaskStatus::Skipped
    );
    assert_eq!(
        report.get("node-c").expect("c").status,
        SubtaskStatus::Skipped
    );
    assert_eq!(report.succeeded, 0);
    assert_eq!(report.skipped(), 2);
}

// ---------------------------------------------------------------------------
// 6. A sub-agent does not inherit the planner's conversation.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agent_never_sees_the_planners_conversation() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let planner_provider = TestProvider::constant(
        "mock-1",
        r#"{"nodes":[{"id":"node-a","objective":"do node-a"}]}"#.to_string(),
    );
    let graph = Planner::new(planner_provider.clone(), "mock-1", 8)
        .plan("build the thing", Some("the repo already has a lexer"))
        .await
        .expect("a plan");

    let worker = TestProvider::constant(
        "mock-1",
        result_json("do node-a", "done", "ran", "nothing else"),
    );

    let config = ExecutorConfig {
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), worker.clone(), config, default_verifier()).await;
    harness.run(&graph).await;

    let requests = worker.requests();
    assert_eq!(requests.len(), 1);
    let first = &requests[0];

    // The loop prepends the agent's own system prompt; the history itself is
    // exactly one user message.
    let user_messages: Vec<&Message> = first
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .collect();
    assert_eq!(user_messages.len(), 1, "{:?}", first.messages);
    assert_eq!(first.messages.len(), 2, "{:?}", first.messages);
    assert!(first.messages[0].text().contains("You are a worker."));

    let prompt = user_messages[0].text();
    assert!(prompt.contains("do node-a"), "{prompt}");
    assert!(prompt.contains("\"boundary\""), "{prompt}");

    // Nothing from the planning turn may leak into the worker's history.
    for message in &first.messages {
        assert!(
            !message.text().contains("planning agent"),
            "the planner's system prompt leaked: {}",
            message.text()
        );
        assert!(
            !message.text().contains("the repo already has a lexer"),
            "the planner's context leaked: {}",
            message.text()
        );
    }
}

// ---------------------------------------------------------------------------
// 7. A sub-agent's own contract is enforced: a bad reply is retried, then fails.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_whose_reply_never_parses_fails_after_its_retries() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::constant("mock-1", "I did the work, trust me.".to_string());

    let config = ExecutorConfig {
        max_retries: 1,
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider.clone(), config, default_verifier()).await;

    let graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    let outcome = report.get("node-a").expect("outcome");
    assert_eq!(outcome.status, SubtaskStatus::Failed);
    assert_eq!(outcome.attempts, 2);
    assert!(outcome.result.is_none());

    assert_eq!(provider.requests().len(), 2);

    let events = harness.emitted();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::SubtaskEnd {
                status: SubtaskStatus::Failed,
                ..
            }
        )),
        "a failing node must still emit a terminal event: {events:?}"
    );
}

// ---------------------------------------------------------------------------
// 8. A node naming an unknown agent fails without a panic.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_agent_fails_the_node_with_a_terminal_event() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::constant(
        "mock-1",
        result_json("do node-a", "done", "ran", "nothing else"),
    );
    let config = ExecutorConfig {
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, default_verifier()).await;

    let mut graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    graph.nodes[0].agent = Some("ghost".into());

    let report = harness.run(&graph).await;
    assert_eq!(
        report.get("node-a").expect("a").status,
        SubtaskStatus::Failed
    );

    let events = harness.emitted();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::SubtaskEnd {
                status: SubtaskStatus::Failed,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(
            |event| matches!(event, AgentEvent::Error { message } if message.contains("ghost"))
        ),
        "{events:?}"
    );
}

// ---------------------------------------------------------------------------
// 9. The verifier's report reaches the outcome, checks and all.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_configured_check_runs_and_is_recorded() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::constant(
        "mock-1",
        result_json("do node-a", "done", "ran", "nothing else"),
    );
    let verifier = Verifier::new(VerifierConfig {
        checks: vec![vec!["git".into(), "--version".into()]],
        ..VerifierConfig::default()
    });

    let config = ExecutorConfig {
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, verifier).await;

    let graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    let outcome = report.get("node-a").expect("outcome");
    let verification = outcome.verification.as_ref().expect("a report");
    assert!(verification.valid);
    assert_eq!(verification.checks.len(), 1);
    assert!(verification.checks[0].passed);
    assert_eq!(outcome.status, SubtaskStatus::Succeeded);
}

// ---------------------------------------------------------------------------
// 10. A provider error still leaves a terminal event and a failed node.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_error_fails_the_node_but_not_the_run() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::new("mock-1", |_request| {
        Box::pin(async move {
            Err(HarnessError::Provider {
                provider: "test".into(),
                message: "upstream exploded".into(),
            })
        })
    });

    let config = ExecutorConfig {
        max_retries: 0,
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, default_verifier()).await;

    let graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    assert_eq!(
        report.get("node-a").expect("a").status,
        SubtaskStatus::Failed
    );
    assert_eq!(report.failed, 1);

    let events = harness.emitted();
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::Error { .. })));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SubtaskEnd {
            status: SubtaskStatus::Failed,
            ..
        }
    )));
}

// ---------------------------------------------------------------------------
// 11. A cyclic graph is refused before anything runs.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cyclic_graph_is_refused_before_any_node_runs() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider =
        TestProvider::constant("mock-1", result_json("do a", "done", "ran", "nothing else"));
    let mut harness = harness(
        tmp.path(),
        provider.clone(),
        ExecutorConfig::default(),
        default_verifier(),
    )
    .await;

    let mut graph = TaskGraph {
        nodes: vec![node("node-a"), node("node-b")],
        guidance: None,
    };
    graph.nodes[0].depends_on = vec!["node-b".into()];
    graph.nodes[1].depends_on = vec!["node-a".into()];

    let err = harness
        .executor
        .execute(&graph, &harness.events, &mut harness.channel)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cycle"), "{err}");
    assert!(provider.requests().is_empty(), "nothing may run");
}

// ---------------------------------------------------------------------------
// 12. The four-field contract survives a round trip through a real run.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reported_result_is_the_four_field_contract() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let reply = format!(
        "Here is the result:\n\n```json\n{}\n```\n",
        result_json("do node-a", "done", "cargo test passed", "docs not written")
    );
    let provider = TestProvider::constant("mock-1", reply);
    let mut harness = harness(
        tmp.path(),
        provider,
        ExecutorConfig::default(),
        default_verifier(),
    )
    .await;

    let graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    let result: SubtaskResult = report
        .get("node-a")
        .expect("outcome")
        .result
        .clone()
        .expect("a parsed result");
    assert_eq!(result.objective, "do node-a");
    assert_eq!(result.state, "done");
    assert_eq!(result.evidence, "cargo test passed");
    assert_eq!(result.boundary, "docs not written");
}

// ---------------------------------------------------------------------------
// 13. A successful node keeps the worktree that holds the only copy of its
//     edits; a failed one discards it, and `keep_worktrees: false` restores the
//     old discard-everything behaviour.
// ---------------------------------------------------------------------------

/// The checkouts currently on disk for one node id. The worktree base is shared
/// by the whole test binary, so the search is scoped to the id's slug prefix.
fn worktree_dirs(node_id: &str) -> Vec<PathBuf> {
    let prefix = format!("{}-", node_id.to_ascii_lowercase());
    let base = std::env::temp_dir().join("harness-worktrees");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(base)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.starts_with(&prefix))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// A kept worktree outlives the test binary's temp dir, so a test that keeps one
/// removes it itself.
fn remove_kept_worktree(repo: &Path, worktree: &WorktreeRef) {
    git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            worktree.path.to_string_lossy().as_ref(),
        ],
    );
    git(repo, &["branch", "-D", &worktree.branch]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_successful_node_keeps_the_worktree_holding_its_edits() {
    let repo = hermetic_repo();

    // The first turn writes the file, the second returns the four-field result,
    // so the edit is on disk before the node is judged to have succeeded.
    let provider = TestProvider::new("mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            let tool_results: Vec<String> = request
                .messages
                .iter()
                .filter(|message| message.role == Role::Tool)
                .map(|message| message.text().to_string())
                .collect();

            if tool_results.is_empty() {
                Ok(tool_reply(
                    "write_file",
                    json!({ "path": "kept.txt", "content": "the node's edit" }),
                ))
            } else {
                Ok(text_reply(result_json(
                    &objective,
                    "done",
                    "wrote kept.txt",
                    "nothing else",
                )))
            }
        })
    });

    let config = ExecutorConfig {
        use_worktrees: true,
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(repo.path(), provider, config, default_verifier()).await;

    let graph = TaskGraph {
        nodes: vec![node("kept-node")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    let outcome = report.get("kept-node").expect("outcome");
    assert_eq!(outcome.status, SubtaskStatus::Succeeded);

    let kept = outcome
        .worktree
        .as_ref()
        .expect("a successful node under isolation must report where its edits are");
    assert!(
        kept.path.is_dir(),
        "the kept worktree must still exist: {:?}",
        kept.path
    );
    assert!(
        kept.branch.starts_with("harness/"),
        "unexpected branch: {}",
        kept.branch
    );
    assert_eq!(
        std::fs::read_to_string(kept.path.join("kept.txt")).expect("the node's file"),
        "the node's edit",
        "the kept worktree must hold the node's content"
    );
    assert!(
        !repo.path().join("kept.txt").exists(),
        "the edit must stay in the worktree, not appear in the live tree"
    );

    // The location has to reach the event stream, not just the report.
    let events = harness.emitted();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::SubtaskEnd { summary, .. }
                if summary.contains(&kept.path.display().to_string())
                    && summary.contains(&kept.branch)
        )),
        "the kept worktree must be named in the SubtaskEnd summary: {events:?}"
    );

    remove_kept_worktree(repo.path(), kept);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_node_discards_its_worktree() {
    let repo = hermetic_repo();

    // Not a four-field result and no retries: the node fails with a worktree
    // already created for it, and there is nothing there worth integrating.
    let provider = TestProvider::constant("mock-1", "I could not finish this one.".to_string());
    let config = ExecutorConfig {
        use_worktrees: true,
        max_retries: 0,
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(repo.path(), provider, config, default_verifier()).await;

    let graph = TaskGraph {
        nodes: vec![node("discard-node")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    let outcome = report.get("discard-node").expect("outcome");
    assert_eq!(outcome.status, SubtaskStatus::Failed);
    assert!(
        outcome.worktree.is_none(),
        "a failed node has nothing to keep: {:?}",
        outcome.worktree
    );
    let left = worktree_dirs("discard-node");
    assert!(
        left.is_empty(),
        "a failed node must leave no checkout behind: {left:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keep_worktrees_false_restores_the_discard_behaviour() {
    let repo = hermetic_repo();

    let provider = TestProvider::constant(
        "mock-1",
        result_json("do discard-edit", "done", "ran", "nothing else"),
    );
    let config = ExecutorConfig {
        use_worktrees: true,
        keep_worktrees: false,
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(repo.path(), provider, config, default_verifier()).await;

    let graph = TaskGraph {
        nodes: vec![node("discard-edit")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    let outcome = report.get("discard-edit").expect("outcome");
    assert_eq!(outcome.status, SubtaskStatus::Succeeded);
    assert!(
        outcome.worktree.is_none(),
        "with keep_worktrees off a success must report no worktree: {:?}",
        outcome.worktree
    );
    let left = worktree_dirs("discard-edit");
    assert!(
        left.is_empty(),
        "keep_worktrees: false must remove the checkout: {left:?}"
    );
}

// ---------------------------------------------------------------------------
// 14. Model resolution: the executor serves one provider, and a node that names
//     another is refused before it spends a token.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_whose_agent_names_another_provider_fails_before_any_request() {
    let tmp = tempfile::tempdir().expect("temp dir");

    // The reported case: a graph whose `code-reviewer` node names
    // `deepseek/deepseek-chat`, run under a different gateway. The executor must
    // refuse it up front rather than send the bare `deepseek-chat` to a gateway
    // that has never heard of it.
    let provider = TestProvider::named("opencode-go", "opencode-go-1", |_| {
        Box::pin(async move { Ok(text_reply("this must never be called".into())) })
    });

    let config = ExecutorConfig {
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider.clone(), config, default_verifier()).await;

    let mut graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    graph.nodes[0].agent = Some("code-reviewer".into());

    let report = harness.run(&graph).await;

    let outcome = report.get("node-a").expect("outcome");
    assert_eq!(outcome.status, SubtaskStatus::Failed);
    assert_eq!(
        outcome.tokens, 0,
        "the node must fail before spending a token"
    );
    assert!(
        provider.requests().is_empty(),
        "no request may reach the provider: {:?}",
        provider.requests()
    );

    let events = harness.emitted();
    // The node was announced, so it must still close its lane.
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::SubtaskStart { .. })),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::SubtaskEnd {
                status: SubtaskStatus::Failed,
                ..
            }
        )),
        "{events:?}"
    );

    let message = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Error { message } => Some(message.clone()),
            _ => None,
        })
        .expect("the refusal must say why");
    assert!(message.contains("node-a"), "{message}");
    assert!(message.contains("deepseek/deepseek-chat"), "{message}");
    assert!(message.contains("deepseek"), "{message}");
    assert!(message.contains("opencode-go"), "{message}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_whose_agent_names_the_executors_own_provider_runs() {
    let tmp = tempfile::tempdir().expect("temp dir");

    // `mock-worker` names `mock/mock-1`, and the executor holds a `mock`
    // provider, so the node runs and only the bare model name is sent.
    let provider = TestProvider::new("mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            Ok(text_reply(result_json(
                &objective,
                "done",
                "ran",
                "nothing else",
            )))
        })
    });

    let config = ExecutorConfig {
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider.clone(), config, default_verifier()).await;

    let mut graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    graph.nodes[0].agent = Some("mock-worker".into());

    let report = harness.run(&graph).await;
    assert_eq!(
        report.get("node-a").expect("outcome").status,
        SubtaskStatus::Succeeded
    );

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].model, "mock-1",
        "the provider must receive the bare model name"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_naming_no_model_inherits_the_executors_provider() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::new("mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            Ok(text_reply(result_json(
                &objective,
                "done",
                "ran",
                "nothing else",
            )))
        })
    });

    let config = ExecutorConfig {
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider.clone(), config, default_verifier()).await;

    // The default agent names no model, so the router's default provider — the
    // one the executor holds — must serve it.
    let graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    assert_eq!(
        report.get("node-a").expect("outcome").status,
        SubtaskStatus::Succeeded
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "mock-1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explicit_provider_id_overrides_the_provider_instances_own_id() {
    let tmp = tempfile::tempdir().expect("temp dir");

    // The instance calls itself `wrapper`, but the caller declares it as `mock`,
    // the provider a node with no model resolves to. The declaration wins.
    let provider = TestProvider::named("wrapper", "mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            Ok(text_reply(result_json(
                &objective,
                "done",
                "ran",
                "nothing else",
            )))
        })
    });

    let config = ExecutorConfig {
        provider_id: Some("mock".into()),
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider.clone(), config, default_verifier()).await;

    let graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    let report = harness.run(&graph).await;

    assert_eq!(
        report.get("node-a").expect("outcome").status,
        SubtaskStatus::Succeeded
    );
    assert_eq!(provider.requests().len(), 1);
}

// ---------------------------------------------------------------------------
// 15. A node run by another agent announces a handoff from the entry agent.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handoff_names_the_entry_agent_and_the_nodes_agent() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::new("mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            Ok(text_reply(result_json(
                &objective,
                "done",
                "ran",
                "nothing else",
            )))
        })
    });

    let config = ExecutorConfig {
        max_parallel: 2,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, default_verifier()).await;

    let mut graph = TaskGraph {
        nodes: vec![node("node-a"), node("node-b")],
        guidance: None,
    };
    graph.nodes[0].agent = Some("reviewer".into());

    let report = harness.run(&graph).await;
    assert_eq!(report.succeeded, 2, "{:?}", report.nodes);

    let events = harness.emitted();
    let handoffs: Vec<(String, String, String)> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AgentHandoff { from, to, reason } => Some((
                from.as_str().to_string(),
                to.as_str().to_string(),
                reason.clone(),
            )),
            _ => None,
        })
        .collect();

    // Only `node-a` runs under a different agent than the entry agent, so it is
    // the only node that may hand off.
    assert_eq!(handoffs.len(), 1, "{events:?}");
    assert_eq!(handoffs[0].0, "default");
    assert_eq!(handoffs[0].1, "reviewer");
    assert!(handoffs[0].2.contains("node-a"), "{:?}", handoffs[0]);
}

// ---------------------------------------------------------------------------
// 16. Every node's run is a session of its own, forked from the parent.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_node_persists_its_own_session_forked_from_the_parent() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let memory = memory().await;
    // The main conversation, empty: a plan writes no turns to it, so the fork
    // point is its empty root and the children inherit nothing.
    let parent = memory
        .create_session(Some("plan:test"))
        .await
        .expect("a parent session");

    let planner = TestProvider::constant(
        "mock-1",
        json!({ "nodes": [
            { "id": "node-a", "objective": "do node-a" },
            { "id": "node-b", "objective": "do node-b" },
        ]})
        .to_string(),
    );
    let graph = Planner::new(planner, "mock-1", 8)
        .plan("build the thing", None)
        .await
        .expect("a plan");

    let worker = TestProvider::new("mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            Ok(text_reply(result_json(
                &objective,
                "done",
                "ran",
                "nothing else",
            )))
        })
    });

    let config = ExecutorConfig {
        parent_session: Some(parent),
        max_parallel: 2,
        ..ExecutorConfig::default()
    };
    let mut harness = harness_with_memory(
        tmp.path(),
        worker.clone(),
        config,
        default_verifier(),
        Arc::clone(&memory),
    )
    .await;
    let report = harness.run(&graph).await;
    assert_eq!(report.succeeded, 2, "{:?}", report.nodes);

    let sessions = memory.sessions().await.expect("the session list");
    // One session per node, plus the parent they fork from.
    assert_eq!(sessions.len(), 3, "{sessions:#?}");
    let children: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|session| session.forked_from_session == Some(parent))
        .collect();
    assert_eq!(children.len(), 2, "{sessions:#?}");

    for (label, objective) in [("node-a", "do node-a"), ("node-b", "do node-b")] {
        let child = children
            .iter()
            .find(|session| session.label.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("no child session labelled `{label}`: {sessions:#?}"));
        assert_ne!(child.id, parent);
        assert!(
            child.forked_from.is_some(),
            "a fork records where in the parent it was taken"
        );

        let history = memory
            .history(child.id, None)
            .await
            .expect("the child's history");
        assert!(!history.is_empty(), "{label} stored no turns");
        assert!(
            history
                .iter()
                .any(|message| message.text().contains(objective)),
            "{label} must hold its own prompt: {history:?}"
        );
        assert!(
            history.iter().any(|message| message.text().contains("ran")),
            "{label} must hold its own reply: {history:?}"
        );

        // Its own conversation alone: the sibling's work, and the planner's
        // graph, must not appear in it.
        let sibling = if label == "node-a" {
            "do node-b"
        } else {
            "do node-a"
        };
        for message in &history {
            assert!(
                !message.text().contains(sibling),
                "{label} inherited its sibling's work: {}",
                message.text()
            );
            assert!(
                !message.text().contains("\"nodes\""),
                "{label} inherited the planner's graph: {}",
                message.text()
            );
        }
    }

    // The parent stays the main conversation: no worker turn reached it.
    let parent_history = memory
        .history(parent, None)
        .await
        .expect("the parent's history");
    assert!(
        parent_history.is_empty(),
        "the parent must hold no worker turns: {parent_history:?}"
    );

    // Each node's events carry its lane, and the lane names the session.
    let routed = harness.emitted_routed();
    let starts: Vec<&RoutedEvent> = routed
        .iter()
        .filter(|event| matches!(event.event, AgentEvent::SubtaskStart { .. }))
        .collect();
    assert_eq!(starts.len(), 2, "{routed:?}");
    for start in starts {
        assert_eq!(
            start.subagent_id.as_ref().map(AgentId::as_str),
            Some("default"),
            "{routed:?}"
        );
        let session = start
            .subagent_session_id
            .expect("a node's lane names the session it was persisted to");
        let info = memory
            .session(session)
            .await
            .expect("the session is readable")
            .expect("the session exists");
        assert_eq!(info.forked_from_session, Some(parent));
    }
    // The worker's own loop output rides the same lane as its bracketing events.
    assert!(
        routed
            .iter()
            .any(|event| event.subagent_id.is_some()
                && matches!(event.event, AgentEvent::Done { .. })),
        "the loop's events must be tagged with the worker's lane: {routed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_parent_session_a_node_runs_unpersisted() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let memory = memory().await;

    let worker = TestProvider::constant(
        "mock-1",
        result_json("do node-a", "done", "ran", "nothing else"),
    );
    let mut harness = harness_with_memory(
        tmp.path(),
        worker,
        ExecutorConfig {
            max_parallel: 1,
            ..ExecutorConfig::default()
        },
        default_verifier(),
        Arc::clone(&memory),
    )
    .await;

    let graph = TaskGraph {
        nodes: vec![node("node-a")],
        guidance: None,
    };
    let report = harness.run(&graph).await;
    assert_eq!(report.succeeded, 1, "{:?}", report.nodes);

    // Nothing was written, and the lane names no session to open.
    assert!(
        memory.sessions().await.expect("the list").is_empty(),
        "an unparented run must not create sessions"
    );
    let routed = harness.emitted_routed();
    assert!(
        routed
            .iter()
            .filter(|event| event.subagent_id.is_some())
            .all(|event| event.subagent_session_id.is_none()),
        "{routed:?}"
    );
}

// ---------------------------------------------------------------------------
// 17. A workflow's guidance reaches its workers and its declared checks run.
// ---------------------------------------------------------------------------

/// The check command the tests below declare on their nodes. `git --version`
/// succeeds anywhere git is installed, which the worktree tests already assume.
fn passing_check() -> Vec<Vec<String>> {
    vec![vec!["git".to_string(), "--version".to_string()]]
}

fn failing_check() -> Vec<Vec<String>> {
    vec![vec![
        "git".to_string(),
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        "HEAD".to_string(),
    ]]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workflows_guidance_and_declared_check_reach_the_worker() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::constant(
        "mock-1",
        result_json("do node-a", "done", "ran", "nothing else"),
    );
    // The executor's own verifier declares no check: the node's own command is
    // the only one that can run, which is what proves it was carried through.
    let config = ExecutorConfig {
        max_retries: 0,
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider.clone(), config, default_verifier()).await;

    let mut with_check = node("node-a");
    with_check.verify = passing_check();
    let graph = TaskGraph {
        nodes: vec![with_check],
        guidance: Some("Verify before you edit, and say what you checked.".into()),
    };

    let report = harness.run(&graph).await;

    // The guidance reached the sub-agent's prompt.
    let prompt = last_user_text(&provider.requests()[0]);
    assert!(
        prompt.contains("Verify before you edit, and say what you checked."),
        "the workflow's guidance must reach the worker's prompt: {prompt}"
    );
    assert!(
        prompt.contains("How this run must be carried out:"),
        "{prompt}"
    );

    // And the node's declared command actually ran.
    let outcome = report.get("node-a").expect("outcome");
    let verification = outcome.verification.as_ref().expect("a report");
    assert_eq!(verification.checks.len(), 1, "{:?}", verification.checks);
    assert_eq!(verification.checks[0].name, "git --version");
    assert!(
        verification.checks[0].passed,
        "{:?}",
        verification.checks[0]
    );
    assert!(verification.valid);
    assert_eq!(outcome.status, SubtaskStatus::Succeeded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_declared_check_keeps_the_node_from_succeeding() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::constant(
        "mock-1",
        result_json(
            "do node-a",
            "done",
            "I ran the tests, honestly",
            "nothing else",
        ),
    );
    let config = ExecutorConfig {
        max_retries: 0,
        max_parallel: 1,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, default_verifier()).await;

    let mut with_check = node("node-a");
    with_check.verify = failing_check();
    let graph = TaskGraph {
        nodes: vec![with_check],
        guidance: None,
    };

    let report = harness.run(&graph).await;

    let outcome = report.get("node-a").expect("outcome");
    assert_eq!(
        outcome.status,
        SubtaskStatus::Failed,
        "a node whose own check failed must not be reported as succeeded"
    );
    let verification = outcome.verification.as_ref().expect("a report");
    assert!(!verification.valid);
    assert!(!verification.checks[0].passed);
    assert_eq!(report.succeeded, 0);
    assert_eq!(report.failed, 1);
}

// ---------------------------------------------------------------------------
// 18. A node that reports itself blocked is not a success, and its dependents
//     do not run.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_node_fails_and_its_dependent_is_skipped() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let provider = TestProvider::new("mock-1", |request| {
        Box::pin(async move {
            let objective = last_user_text(&request);
            let state = if objective.contains("node-a") {
                "blocked — the API key is missing"
            } else {
                "done"
            };
            Ok(text_reply(result_json(
                &objective,
                state,
                "ran",
                "nothing else",
            )))
        })
    });
    let config = ExecutorConfig {
        max_retries: 2,
        max_parallel: 2,
        ..ExecutorConfig::default()
    };
    let mut harness = harness(tmp.path(), provider, config, default_verifier()).await;

    let mut graph = TaskGraph {
        nodes: vec![node("node-a"), node("node-b")],
        guidance: None,
    };
    graph.nodes[1].depends_on = vec!["node-a".into()];

    let report = harness.run(&graph).await;

    let blocked = report.get("node-a").expect("a");
    assert_eq!(
        blocked.status,
        SubtaskStatus::Failed,
        "a blocked node must not be reported as succeeded"
    );
    // The report says why, so a user does not have to guess what "failed" meant.
    assert!(
        blocked
            .result
            .as_ref()
            .is_some_and(|result| result.state.contains("blocked")),
        "{:?}",
        blocked.result
    );
    assert_eq!(
        report.get("node-b").expect("b").status,
        SubtaskStatus::Skipped,
        "a dependent of a blocked node must not run"
    );
    assert_eq!(report.succeeded, 0);
    assert_eq!(report.failed, 1);
}
