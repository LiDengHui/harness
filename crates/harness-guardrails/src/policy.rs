//! Allow/deny rules over the tool calls a model asks for.

use async_trait::async_trait;
use harness_core::{HarnessError, Result};
use regex::Regex;

use crate::{GuardContext, GuardSource, GuardVerdict, Guardrail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    Allow,
    Deny,
}

#[derive(Debug)]
struct Rule {
    /// Exact tool name; `None` matches every tool.
    tool: Option<String>,
    /// Regex the serialized arguments must match; `None` matches any call.
    arguments: Option<Regex>,
    action: PolicyAction,
    detail: String,
}

impl Rule {
    fn matches(&self, tool: &str, arguments: &str) -> bool {
        let tool_ok = match self.tool.as_deref() {
            Some(name) => name == tool,
            None => true,
        };
        let arguments_ok = match &self.arguments {
            Some(regex) => regex.is_match(arguments),
            None => true,
        };
        tool_ok && arguments_ok
    }
}

/// Refuses tool calls that match a deny rule.
///
/// `Allow` rules are consulted first, so an exception always wins over a
/// broader `Deny` no matter which was declared first. A call that matches no
/// rule at all is allowed, which keeps a policy written as a denylist a
/// denylist.
#[derive(Debug, Default)]
pub struct ToolPolicy {
    rules: Vec<Rule>,
}

impl ToolPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// The rules a run gets by default: recursive deletes aimed at the
    /// filesystem root, which no agent task legitimately needs.
    pub fn destructive_defaults() -> Result<Self> {
        let mut policy = Self::new();
        policy.deny(
            "shell",
            r"(?i)\brm\s+-[a-z]*r[a-z]*f[a-z]*\s+/",
            "recursive delete of the filesystem root",
        )?;
        policy.deny(
            "shell",
            r"(?i)\bmkfs(?:\.\w+)?\b",
            "formatting a filesystem",
        )?;
        Ok(policy)
    }

    /// Denies `tool` when its serialized arguments match `arguments`.
    pub fn deny(
        &mut self,
        tool: impl Into<String>,
        arguments: impl AsRef<str>,
        detail: impl Into<String>,
    ) -> Result<&mut Self> {
        self.push(
            PolicyAction::Deny,
            Some(tool.into()),
            Some(arguments),
            detail,
        )
    }

    /// Denies every call to `tool`, whatever the arguments.
    pub fn deny_tool(&mut self, tool: impl Into<String>, detail: impl Into<String>) -> &mut Self {
        self.rules.push(Rule {
            tool: Some(tool.into()),
            arguments: None,
            action: PolicyAction::Deny,
            detail: detail.into(),
        });
        self
    }

    /// Permits `tool` when its arguments match, whatever the deny rules say.
    pub fn allow(
        &mut self,
        tool: impl Into<String>,
        arguments: impl AsRef<str>,
        detail: impl Into<String>,
    ) -> Result<&mut Self> {
        self.push(
            PolicyAction::Allow,
            Some(tool.into()),
            Some(arguments),
            detail,
        )
    }

    fn push(
        &mut self,
        action: PolicyAction,
        tool: Option<String>,
        arguments: Option<impl AsRef<str>>,
        detail: impl Into<String>,
    ) -> Result<&mut Self> {
        let arguments = match arguments {
            Some(source) => Some(Regex::new(source.as_ref()).map_err(|err| {
                HarnessError::Other(format!("tool policy pattern is invalid: {err}"))
            })?),
            None => None,
        };
        self.rules.push(Rule {
            tool,
            arguments,
            action,
            detail: detail.into(),
        });
        Ok(self)
    }
}

#[async_trait]
impl Guardrail for ToolPolicy {
    fn name(&self) -> &str {
        "tool_policy"
    }

    async fn inspect(&self, ctx: &GuardContext) -> Result<GuardVerdict> {
        if ctx.source != GuardSource::ToolCall {
            return Ok(GuardVerdict::Allow);
        }
        let name = ctx.tool_name.as_deref().unwrap_or_default();

        let permitted = self
            .rules
            .iter()
            .any(|rule| rule.action == PolicyAction::Allow && rule.matches(name, &ctx.text));
        if permitted {
            return Ok(GuardVerdict::Allow);
        }

        match self
            .rules
            .iter()
            .find(|rule| rule.action == PolicyAction::Deny && rule.matches(name, &ctx.text))
        {
            Some(rule) => Ok(GuardVerdict::Block {
                detail: format!("tool `{name}`: {}", rule.detail),
            }),
            None => Ok(GuardVerdict::Allow),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn inspect(
        policy: &ToolPolicy,
        name: &str,
        arguments: serde_json::Value,
    ) -> GuardVerdict {
        policy
            .inspect(&GuardContext::tool_call(name, arguments))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_destructive_shell_command_is_refused() {
        let policy = ToolPolicy::destructive_defaults().unwrap();

        let verdict = inspect(&policy, "shell", json!({ "command": "rm -rf /" })).await;
        assert!(matches!(verdict, GuardVerdict::Block { .. }), "{verdict:?}");

        let harmless = inspect(&policy, "shell", json!({ "command": "rm -rf ./build" })).await;
        assert_eq!(harmless, GuardVerdict::Allow);
    }

    #[tokio::test]
    async fn a_policy_only_judges_tool_calls() {
        let policy = ToolPolicy::destructive_defaults().unwrap();
        let verdict = policy
            .inspect(&GuardContext::tool_result("rm -rf /"))
            .await
            .unwrap();
        assert_eq!(verdict, GuardVerdict::Allow);
    }

    #[tokio::test]
    async fn a_deny_rule_is_scoped_to_its_tool() {
        let mut policy = ToolPolicy::new();
        policy.deny_tool("shell", "the shell is switched off");

        assert!(matches!(
            inspect(&policy, "shell", json!({ "command": "ls" })).await,
            GuardVerdict::Block { .. }
        ));
        assert_eq!(
            inspect(&policy, "read_file", json!({ "path": "a.txt" })).await,
            GuardVerdict::Allow
        );
    }

    #[tokio::test]
    async fn an_allow_rule_carves_an_exception_out_of_an_earlier_deny() {
        let mut policy = ToolPolicy::new();
        policy.deny_tool("shell", "no shell by default");
        policy
            .allow("shell", r#""command":"git status""#, "read-only git")
            .unwrap();

        assert_eq!(
            inspect(&policy, "shell", json!({ "command": "git status" })).await,
            GuardVerdict::Allow
        );
        assert!(matches!(
            inspect(&policy, "shell", json!({ "command": "git push" })).await,
            GuardVerdict::Block { .. }
        ));
    }

    #[tokio::test]
    async fn an_invalid_argument_pattern_is_an_error_not_a_panic() {
        let mut policy = ToolPolicy::new();
        let err = policy.deny("shell", "rm -rf (", "unbalanced").unwrap_err();
        assert!(err.to_string().contains("invalid"), "{err}");
    }
}
