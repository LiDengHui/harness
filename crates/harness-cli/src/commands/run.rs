//! `harness run`: the headless, single-prompt entry point.
//!
//! This is where the declarative layer pays off: the agent's system prompt, tool
//! whitelist, model and token budget all come from its `.agent.md` file, and the
//! conversation is persisted into the memory DAG so it can be branched or trimmed
//! afterwards.

use std::borrow::Cow;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use harness_agent::{
    AgentLoop, AgentRegistry, AgentRuntime, AgentSpec, MemoryRecorder, ModelRouter,
};
use harness_core::{
    AgentEvent, CompletionReason, GuardrailsConfig, HarnessError, Memory, Message, SessionId,
    ToolCallStatus,
};
use harness_guardrails::{
    BehaviorMonitor, ContentFence, GuardrailPipeline, PiiDetector, SecretScanner, ToolPolicy,
};
use harness_llm::Provider;
use harness_memory::SqliteMemory;
use harness_tools::{ToolContext, ToolRegistry};
use tokio::sync::mpsc;

use crate::commands::RunCommand;
use crate::context::AppContext;

/// Used only when a workspace has no `.agent.md` files at all.
const FALLBACK_SYSTEM_PROMPT: &str = "\
You are a coding agent working inside a project workspace.
Use the available tools to inspect and modify files. Prefer reading before writing.
Keep answers short and concrete.";

pub(crate) async fn execute(command: RunCommand, ctx: &AppContext) -> anyhow::Result<()> {
    let prompt = read_prompt(command.prompt.as_deref())?;

    let registry = AgentRegistry::load(&ctx.workspace_root).map_err(to_anyhow)?;
    let spec = select_spec(
        &registry,
        command.agent.as_deref(),
        &ctx.config.harness.default_agent,
    )?;

    let router = ModelRouter::new(ctx.config.clone());
    let overrides = model_overrides(ctx, &command)?;
    // The top tier of the precedence rule. An unrecognised level is refused
    // here rather than sent: the gateway answers one with an opaque upstream
    // failure, which names neither the flag nor the value.
    let requested_effort = match command.effort.as_deref() {
        Some(raw) => Some(harness_core::resolve_thinking_effort(raw).ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported --effort `{raw}`; supported: {}",
                harness_core::SUPPORTED_THINKING_EFFORTS.join(", ")
            )
        })?),
        None => None,
    };
    let resolved = router
        .resolve(Some(spec.as_ref()), None, &overrides)
        .map_err(to_anyhow)?;
    let provider = build_provider(ctx, &resolved.provider_id, &resolved.model)?;

    let memory: Arc<dyn Memory> =
        Arc::new(SqliteMemory::open(ctx.db_path()).await.map_err(to_anyhow)?);
    let session_id = memory
        .create_session(Some(&format!("run:{}", spec.id)))
        .await
        .map_err(to_anyhow)?;

    let all_tools = ToolRegistry::with_builtins();
    let runtime = AgentRuntime::build_with_overrides(
        spec.as_ref(),
        &router,
        Arc::clone(&provider),
        &all_tools,
        Arc::clone(&memory),
        &overrides,
    )
    .map_err(to_anyhow)?;

    let tool_ctx = ToolContext::new(ctx.workspace_root.clone(), ctx.config.tools.clone());
    let mut agent_config = runtime.agent_config(session_id);
    // Precedence, highest first: the flag, the agent's declaration (already on
    // the config), then the configured default.
    let effort = ctx
        .config
        .thinking
        .resolve(requested_effort, agent_config.reasoning_effort.as_deref())
        .to_string();
    agent_config.reasoning_effort = Some(effort);
    // The loop's own default is the same value; reading it from the config is
    // what makes `[context] trim_threshold_chars` able to override it.
    agent_config.context_trim_threshold = ctx.config.context.trim_threshold_chars;
    if let Some(limit) = command.max_tokens {
        agent_config.budget = Some(Arc::new(harness_agent::TokenBudget::new(limit)));
    }
    // The pipeline is attached only when the config asks for it, and the run says
    // so once: a security control the operator cannot see is one they cannot trust.
    if let Some(pipeline) = guardrail_pipeline(&ctx.config.guardrails)? {
        eprintln!("-- guardrails on: {}", pipeline.names().join(", "));
        agent_config = agent_config.with_guardrails(pipeline);
    }
    let agent = AgentLoop::new(
        agent_config,
        Arc::clone(&provider),
        runtime.tools.clone(),
        tool_ctx,
    );

    let (events, mut receiver) = mpsc::unbounded_channel::<AgentEvent>();
    let printer =
        tokio::spawn(async move { print_events(&mut receiver, command.json_events).await });

    let (control_handle, mut control) = harness_agent::ControlHandle::channel();
    install_ctrl_c_handler(control_handle);

    let mut history = vec![Message::user(prompt)];
    // Persisted turn by turn rather than once at the end: an abort, a provider
    // error or a killed process must not lose the whole conversation. The loop
    // hands each turn to the recorder as it completes, so nothing is appended
    // afterwards — a second pass would duplicate what the recorder wrote.
    let recorder = MemoryRecorder::new(Arc::clone(&memory), session_id);
    let outcome = agent
        .run_persisting(&mut history, &events, &mut control, &recorder, 0)
        .await?;

    drop(events);
    let _ = printer.await;

    summarize(&outcome, &spec.id, &resolved.model, session_id);

    match outcome.reason {
        CompletionReason::EndTurn => Ok(()),
        other => Err(anyhow::anyhow!("run ended early: {other:?}")),
    }
}

