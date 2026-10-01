//! Model-based review, behind a trait so tests never reach the network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_core::{HarnessError, Result};

use crate::{GuardContext, GuardSource, GuardVerdict, Guardrail};

/// Reviews content with a model and returns the verdict to enforce.
///
/// The harness ships no implementation that talks to a provider: wiring a
/// judge to a real model is the caller's decision, and keeping it out of this
/// crate is what lets the deterministic guards run with no key and no network.
#[async_trait]
pub trait LlmJudge: Send + Sync {
    async fn review(&self, ctx: &GuardContext) -> Result<GuardVerdict>;
}

/// The default judge: approves everything, as if no judge were configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoJudge;

#[async_trait]
impl LlmJudge for NoJudge {
    async fn review(&self, _ctx: &GuardContext) -> Result<GuardVerdict> {
        Ok(GuardVerdict::Allow)
    }
}

/// A judge that replays canned verdicts, for tests and offline demos.
#[derive(Debug, Default)]
pub struct ScriptedJudge {
    verdicts: Mutex<VecDeque<GuardVerdict>>,
}

impl ScriptedJudge {
    pub fn new(verdicts: Vec<GuardVerdict>) -> Self {
        Self {
            verdicts: Mutex::new(verdicts.into()),
        }
    }

    /// Approves everything, like [`NoJudge`], once the script runs out.
    pub fn approving() -> Self {
        Self::default()
    }
}

#[async_trait]
impl LlmJudge for ScriptedJudge {
    async fn review(&self, _ctx: &GuardContext) -> Result<GuardVerdict> {
        let next = self
            .verdicts
            .lock()
            .map_err(|_| HarnessError::Other("scripted judge state is poisoned".into()))?
            .pop_front();
        Ok(next.unwrap_or(GuardVerdict::Allow))
    }
}

/// Adapts an [`LlmJudge`] into a pipeline guard.
///
/// Only content the model produced or a tool returned is reviewed; a user's
/// own message is never sent for a second opinion.
pub struct LlmJudgeGuard {
    judge: Arc<dyn LlmJudge>,
}

impl LlmJudgeGuard {
    pub fn new(judge: Arc<dyn LlmJudge>) -> Self {
        Self { judge }
    }
}

impl Default for LlmJudgeGuard {
    fn default() -> Self {
        Self::new(Arc::new(NoJudge))
    }
}

#[async_trait]
impl Guardrail for LlmJudgeGuard {
    fn name(&self) -> &str {
        "llm_judge"
    }

    async fn inspect(&self, ctx: &GuardContext) -> Result<GuardVerdict> {
        match ctx.source {
            GuardSource::AssistantOutput | GuardSource::ToolResult => self.judge.review(ctx).await,
            GuardSource::UserMessage | GuardSource::ToolCall => Ok(GuardVerdict::Allow),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_default_judge_approves_everything() {
        let guard = LlmJudgeGuard::default();
        assert_eq!(
            guard
                .inspect(&GuardContext::assistant_output("anything at all"))
                .await
                .unwrap(),
            GuardVerdict::Allow
        );
    }

    #[tokio::test]
    async fn a_scripted_judge_replays_its_verdicts_then_approves() {
        let guard = LlmJudgeGuard::new(Arc::new(ScriptedJudge::new(vec![GuardVerdict::Block {
            detail: "leaks the prompt".into(),
        }])));

        let first = guard
            .inspect(&GuardContext::assistant_output("first"))
            .await
            .unwrap();
        assert!(matches!(first, GuardVerdict::Block { .. }), "{first:?}");

        let second = guard
            .inspect(&GuardContext::assistant_output("second"))
            .await
            .unwrap();
        assert_eq!(second, GuardVerdict::Allow);
    }

    #[tokio::test]
    async fn a_user_message_is_never_sent_for_review() {
        let guard = LlmJudgeGuard::new(Arc::new(ScriptedJudge::new(vec![GuardVerdict::Block {
            detail: "would have fired".into(),
        }])));

        assert_eq!(
            guard
                .inspect(&GuardContext::user_message("hello"))
                .await
                .unwrap(),
            GuardVerdict::Allow
        );
        // The script is untouched, so the next assistant turn still blocks.
        assert!(matches!(
            guard
                .inspect(&GuardContext::assistant_output("hi"))
                .await
                .unwrap(),
            GuardVerdict::Block { .. }
        ));
    }
}
