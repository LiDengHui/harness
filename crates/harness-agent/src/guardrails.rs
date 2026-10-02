//! Screening glue between the guardrail pipeline and the agent loop.
//!
//! The loop asks this module to screen every piece of content that would enter
//! the conversation. It owns the mapping from a guard verdict onto the event
//! stream and onto whatever replaces the content: a redaction rewrites it, a
//! block substitutes a refusal.

use harness_core::{AgentEvent, PermissionMode, Result, ToolCallStatus};
use harness_guardrails::{GuardContext, GuardReport, GuardrailPipeline, Inspection};
use harness_llm::{Message, ToolCall};
use harness_tools::ToolOutput;
use tokio::sync::mpsc;

/// Runs `pipeline` over one piece of content, emitting an event per guard that
/// fired.
///
/// Without a pipeline the content passes through untouched, which is the cheap
/// path every run without guardrails takes.
async fn screen(
    pipeline: Option<&GuardrailPipeline>,
    ctx: &GuardContext,
    events: &mpsc::UnboundedSender<AgentEvent>,
) -> Result<Inspection> {
    let Some(pipeline) = pipeline else {
        return Ok(Inspection::allowed(ctx.text.clone()));
    };

    let inspection = pipeline.inspect(ctx).await?;
    for report in &inspection.redactions {
        emit_report(events, report, false);
    }
    if let Some(report) = &inspection.blocked {
        emit_report(events, report, true);
    }
    Ok(inspection)
}

/// Screens text and returns what should replace it: the rewritten text, or a
/// refusal when a guard blocked it.
async fn screen_text(
    pipeline: Option<&GuardrailPipeline>,
    ctx: GuardContext,
    events: &mpsc::UnboundedSender<AgentEvent>,
) -> Result<String> {
    let inspection = screen(pipeline, &ctx, events).await?;
    Ok(match &inspection.blocked {
        Some(report) => refusal_text(report),
        None => inspection.text,
    })
}

/// Screens a user message in place.
pub(crate) async fn screen_user_message(
    pipeline: Option<&GuardrailPipeline>,
    message: &mut Message,
    events: &mpsc::UnboundedSender<AgentEvent>,
) -> Result<()> {
    if message.text().is_empty() {
        return Ok(());
    }
    let screened =
        screen_text(pipeline, GuardContext::user_message(message.text()), events).await?;
    message.content = Some(screened);
    Ok(())
}

/// Screens the text of one assistant turn.
pub(crate) async fn screen_assistant_output(
    pipeline: Option<&GuardrailPipeline>,
    text: &str,
    events: &mpsc::UnboundedSender<AgentEvent>,
) -> Result<String> {
    screen_text(pipeline, GuardContext::assistant_output(text), events).await
}

/// What screening decided about one tool call.
#[derive(Debug)]
pub(crate) enum Screened {
    /// The call may run.
    Allowed,
    /// A guard refused it outright, and the permission tier cannot reopen that.
    Refused(GuardReport),
    /// A guard or the permission tier wants a human to approve it first.
    NeedsApproval(GuardReport),
}

/// Screens one tool call.
///
/// `permission_mode` is this run's tier when it differs from the one the policy
/// was built with; it is attached to the context so a per-run choice reaches a
/// policy that was constructed for the whole session. A redaction rewrites the
/// arguments in place, which keeps a call alive with a scrubbed payload rather
/// than dropping it.
pub(crate) async fn screen_tool_call(
    pipeline: Option<&GuardrailPipeline>,
    call: &mut ToolCall,
    events: &mpsc::UnboundedSender<AgentEvent>,
    permission_mode: Option<PermissionMode>,
) -> Result<Screened> {
    let mut ctx = GuardContext::tool_call(&call.name, call.arguments.clone());
    if let Some(mode) = permission_mode {
        ctx = ctx.with_permission_mode(mode);
    }
    let inspection = screen(pipeline, &ctx, events).await?;

    if let Some(report) = &inspection.blocked {
        return Ok(Screened::Refused(report.clone()));
    }
    if !inspection.redactions.is_empty() {
        // The pipeline rewrote the serialized arguments; keep the call only
        // when the rewritten form is still valid JSON.
        if let Ok(value) = serde_json::from_str(&inspection.text) {
            call.arguments = value;
        }
    }
    match &inspection.approval {
        Some(report) => Ok(Screened::NeedsApproval(report.clone())),
        None => Ok(Screened::Allowed),
    }
}

