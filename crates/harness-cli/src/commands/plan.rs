//! `harness plan`: decompose one task into a DAG, then execute it.
//!
//! A sibling of `run`, assembled from the same pieces: the same agent
//! resolution, provider factory, memory and event printer. The difference is
//! that a model call comes first and the orchestrator does the work. The plan is
//! printed and validated before any sub-agent starts, so a malformed plan costs
//! one planning call and nothing else.

use std::borrow::Cow;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use harness_agent::{AgentRegistry, AgentSpec, ControlHandle, ModelRouter, ResolvedModel};
use harness_core::{
    AgentEvent, Config, HarnessError, Memory, RoutedEvent, SubtaskStatus, ToolCallStatus,
};
use harness_llm::Provider;
use harness_memory::SqliteMemory;
use harness_orchestrator::{
    ExecutionReport, Executor, ExecutorConfig, NodeOutcome, Planner, Severity, TaskGraph, TaskNode,
    VerificationReport,
};
use tokio::sync::mpsc;

use crate::commands::{build_verifier, resolve_verify_checks, PlanCommand};
use crate::context::AppContext;

/// Used only when a workspace has no `.agent.md` files at all.
const FALLBACK_SYSTEM_PROMPT: &str = "\
You are a coding agent working inside a project workspace.
Use the available tools to inspect and modify files. Prefer reading before writing.
Keep answers short and concrete.";

/// A ceiling, not a target. A plan that explodes into dozens of nodes spends
/// more tokens coordinating than working, and the planner is told to prefer a
/// few substantial nodes.
const MAX_PLAN_NODES: usize = 12;

pub(crate) async fn execute(command: PlanCommand, ctx: &AppContext) -> anyhow::Result<()> {
    let registry = AgentRegistry::load(&ctx.workspace_root).map_err(to_anyhow)?;
    let spec = select_spec(
        &registry,
        command.agent.as_deref(),
        &ctx.config.harness.default_agent,
    )?;

    let router = ModelRouter::new(ctx.config.clone());
    let overrides = model_overrides(ctx, &command)?;
    let resolved = router
        .resolve(Some(spec.as_ref()), None, &overrides)
        .map_err(to_anyhow)?;
    let provider = build_provider(ctx, &resolved.provider_id, &resolved.model)?;

    // The planner is a single tool-free turn, so it can be moved to a cheaper
    // model than the workers without touching what they run on.
    let (planner_provider, planner_provider_id, planner_model) =
        match command.planner_model.as_deref() {
            Some(hint) => {
                let chosen = router
                    .resolve(Some(spec.as_ref()), None, &[hint.to_string()])
                    .map_err(to_anyhow)?;
                let provider = build_provider(ctx, &chosen.provider_id, &chosen.model)?;
                (provider, chosen.provider_id, chosen.model)
            }
            None => (
                Arc::clone(&provider),
                resolved.provider_id.clone(),
                resolved.model.clone(),
            ),
        };
    eprintln!("-- planning with {planner_provider_id}/{planner_model}");

    let planner = Planner::new(planner_provider, planner_model, MAX_PLAN_NODES);
    let mut graph = planner.plan(&command.task, None).await.map_err(to_anyhow)?;

    // `--verify` is this run's contract for every node that does not declare its
    // own: a planner-produced node carries no checks, and writing them onto the
    // node is what makes the gate visible in the plan rather than only at the
    // executor's default. A workflow stage that declares its own still wins.
    let verify_checks = resolve_verify_checks(&command.verify, &ctx.config.verify.checks);
    if !verify_checks.is_empty() {
        for node in graph.nodes.iter_mut() {
            if node.verify.is_empty() {
                node.verify = verify_checks.clone();
            }
        }
    }

    // Layering validates first, so a graph that fails is refused here rather
    // than discovered node by node with tokens already spent.
    let layers = graph.layers().map_err(to_anyhow)?;
    print_plan(&graph, &layers);

    let (use_worktrees, reason) = worktree_decision(&ctx.workspace_root);
    eprintln!(
        "-- worktree isolation {}: {reason}",
        if use_worktrees { "on" } else { "off" }
    );

    let memory: Arc<dyn Memory> =
        Arc::new(SqliteMemory::open(ctx.db_path()).await.map_err(to_anyhow)?);

    // The session the run belongs to. Each node's sub-agent forks its own
    // session from this one, so the delegation stays traceable after the
    // command exits — `harness memory tree` shows the children underneath it.
    let session_id = memory
        .create_session(Some(&format!("plan:{}", spec.id)))
        .await
        .map_err(to_anyhow)?;
    eprintln!("-- session {session_id} (see `harness memory tree`)");

    // The executor's verifier covers the nodes that declare no check of their
    // own. `--verify` has already been written onto those nodes above, so this
    // is where the `[verify]` section and the adversarial reviewer are wired in.
    let verifier = build_verifier(
        &ctx.config,
        Arc::clone(&provider),
        ctx.config.verify.checks.clone(),
    );
    let executor = Executor::new(
        ExecutorConfig {
            max_retries: command.max_retries,
            use_worktrees,
            parent_session: Some(session_id),
            ..ExecutorConfig::default()
        },
        registry.clone(),
        ModelRouter::new(executor_config(ctx, &resolved)),
        provider,
        memory,
        ctx.workspace_root.clone(),
    )
    .with_verifier(verifier);

    let (events, mut receiver) = mpsc::unbounded_channel::<RoutedEvent>();
    let printer = tokio::spawn(async move { print_events(&mut receiver).await });

    let (control_handle, mut control) = ControlHandle::channel();
    install_ctrl_c_handler(control_handle);

    let report = executor
        .execute(&graph, &events, &mut control)
        .await
        .map_err(to_anyhow)?;

    drop(events);
    let _ = printer.await;

    print_report(&report);
    print_worktrees(&report);

    if report.failed > 0 {
        anyhow::bail!("{} of {} node(s) failed", report.failed, report.nodes.len());
    }
    Ok(())
}

