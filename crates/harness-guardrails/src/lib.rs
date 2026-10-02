//! Deterministic and model-based guardrails.
//!
//! A guardrail inspects one piece of content — a user message, an assistant
//! answer, a tool call or a tool result — and returns a verdict: allow it,
//! rewrite it, or refuse it. [`GuardrailPipeline`] runs a list of them in
//! order, so the agent loop has a single place to ask whether content may
//! enter the conversation.
//!
//! The deterministic guards are cheap enough to run on every turn. Model-based
//! review sits behind [`LlmJudge`], so nothing in this crate reaches the
//! network on its own.

mod behavior;
mod fence;
mod judge;
mod pii;
mod policy;
mod secrets;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use harness_core::{PermissionMode, Result};
use serde_json::Value;

pub use behavior::BehaviorMonitor;
pub use fence::ContentFence;
pub use judge::{LlmJudge, LlmJudgeGuard, NoJudge, ScriptedJudge};
pub use pii::PiiDetector;
pub use policy::{PolicyAction, ToolPolicy};
pub use secrets::SecretScanner;

/// Where a piece of inspected content came from.
///
/// Guards that only make sense for one source read this: a content fence is
/// about untrusted tool output, a tool policy about the call that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardSource {
    UserMessage,
    AssistantOutput,
    ToolCall,
    ToolResult,
}

/// What one guard decided about the content it was shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    Allow,
    /// Refuse the content; the caller substitutes a refusal in its place.
    Block {
        detail: String,
    },
    /// The content may proceed, but only if a human approves it first.
    ///
    /// Distinct from `Block` because a hard refusal is not approvable: a deny
    /// rule is the operator saying "never", and the permission tier cannot
    /// reopen it.
    Ask {
        detail: String,
    },
    /// Keep the content, with `text` standing in for what was inspected.
    Redact {
        text: String,
        detail: String,
    },
}

/// One piece of content, plus the provenance a guard may need to judge it.
#[derive(Debug, Clone, PartialEq)]
pub struct GuardContext {
    pub source: GuardSource,
    /// The text under inspection. For a tool call this is the serialized
    /// arguments, which is also what a redaction rewrites.
    pub text: String,
    /// Set for [`GuardSource::ToolCall`].
    pub tool_name: Option<String>,
    /// Set for [`GuardSource::ToolCall`].
    pub arguments: Option<Value>,
    /// The permission tier in force for this call, when the caller has one.
    ///
    /// A tool policy consults it so that a per-run override can outrank the
    /// tier the policy was built with; `None` means "use the policy's own".
    pub permission_mode: Option<PermissionMode>,
}

impl GuardContext {
    pub fn user_message(text: impl Into<String>) -> Self {
        Self::text(GuardSource::UserMessage, text)
    }

    pub fn assistant_output(text: impl Into<String>) -> Self {
        Self::text(GuardSource::AssistantOutput, text)
    }

    pub fn tool_result(text: impl Into<String>) -> Self {
        Self::text(GuardSource::ToolResult, text)
    }

    pub fn tool_call(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            source: GuardSource::ToolCall,
            text: arguments.to_string(),
            tool_name: Some(name.into()),
            arguments: Some(arguments),
            permission_mode: None,
        }
    }

    /// Sets the tier a tool policy judges this call under.
    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = Some(mode);
        self
    }

    fn text(source: GuardSource, text: impl Into<String>) -> Self {
        Self {
            source,
            text: text.into(),
            tool_name: None,
            arguments: None,
            permission_mode: None,
        }
    }
}

/// Inspects content and returns a verdict on it.
#[async_trait]
pub trait Guardrail: Send + Sync {
    /// Stable identifier, reported on the event stream.
    fn name(&self) -> &str;

    /// Inspects one piece of content. A guard that does not apply to a source
    /// returns [`GuardVerdict::Allow`] without touching the text.
    async fn inspect(&self, ctx: &GuardContext) -> Result<GuardVerdict>;

    /// Clears per-run state. Stateless guards keep the default.
    fn reset(&self) {}
}

/// A guard that fired, in the shape the event stream reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardReport {
    pub name: String,
    pub detail: String,
}

