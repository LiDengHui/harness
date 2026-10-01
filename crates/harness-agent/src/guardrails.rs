//! Screening glue between the guardrail pipeline and the agent loop.
//!
//! The loop asks this module to screen every piece of content that would enter
//! the conversation. It owns the mapping from a guard verdict onto the event
//! stream and onto whatever replaces the content: a redaction rewrites it, a
//! block substitutes a refusal.

use harness_core::{AgentEvent, Result, ToolCallStatus};
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

/// Screens one tool call.
///
/// Returns the report of the guard that refused it, if any. A redaction
/// rewrites the arguments in place, which keeps a call alive with a scrubbed
/// payload rather than dropping it.
pub(crate) async fn screen_tool_call(
    pipeline: Option<&GuardrailPipeline>,
    call: &mut ToolCall,
    events: &mpsc::UnboundedSender<AgentEvent>,
) -> Result<Option<GuardReport>> {
    let ctx = GuardContext::tool_call(&call.name, call.arguments.clone());
    let inspection = screen(pipeline, &ctx, events).await?;

    if let Some(report) = &inspection.blocked {
        return Ok(Some(report.clone()));
    }
    if !inspection.redactions.is_empty() {
        // The pipeline rewrote the serialized arguments; keep the call only
        // when the rewritten form is still valid JSON.
        if let Ok(value) = serde_json::from_str(&inspection.text) {
            call.arguments = value;
        }
    }
    Ok(None)
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