fn to_anyhow(err: HarnessError) -> anyhow::Error {
    anyhow::anyhow!("{err}")
}

/// The agent a plan is considered to run under: the one asked for, else the
/// configured default, else a built-in fallback.
fn select_spec<'a>(
    registry: &'a AgentRegistry,
    requested: Option<&str>,
    configured_default: &str,
) -> anyhow::Result<Cow<'a, AgentSpec>> {
    if let Some(id) = requested {
        return registry.get(id).map(Cow::Borrowed).ok_or_else(|| {
            anyhow::anyhow!("agent `{id}` not found; available: {}", available(registry))
        });
    }

    match registry.default_spec(configured_default) {
        Some(spec) => Ok(Cow::Borrowed(spec)),
        None => {
            tracing::debug!("no .agent.md files found, using the built-in agent");
            Ok(Cow::Owned(AgentSpec {
                id: "default".to_string(),
                name: "Default".to_string(),
                description: "Built-in fallback for workspaces with no agent files.".to_string(),
                system_prompt: FALLBACK_SYSTEM_PROMPT.to_string(),
                source_path: PathBuf::from("<built-in>"),
                ..Default::default()
            }))
        }
    }
}

fn available(registry: &AgentRegistry) -> String {
    let ids: Vec<&str> = registry
        .list()
        .iter()
        .map(|spec| spec.id.as_str())
        .collect();
    if ids.is_empty() {
        "(none)".to_string()
    } else {
        ids.join(", ")
    }
}

/// `--provider` and `--model`, expanded to `provider/model` the way `run` does,
/// so both commands read one string the same way.
fn model_overrides(ctx: &AppContext, command: &PlanCommand) -> anyhow::Result<Vec<String>> {
    Ok(
        match (command.provider.as_deref(), command.model.as_deref()) {
            (Some(provider), Some(model)) => vec![format!("{provider}/{model}")],
            (None, Some(model)) => vec![model.to_string()],
            (Some(provider), None) => {
                let (_, config) = ctx
                    .config
                    .provider(Some(provider))
                    .map_err(|err| anyhow::anyhow!("{err}"))?;
                let model = ctx
                    .config
                    .model_for(config, None)
                    .map_err(|err| anyhow::anyhow!("{err}"))?;
                vec![format!("{provider}/{model}")]
            }
            (None, None) => Vec::new(),
        },
    )
}

fn build_provider(
    ctx: &AppContext,
    provider_id: &str,
    model: &str,
) -> anyhow::Result<Arc<dyn Provider>> {
    let (id, config) = ctx
        .config
        .provider(Some(provider_id))
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    harness_llm::from_config(
        &id,
        config,
        model,
        Duration::from_secs(ctx.config.provider.request_timeout_secs),
        ctx.config.provider.max_retries,
    )
    .map_err(to_anyhow)
}

/// The config the executor's router resolves node models against.
///
/// `Executor` has no model-override hook: it re-resolves every node's model from
/// the spec, falling back to the *configured default provider's* model. Handing
/// it the untouched config would make a node that names no model ask the
/// selected provider for the default provider's model, so the command-line
/// choice is written into the config the router sees.
fn executor_config(ctx: &AppContext, resolved: &ResolvedModel) -> Config {
    let mut config = ctx.config.clone();
    config.provider.default = resolved.provider_id.clone();
    if let Some(provider) = config.providers.get_mut(&resolved.provider_id) {
        provider.default_model = Some(resolved.model.clone());
    }
    config
}