/// The outcome of running a whole pipeline over one piece of content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// The text after every `Redact` was applied, in guard order.
    pub text: String,
    /// The guard that blocked, if any. The pipeline stops at the first one.
    pub blocked: Option<GuardReport>,
    /// The guard that asked for approval, if any.
    ///
    /// A block outranks an ask: a call a deny rule refused is never put to the
    /// user, because the operator already answered "no".
    pub approval: Option<GuardReport>,
    /// Guards that rewrote the text, in the order they ran.
    pub redactions: Vec<GuardReport>,
}

impl Inspection {
    /// The outcome when no guard had anything to say.
    pub fn allowed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            blocked: None,
            approval: None,
            redactions: Vec::new(),
        }
    }
}

/// An ordered set of guardrails.
pub struct GuardrailPipeline {
    guards: Vec<Arc<dyn Guardrail>>,
}

impl GuardrailPipeline {
    pub fn new(guards: Vec<Arc<dyn Guardrail>>) -> Self {
        Self { guards }
    }

    /// The deterministic set a run gets when no model-based judge is available.
    pub fn standard() -> Result<Self> {
        Ok(Self::new(vec![
            Arc::new(SecretScanner::new()?),
            Arc::new(PiiDetector::new()?),
            Arc::new(ContentFence::new()?),
            Arc::new(ToolPolicy::destructive_defaults()?),
            Arc::new(BehaviorMonitor::new(3)),
        ]))
    }

    pub fn names(&self) -> Vec<&str> {
        self.guards.iter().map(|guard| guard.name()).collect()
    }

    /// Clears per-run state on every guard; the loop calls this at run start.
    pub fn reset(&self) {
        for guard in &self.guards {
            guard.reset();
        }
    }

    /// Runs the guards in order over `ctx`.
    ///
    /// The first `Block` wins and stops the pass; every `Redact` is applied in
    /// sequence, so a later guard inspects what an earlier one rewrote.
    pub async fn inspect(&self, ctx: &GuardContext) -> Result<Inspection> {
        let mut text = ctx.text.clone();
        let mut redactions = Vec::new();
        let mut approval: Option<GuardReport> = None;

        for guard in &self.guards {
            let current = GuardContext {
                text: text.clone(),
                ..ctx.clone()
            };
            match guard.inspect(&current).await? {
                GuardVerdict::Allow => {}
                GuardVerdict::Block { detail } => {
                    return Ok(Inspection {
                        text,
                        blocked: Some(GuardReport {
                            name: guard.name().to_string(),
                            detail,
                        }),
                        approval: None,
                        redactions,
                    });
                }
                GuardVerdict::Ask { detail } => {
                    // Recorded rather than returned on the spot: a later guard
                    // may still refuse the content outright, and a refusal is
                    // not something a human can approve away.
                    approval = Some(GuardReport {
                        name: guard.name().to_string(),
                        detail,
                    });
                }
                GuardVerdict::Redact {
                    text: rewritten,
                    detail,
                } => {
                    redactions.push(GuardReport {
                        name: guard.name().to_string(),
                        detail,
                    });
                    text = rewritten;
                }
            }
        }

        Ok(Inspection {
            text,
            blocked: None,
            approval,
            redactions,
        })
    }
}