fn to_anyhow(err: HarnessError) -> anyhow::Error {
    anyhow::anyhow!("{err}")
}

/// The guard pipeline the config asks for, or `None` when it is switched off.
///
/// The guard set mirrors [`GuardrailPipeline::standard`]; the loop detector's
/// threshold is the one from the config rather than the library default.
fn guardrail_pipeline(config: &GuardrailsConfig) -> anyhow::Result<Option<Arc<GuardrailPipeline>>> {
    if !config.enabled {
        return Ok(None);
    }
    Ok(Some(Arc::new(GuardrailPipeline::new(vec![
        Arc::new(SecretScanner::new().map_err(to_anyhow)?),
        Arc::new(PiiDetector::new().map_err(to_anyhow)?),
        Arc::new(ContentFence::new().map_err(to_anyhow)?),
        Arc::new(ToolPolicy::destructive_defaults().map_err(to_anyhow)?),
        Arc::new(BehaviorMonitor::new(config.max_identical_tool_calls)),
    ]))))
}

fn read_prompt(from_flag: Option<&str>) -> anyhow::Result<String> {
    if let Some(prompt) = from_flag {
        return Ok(prompt.to_string());
    }

    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .context("failed to read the prompt from stdin")?;

    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        anyhow::bail!("no prompt given; pass --prompt or pipe one in on stdin");
    }
    Ok(trimmed.to_string())
}

/// The agent to run: the one asked for, else the configured default, else a
/// built-in fallback so `harness run` works in a workspace with no agent files.
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

/// `--provider` and `--model` are the top precedence tier.
///
/// A provider given without a model is expanded here to `provider/model` using
/// that provider's own default, so the router and the runtime resolve the same
/// thing from one string.
fn model_overrides(ctx: &AppContext, command: &RunCommand) -> anyhow::Result<Vec<String>> {
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

/// Turns Ctrl-C into a cooperative abort so the loop can kill its tools.
fn install_ctrl_c_handler(handle: harness_agent::ControlHandle) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            handle.abort(Some("interrupted".into()));
        }
    });
}

async fn print_events(receiver: &mut mpsc::UnboundedReceiver<AgentEvent>, json_events: bool) {
    let color = std::io::stdout().is_terminal();
    let mut in_thinking = false;

    while let Some(event) = receiver.recv().await {
        if json_events {
            match serde_json::to_string(&event) {
                Ok(line) => println!("{line}"),
                Err(err) => eprintln!("failed to encode event: {err}"),
            }
            continue;
        }

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

fn summarize(
    outcome: &harness_agent::RunOutcome,
    agent_id: &str,
    model: &str,
    session_id: SessionId,
) {
    if !outcome.final_text.is_empty() {
        println!();
    }
    eprintln!(
        "-- {agent_id} on {model}: {:?}, {} turns, {} tokens",
        outcome.reason,
        outcome.turns,
        outcome.usage.total()
    );
    eprintln!("-- session {session_id} (see `harness memory tree`)");
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
    use harness_guardrails::GuardContext;

    fn config(enabled: bool, max_identical_tool_calls: usize) -> GuardrailsConfig {
        GuardrailsConfig {
            enabled,
            max_identical_tool_calls,
        }
    }

    #[test]
    fn guardrails_switched_off_build_no_pipeline() {
        assert!(guardrail_pipeline(&config(false, 3)).unwrap().is_none());
    }

    #[test]
    fn guardrails_switched_on_build_the_standard_guard_set() {
        let pipeline = guardrail_pipeline(&config(true, 3))
            .unwrap()
            .expect("a pipeline when enabled");
        assert_eq!(
            pipeline.names(),
            vec![
                "secret_scanner",
                "pii_detector",
                "content_fence",
                "tool_policy",
                "behavior_monitor",
            ]
        );
    }

    #[tokio::test]
    async fn the_loop_detector_uses_the_configured_threshold() {
        // A threshold of two refuses the third identical call, where the
        // library's own default of three would still allow it.
        let pipeline = guardrail_pipeline(&config(true, 2))
            .unwrap()
            .expect("a pipeline when enabled");
        let call = GuardContext::tool_call("list_dir", serde_json::json!({ "path": "." }));

        assert_eq!(pipeline.inspect(&call).await.unwrap().blocked, None);
        assert_eq!(pipeline.inspect(&call).await.unwrap().blocked, None);
        let third = pipeline.inspect(&call).await.unwrap();
        assert_eq!(
            third.blocked.map(|report| report.name),
            Some("behavior_monitor".to_string())
        );
    }
}