/// Isolation is only possible where `git worktree add` would succeed, so the
/// decision is made from the same two facts the worktree code checks. It is
/// reported either way: a run that silently skipped isolation is exactly the
/// thing a user must not have to guess.
fn worktree_decision(root: &Path) -> (bool, String) {
    match git(root, &["rev-parse", "--git-dir"]) {
        Ok(output) if output.status.success() => {}
        Ok(_) => return (false, format!("{} is not a git repository", root.display())),
        Err(err) => return (false, format!("git could not be run: {err}")),
    }

    match git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
        Ok(output) if output.status.success() => (
            true,
            format!(
                "{} is a git repository with a commit to base a checkout on",
                root.display()
            ),
        ),
        Ok(_) => (false, format!("{} has no commits yet", root.display())),
        Err(err) => (false, format!("git could not be run: {err}")),
    }
}

fn git(root: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").arg("-C").arg(root).args(args).output()
}

/// Turns Ctrl-C into a cooperative abort so the executor can stop its workers.
fn install_ctrl_c_handler(handle: ControlHandle) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            handle.abort(Some("interrupted".into()));
        }
    });
}

fn print_plan(graph: &TaskGraph, layers: &[Vec<&TaskNode>]) {
    println!();
    println!(
        "plan: {} node(s) in {} layer(s)",
        graph.nodes.len(),
        layers.len()
    );
    for (index, layer) in layers.iter().enumerate() {
        println!(
            "  layer {} ({} node(s), may run in parallel)",
            index + 1,
            layer.len()
        );
        for node in layer {
            println!(
                "    {:<22} {:<18} {}",
                node.id,
                node.agent.as_deref().unwrap_or("(default)"),
                node.objective
            );
        }
    }

    // Two nodes in one layer claiming one file is a scheduling hazard, and the
    // point of showing it here is that it is visible before the run starts.
    let conflicts = graph.file_conflicts();
    if conflicts.is_empty() {
        println!("  no file conflicts within a layer");
    } else {
        for (file, first, second) in conflicts {
            println!("  warning: `{first}` and `{second}` both claim {file}");
        }
    }
}

/// What the report says about one node's verification, in the three states that
/// matter: nothing ran, ran and passed, ran and failed. A node the executor
/// never reached has no report at all, which is plainly unverified.
fn verification_label(verification: Option<&VerificationReport>) -> &'static str {
    match verification {
        Some(report) if report.valid => "verified",
        Some(report) if report.checked => "verification failed",
        Some(_) => "not verified (no check ran)",
        None => "not verified",
    }
}

fn print_report(report: &ExecutionReport) {
    println!();
    println!("report:");
    for outcome in &report.nodes {
        let verification = verification_label(outcome.verification.as_ref());
        println!(
            "  {:<22} {:<10} attempts={} tokens={} {verification}",
            outcome.id,
            status_label(outcome.status),
            outcome.attempts,
            outcome.tokens
        );

        let Some(verification) = &outcome.verification else {
            continue;
        };
        for check in &verification.checks {
            if !check.passed {
                println!("      check `{}` failed: {}", check.name, check.detail);
            }
        }
        for issue in &verification.issues {
            println!(
                "      [{}] {}: {}",
                severity_label(issue.severity),
                issue.summary,
                first_line(&issue.evidence)
            );
        }
    }

    println!();
    println!(
        "{} succeeded, {} failed, {} skipped",
        report.succeeded,
        report.failed,
        report.skipped()
    );
}

/// Isolation keeps a successful node's checkout rather than deleting it, and the
/// report is the only place the user learns where those edits are. Nothing is
/// merged for them, so the branch is named as the place to integrate from. With
/// isolation off there is nothing to list and nothing extra is printed.
fn print_worktrees(report: &ExecutionReport) {
    let kept: Vec<&NodeOutcome> = report
        .nodes
        .iter()
        .filter(|outcome| outcome.worktree.is_some())
        .collect();
    if kept.is_empty() {
        return;
    }

    println!();
    println!("kept worktrees — the edits are on these branches, nothing was merged automatically:");
    for outcome in kept {
        let Some(worktree) = &outcome.worktree else {
            continue;
        };
        println!(
            "  {:<22} {}  (branch {})",
            outcome.id,
            worktree.path.display(),
            worktree.branch
        );
    }
    println!("integrate with `git merge <branch>` or `git cherry-pick` from the repository root.");
}