impl fmt::Debug for GuardrailPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardrailPipeline")
            .field("guards", &self.names())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Replays one verdict and records the text it was shown, so a test can
    /// observe both the call order and what an earlier redaction left behind.
    struct Scripted {
        name: String,
        verdict: GuardVerdict,
        log: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Scripted {
        fn make(
            name: &str,
            verdict: GuardVerdict,
            log: &Arc<Mutex<Vec<(String, String)>>>,
        ) -> Arc<dyn Guardrail> {
            Arc::new(Self {
                name: name.to_string(),
                verdict,
                log: Arc::clone(log),
            })
        }
    }

    #[async_trait]
    impl Guardrail for Scripted {
        fn name(&self) -> &str {
            &self.name
        }

        async fn inspect(&self, ctx: &GuardContext) -> Result<GuardVerdict> {
            self.log
                .lock()
                .unwrap()
                .push((self.name.clone(), ctx.text.clone()));
            Ok(self.verdict.clone())
        }
    }

    fn redact(text: &str) -> GuardVerdict {
        GuardVerdict::Redact {
            text: text.to_string(),
            detail: "rewrote it".into(),
        }
    }

    #[tokio::test]
    async fn guards_run_in_order_and_each_sees_the_previous_rewrite() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let pipeline = GuardrailPipeline::new(vec![
            Scripted::make("first", redact("one"), &log),
            Scripted::make("second", redact("two"), &log),
        ]);

        let outcome = pipeline
            .inspect(&GuardContext::user_message("raw"))
            .await
            .unwrap();

        assert_eq!(outcome.text, "two");
        assert_eq!(outcome.blocked, None);
        assert_eq!(
            outcome
                .redactions
                .iter()
                .map(|report| report.name.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );

        let calls = log.lock().unwrap().clone();
        assert_eq!(calls[0], ("first".to_string(), "raw".to_string()));
        assert_eq!(calls[1], ("second".to_string(), "one".to_string()));
    }

    #[tokio::test]
    async fn a_block_stops_the_pass_and_keeps_earlier_redactions() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let pipeline = GuardrailPipeline::new(vec![
            Scripted::make("redactor", redact("clean"), &log),
            Scripted::make(
                "blocker",
                GuardVerdict::Block {
                    detail: "not allowed".into(),
                },
                &log,
            ),
            Scripted::make("never", redact("unreached"), &log),
        ]);

        let outcome = pipeline
            .inspect(&GuardContext::assistant_output("raw"))
            .await
            .unwrap();

        assert_eq!(outcome.text, "clean");
        assert_eq!(
            outcome.blocked,
            Some(GuardReport {
                name: "blocker".into(),
                detail: "not allowed".into(),
            })
        );
        assert_eq!(outcome.redactions.len(), 1);
        assert_eq!(outcome.redactions[0].name, "redactor");

        let names: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        assert_eq!(names, vec!["redactor", "blocker"]);
    }

    #[tokio::test]
    async fn an_ask_is_recorded_and_a_hard_block_still_outranks_it() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let pipeline = GuardrailPipeline::new(vec![Scripted::make(
            "asker",
            GuardVerdict::Ask {
                detail: "needs a human".into(),
            },
            &log,
        )]);

        let outcome = pipeline
            .inspect(&GuardContext::tool_call(
                "shell",
                serde_json::json!({ "command": "ls" }),
            ))
            .await
            .unwrap();
        assert_eq!(outcome.blocked, None);
        assert_eq!(
            outcome.approval,
            Some(GuardReport {
                name: "asker".into(),
                detail: "needs a human".into(),
            })
        );

        // A question cannot reopen a refusal: a later block wins outright.
        let pipeline = GuardrailPipeline::new(vec![
            Scripted::make(
                "asker",
                GuardVerdict::Ask {
                    detail: "needs a human".into(),
                },
                &log,
            ),
            Scripted::make(
                "blocker",
                GuardVerdict::Block {
                    detail: "never".into(),
                },
                &log,
            ),
        ]);
        let outcome = pipeline
            .inspect(&GuardContext::tool_call("shell", serde_json::json!({})))
            .await
            .unwrap();
        assert!(outcome.approval.is_none());
        assert_eq!(
            outcome.blocked.map(|report| report.name),
            Some("blocker".into())
        );
    }

    #[tokio::test]
    async fn a_pipeline_of_guards_that_do_not_apply_leaves_the_text_alone() {
        let pipeline = GuardrailPipeline::standard().unwrap();
        let outcome = pipeline
            .inspect(&GuardContext::user_message("summarise the changelog"))
            .await
            .unwrap();

        assert_eq!(outcome.text, "summarise the changelog");
        assert_eq!(outcome.blocked, None);
        assert!(outcome.redactions.is_empty());
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
    async fn reset_clears_the_loop_detector_between_runs() {
        let pipeline = GuardrailPipeline::standard().unwrap();
        let call = || GuardContext::tool_call("list_dir", serde_json::json!({ "path": "." }));

        for _ in 0..3 {
            assert_eq!(pipeline.inspect(&call()).await.unwrap().blocked, None);
        }
        assert!(pipeline.inspect(&call()).await.unwrap().blocked.is_some());

        pipeline.reset();
        assert_eq!(pipeline.inspect(&call()).await.unwrap().blocked, None);
    }
}
