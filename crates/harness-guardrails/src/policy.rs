//! Allow/deny rules over the tool calls a model asks for.

use async_trait::async_trait;
use harness_core::{classify, mode_requires_approval, HarnessError, PermissionMode, Result};
use regex::Regex;

use crate::{GuardContext, GuardSource, GuardVerdict, Guardrail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    Allow,
    /// Refuse outright; the permission tier cannot reopen it.
    Deny,
    /// Let the call proceed only if a human approves it.
    Ask,
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

/// Refuses or gates tool calls that match a rule, and gates the rest by risk.
///
/// `Allow` rules are consulted first, so an exception always wins over a
/// broader `Ask` or `Deny` no matter which was declared first. `Ask` rules come
/// next, ahead of `Deny`, so a policy can name a call that is approvable where
/// the rest of its tool is not. A call that matches no rule is still judged by
/// the run's permission tier, and only a tier that asks nothing lets it through
/// untouched — which keeps a policy written as a denylist a denylist while
/// still making "always ask" mean what it says.
#[derive(Debug, Default)]
pub struct ToolPolicy {
    rules: Vec<Rule>,
    /// The tier used when the inspected call carries none of its own.
    mode: PermissionMode,
}

impl ToolPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the tier the policy judges by when a call does not name one.
    ///
    /// A per-call override on the [`GuardContext`] outranks it, which is how a
    /// per-message mode reaches a policy that was built for the whole session.
    pub fn with_mode(mut self, mode: PermissionMode) -> Self {
        self.mode = mode;
        self
    }

    /// The rules a run gets by default: recursive deletes aimed at the
    /// filesystem root, which no agent task legitimately needs.
    ///
    /// Both are `Deny` and stay `Deny`: this is a hard block, not a question, so
    /// the permission tier cannot turn it into an approval.
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

    /// Asks for `tool` when its serialized arguments match `arguments`.
    pub fn ask(
        &mut self,
        tool: impl Into<String>,
        arguments: impl AsRef<str>,
        detail: impl Into<String>,
    ) -> Result<&mut Self> {
        self.push(
            PolicyAction::Ask,
            Some(tool.into()),
            Some(arguments),
            detail,
        )
    }

    /// Asks for every call to `tool`, whatever the arguments.
    pub fn ask_tool(&mut self, tool: impl Into<String>, detail: impl Into<String>) -> &mut Self {
        self.rules.push(Rule {
            tool: Some(tool.into()),
            arguments: None,
            action: PolicyAction::Ask,
            detail: detail.into(),
        });
        self
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

    /// Permits `tool` when its arguments match, whatever the ask or deny rules
    /// say.
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

        // An explicit exception outranks every rule and the tier alike.
        if self
            .rules
            .iter()
            .any(|rule| rule.action == PolicyAction::Allow && rule.matches(name, &ctx.text))
        {
            return Ok(GuardVerdict::Allow);
        }
        // An explicit ask outranks a deny, so a policy can make one call to a
        // tool approvable without opening the rest of it.
        if let Some(rule) = self
            .rules
            .iter()
            .find(|rule| rule.action == PolicyAction::Ask && rule.matches(name, &ctx.text))
        {
            return Ok(GuardVerdict::Ask {
                detail: format!("tool `{name}`: {}", rule.detail),
            });
        }
        // A deny is final: the tier below must not turn it into a question.
        if let Some(rule) = self
            .rules
            .iter()
            .find(|rule| rule.action == PolicyAction::Deny && rule.matches(name, &ctx.text))
        {
            return Ok(GuardVerdict::Block {
                detail: format!("tool `{name}`: {}", rule.detail),
            });
        }

        let mode = ctx.permission_mode.unwrap_or(self.mode);
        if mode_requires_approval(mode, classify(name)) {
            return Ok(GuardVerdict::Ask {
                detail: format!("tool `{name}` needs approval under `{}`", mode.as_str()),
            });
        }
        Ok(GuardVerdict::Allow)
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

    async fn inspect_under(
        policy: &ToolPolicy,
        mode: PermissionMode,
        name: &str,
        arguments: serde_json::Value,
    ) -> GuardVerdict {
        policy
            .inspect(&GuardContext::tool_call(name, arguments).with_permission_mode(mode))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_tier_gates_calls_no_rule_mentions() {
        let policy = ToolPolicy::new().with_mode(PermissionMode::AskWhenNeeded);

        assert_eq!(
            inspect(&policy, "read_file", json!({ "path": "a.txt" })).await,
            GuardVerdict::Allow,
            "a read changes nothing, so asking about it is noise"
        );
        assert!(matches!(
            inspect(&policy, "write_file", json!({ "path": "a.txt" })).await,
            GuardVerdict::Ask { .. }
        ));
        assert!(matches!(
            inspect(&policy, "shell", json!({ "command": "ls" })).await,
            GuardVerdict::Ask { .. }
        ));
        // An unknown tool is treated as consequential rather than waved through.
        assert!(matches!(
            inspect(&policy, "mystery", json!({})).await,
            GuardVerdict::Ask { .. }
        ));

        let always = ToolPolicy::new().with_mode(PermissionMode::AlwaysAsk);
        assert!(matches!(
            inspect(&always, "read_file", json!({ "path": "a.txt" })).await,
            GuardVerdict::Ask { .. }
        ));

        let auto = ToolPolicy::new().with_mode(PermissionMode::FullAuto);
        assert_eq!(
            inspect(&auto, "shell", json!({ "command": "ls" })).await,
            GuardVerdict::Allow
        );
    }

    #[tokio::test]
    async fn a_per_call_tier_outranks_the_one_the_policy_was_built_with() {
        let policy = ToolPolicy::new().with_mode(PermissionMode::FullAuto);

        // The session tier says nothing is asked, but this run's override does.
        assert!(matches!(
            inspect_under(
                &policy,
                PermissionMode::AlwaysAsk,
                "read_file",
                json!({ "path": "a.txt" })
            )
            .await,
            GuardVerdict::Ask { .. }
        ));
        // And the other way round: the override can also stop the asking.
        let asking = ToolPolicy::new().with_mode(PermissionMode::AskWhenNeeded);
        assert_eq!(
            inspect_under(&asking, PermissionMode::FullAuto, "shell", json!({})).await,
            GuardVerdict::Allow
        );
    }

    #[tokio::test]
    async fn an_ask_rule_outranks_a_deny_and_an_allow_outranks_both() {
        let mut policy = ToolPolicy::new().with_mode(PermissionMode::FullAuto);
        policy.deny_tool("shell", "no shell by default");
        policy.ask_tool("shell", "shell needs a human");

        // Ask beats Deny.
        assert!(matches!(
            inspect(&policy, "shell", json!({ "command": "ls" })).await,
            GuardVerdict::Ask { .. }
        ));

        // Allow beats both.
        policy
            .allow("shell", r#""command":"git status""#, "read-only git")
            .unwrap();
        assert_eq!(
            inspect(&policy, "shell", json!({ "command": "git status" })).await,
            GuardVerdict::Allow
        );
    }

    #[tokio::test]
    async fn the_tier_never_reopens_a_deny() {
        let mut policy = ToolPolicy::new().with_mode(PermissionMode::FullAuto);
        policy
            .deny("shell", r"rm\s+-rf\s+/", "destructive command")
            .unwrap();

        for mode in [
            PermissionMode::AlwaysAsk,
            PermissionMode::AskWhenNeeded,
            PermissionMode::FullAuto,
        ] {
            assert!(
                matches!(
                    inspect_under(&policy, mode, "shell", json!({ "command": "rm -rf /" })).await,
                    GuardVerdict::Block { .. }
                ),
                "a deny must stay a deny under {mode:?}"
            );
        }
    }
}