async fn print_events(receiver: &mut mpsc::UnboundedReceiver<RoutedEvent>) {
    let color = std::io::stdout().is_terminal();
    let mut in_thinking = false;

    while let Some(routed) = receiver.recv().await {
        // The terminal shows one flat transcript; the lane a sub-agent's events
        // carry is for the socket client, which draws a lane per worker.
        let event = routed.event;
        // Reasoning and answer text arrive interleaved; marking the boundaries
        // keeps a non-TTY transcript readable, where the dim colour is lost.
        let thinking = matches!(event, AgentEvent::ThinkingChunk { .. });
        if in_thinking && !thinking {
            eprintln!();
        }
        if thinking && !in_thinking {
            eprint!("{}", tint("[thinking] ", "2", color));
        }
        in_thinking = thinking;

        match event {
            AgentEvent::SubtaskStart {
                node_id,
                agent_id,
                objective,
                ..
            } => {
                eprintln!();
                eprintln!(
                    "{}",
                    tint(
                        &format!("[subtask {node_id}] {agent_id} starts: {objective}"),
                        "35",
                        color
                    )
                );
            }
            AgentEvent::SubtaskEnd {
                node_id,
                status,
                summary,
                ..
            } => {
                eprintln!();
                eprintln!(
                    "{}",
                    tint(
                        &format!(
                            "[subtask {node_id}] {}: {}",
                            status_label(status),
                            first_line(&summary)
                        ),
                        "35",
                        color
                    )
                );
            }
            AgentEvent::AgentHandoff { from, to, reason } => {
                eprintln!(
                    "{}",
                    tint(&format!("[handoff] {from} -> {to}: {reason}"), "35", color)
                );
            }
            AgentEvent::AssistantChunk { text } => {
                print!("{text}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            AgentEvent::ThinkingChunk { text } => {
                eprint!("{}", tint(&text, "2", color));
            }
            AgentEvent::ToolCallStart {
                name, arguments, ..
            } => {
                eprintln!();
                eprintln!("{}", tint(&format!("-> {name} {arguments}"), "36", color));
            }
            AgentEvent::ToolCallEnd {
                name,
                status,
                output,
                duration_ms,
                ..
            } => {
                let label = match status {
                    ToolCallStatus::Ok => "ok",
                    ToolCallStatus::Error => "error",
                    ToolCallStatus::Rejected => "rejected",
                    ToolCallStatus::Timeout => "timeout",
                };
                eprintln!(
                    "{}",
                    tint(
                        &format!("<- {name} {label} in {duration_ms}ms"),
                        "36",
                        color
                    )
                );
                if status != ToolCallStatus::Ok {
                    eprintln!("{}", tint(&first_line(&output), "33", color));
                }
            }
            AgentEvent::TokenUsage {
                usage,
                budget_remaining,
            } => {
                let suffix = match budget_remaining {
                    Some(left) => format!("  ({left} left)"),
                    None => String::new(),
                };
                eprintln!(
                    "{}",
                    tint(
                        &format!(
                            "   tokens: in={} out={}{suffix}",
                            usage.input_tokens, usage.output_tokens
                        ),
                        "2",
                        color
                    )
                );
            }
            AgentEvent::Guardrail {
                name,
                blocked,
                detail,
            } => {
                eprintln!();
                eprintln!(
                    "{}",
                    tint(
                        &format!("[{name}] {detail}"),
                        if blocked { "31" } else { "33" },
                        color
                    )
                );
            }
            AgentEvent::Error { message } => eprintln!("{}", tint(&message, "31", color)),
            _ => {}
        }
    }
}

fn status_label(status: SubtaskStatus) -> &'static str {
    match status {
        SubtaskStatus::Pending => "pending",
        SubtaskStatus::Running => "running",
        SubtaskStatus::Succeeded => "succeeded",
        SubtaskStatus::Failed => "failed",
        SubtaskStatus::Skipped => "skipped",
    }
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 160 {
        let clipped: String = line.chars().take(160).collect();
        format!("{clipped}…")
    } else {
        line.to_string()
    }
}

fn tint(text: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(valid: bool, checked: bool) -> VerificationReport {
        VerificationReport {
            valid,
            checked,
            checks: Vec::new(),
            issues: Vec::new(),
        }
    }

    #[test]
    fn the_three_verification_states_read_differently() {
        assert_eq!(verification_label(None), "not verified");
        assert_eq!(
            verification_label(Some(&report(false, false))),
            "not verified (no check ran)"
        );
        assert_eq!(verification_label(Some(&report(true, true))), "verified");
        assert_eq!(
            verification_label(Some(&report(false, true))),
            "verification failed"
        );
    }
}