/// Answers a tool call a guardrail refused.
///
/// The call is never executed, but the provider rejects a request whose tool
/// call has no matching result, so it is answered with a refusal that the
/// model can tell apart from a tool failure.
pub(crate) fn refuse_tool_call(
    events: &mpsc::UnboundedSender<AgentEvent>,
    call: &ToolCall,
    report: &GuardReport,
) -> ToolOutput {
    let output = ToolOutput::error(refusal_text(report));
    let _ = events.send(AgentEvent::ToolCallEnd {
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        status: ToolCallStatus::Rejected,
        output: output.content.clone(),
        duration_ms: 0,
    });
    output
}

/// Screens a tool's output before it joins the history, so a secret or an
/// injected instruction in a file never reaches the next request.
pub(crate) async fn screen_tool_output(
    pipeline: Option<&GuardrailPipeline>,
    output: ToolOutput,
    events: &mpsc::UnboundedSender<AgentEvent>,
) -> Result<ToolOutput> {
    let ctx = GuardContext::tool_result(output.content.clone());
    let inspection = screen(pipeline, &ctx, events).await?;

    Ok(match &inspection.blocked {
        Some(report) => ToolOutput::error(refusal_text(report)),
        None => ToolOutput {
            content: inspection.text,
            ..output
        },
    })
}

fn emit_report(events: &mpsc::UnboundedSender<AgentEvent>, report: &GuardReport, blocked: bool) {
    let _ = events.send(AgentEvent::Guardrail {
        name: report.name.clone(),
        blocked,
        detail: report.detail.clone(),
    });
}

/// The text that stands in for content a guardrail refused.
///
/// It names the guard so the model can tell a policy refusal from a tool
/// failure and stop retrying the same thing.
fn refusal_text(report: &GuardReport) -> String {
    format!("refused by guardrail `{}`: {}", report.name, report.detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_guardrails::{SecretScanner, ToolPolicy};
    use serde_json::json;
    use std::sync::Arc;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            name: name.into(),
            arguments: json!({ "path": "a.txt" }),
        }
    }

    fn pipeline(policy: ToolPolicy) -> GuardrailPipeline {
        GuardrailPipeline::new(vec![Arc::new(policy)])
    }

    #[tokio::test]
    async fn a_call_no_guard_mentions_is_allowed() {
        let (events, _rx) = mpsc::unbounded_channel();
        let mut call = call("read_file");

        let screened = screen_tool_call(None, &mut call, &events, None)
            .await
            .unwrap();
        assert!(matches!(screened, Screened::Allowed), "{screened:?}");
    }

    #[tokio::test]
    async fn a_deny_is_refused_and_an_ask_needs_approval() {
        let (events, _rx) = mpsc::unbounded_channel();

        let mut denying = ToolPolicy::new();
        denying.deny_tool("shell", "no shell");
        let mut shell = call("shell");
        let screened = screen_tool_call(Some(&pipeline(denying)), &mut shell, &events, None)
            .await
            .unwrap();
        assert!(matches!(screened, Screened::Refused(_)), "{screened:?}");

        let mut asking = ToolPolicy::new();
        asking.ask_tool("shell", "shell needs a human");
        let mut shell = call("shell");
        let screened = screen_tool_call(Some(&pipeline(asking)), &mut shell, &events, None)
            .await
            .unwrap();
        assert!(
            matches!(screened, Screened::NeedsApproval(_)),
            "{screened:?}"
        );
    }

    #[tokio::test]
    async fn the_tier_attached_to_the_context_reaches_the_policy() {
        let (events, _rx) = mpsc::unbounded_channel();
        // The policy was built for full auto, but this run's override asks.
        let policy = ToolPolicy::new().with_mode(PermissionMode::FullAuto);

        let mut shell = call("shell");
        let screened = screen_tool_call(
            Some(&pipeline(policy)),
            &mut shell,
            &events,
            Some(PermissionMode::AlwaysAsk),
        )
        .await
        .unwrap();
        assert!(
            matches!(screened, Screened::NeedsApproval(_)),
            "{screened:?}"
        );
    }

    #[tokio::test]
    async fn a_redaction_rewrites_the_arguments_and_still_allows_the_call() {
        let (events, _rx) = mpsc::unbounded_channel();
        let pipeline = GuardrailPipeline::new(vec![Arc::new(SecretScanner::new().unwrap())]);
        let mut call = ToolCall {
            id: "call_1".into(),
            name: "write_file".into(),
            arguments: json!({ "path": "a.txt", "content": "key AKIAIOSFODNN7EXAMPLE" }),
        };

        let screened = screen_tool_call(Some(&pipeline), &mut call, &events, None)
            .await
            .unwrap();
        assert!(matches!(screened, Screened::Allowed), "{screened:?}");

        // The secret never survives into the call that would be executed.
        let rendered = call.arguments.to_string();
        assert!(!rendered.contains("AKIAIOSFODNN7EXAMPLE"), "{rendered}");
        assert!(rendered.contains("[redacted:aws_key]"), "{rendered}");
    }
}
