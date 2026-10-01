//! Assembly of a declarative spec into something the loop can run.
//!
//! This is the capability-hardening step: the model, the tool whitelist and the
//! token ceiling all come from the file, so what an agent *may* do is reviewable
//! in the same document that tells it what to do.

use std::sync::Arc;

use harness_core::{AgentId, HarnessError, Memory, Result, SessionId};
use harness_llm::Provider;
use harness_tools::ToolRegistry;

use crate::agent_loop::AgentConfig;
use crate::budget::TokenBudget;
use crate::router::{ModelRouter, ResolvedModel};
use crate::spec::AgentSpec;

pub struct AgentRuntime {
    pub spec: AgentSpec,
    pub provider: Arc<dyn Provider>,
    pub model: ResolvedModel,
    pub tools: ToolRegistry,
    pub memory: Arc<dyn Memory>,
    pub budget: Arc<TokenBudget>,
    // Both are derived from inputs already present, but resolving them at build
    // time is what lets `agent_config` be infallible.
    agent_id: AgentId,
    max_iterations: usize,
}

impl AgentRuntime {
    /// Assembles an agent from its declarative spec.
    ///
    /// The sub-agent wiring (`spec.subagents`) is still carried but not yet
    /// consumed; `spec.skills` is carried onto the config, where the loop
    /// resolves it into the prompt's skill index.
    pub fn build(
        spec: &AgentSpec,
        router: &ModelRouter,
        provider: Arc<dyn Provider>,
        all_tools: &ToolRegistry,
        memory: Arc<dyn Memory>,
    ) -> Result<Self> {
        Self::build_with_overrides(spec, router, provider, all_tools, memory, &[])
    }

    /// [`Self::build`] with runtime model overrides, which outrank the `model:`
    /// line in the agent file. The CLI passes `--provider`/`--model` through here.
    pub fn build_with_overrides(
        spec: &AgentSpec,
        router: &ModelRouter,
        provider: Arc<dyn Provider>,
        all_tools: &ToolRegistry,
        memory: Arc<dyn Memory>,
        overrides: &[String],
    ) -> Result<Self> {
        let model = router.resolve(Some(spec), None, overrides)?;
        let tools = select_tools(spec, all_tools)?;
        let budget = Arc::new(TokenBudget::new(spec.max_tokens.unwrap_or(0)));
        let agent_id =
            AgentId::new(spec.id.clone()).map_err(|err| HarnessError::InvalidAgentSpec {
                path: spec.source_path.clone(),
                message: err.to_string(),
            })?;

        Ok(Self {
            spec: spec.clone(),
            provider,
            model,
            tools,
            memory,
            budget,
            agent_id,
            max_iterations: router.max_iterations(),
        })
    }

    /// Feeds the whitelist and budget into the loop's config.
    pub fn agent_config(&self, session_id: SessionId) -> AgentConfig {
        let mut config = AgentConfig::new(
            self.agent_id.clone(),
            session_id,
            self.model.model.clone(),
            self.spec.system_prompt.clone(),
        );
        config.temperature = self.spec.temperature;
        // Validated here so an unrecognised value in an agent file is dropped
        // rather than sent: the gateway answers one with an opaque upstream
        // failure, and the configured default is the safer answer.
        config.reasoning_effort = self
            .spec
            .thinking_effort
            .as_deref()
            .and_then(harness_core::resolve_thinking_effort)
            .map(str::to_string);
        config.max_iterations = self.max_iterations;
        // Names only: the loop loads the registry and resolves them into the
        // prompt's index at construction, so nothing is read here.
        config.skills = self.spec.skills.clone();
        // `fallback_model` uses the same `provider/model` spelling as `model`,
        // but the loop can only swap the model on the connection it already
        // holds. A fallback naming a different provider is dropped rather than
        // sent to a gateway that would reject it.
        config.fallback_model =
            self.spec
                .fallback_model
                .as_deref()
                .and_then(|raw| match raw.split_once('/') {
                    Some((provider, model)) if provider == self.model.provider_id => {
                        Some(model.to_string())
                    }
                    Some(_) => None,
                    None => Some(raw.to_string()),
                });
        config.budget = Some(Arc::clone(&self.budget));
        config
    }
}

/// Applies the whitelist, refusing names that match no registered tool.
///
/// `ToolRegistry::filter` deliberately ignores unknown names, so the check
/// happens here: a typo in a whitelist would otherwise remove a capability the
/// author believed they had granted.
fn select_tools(spec: &AgentSpec, all_tools: &ToolRegistry) -> Result<ToolRegistry> {
    if spec.tools.is_empty() {
        return Ok(all_tools.clone());
    }
    for name in &spec.tools {
        if all_tools.get(name).is_none() {
            return Err(HarnessError::ToolNotFound(name.clone()));
        }
    }
    Ok(all_tools.filter(&spec.tools))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use harness_core::{
        Config, MemoryStats, Message, Node, NodeId, ProviderConfig, ProviderKind, RecallHit,
        RecallQuery, SessionInfo, TrimOptions, TrimReport,
    };
    use harness_llm::{MockProvider, ScriptedTurn};

    fn unused() -> HarnessError {
        HarnessError::Memory("the test runtime never touches memory".into())
    }

    struct UnusedMemory;

    #[async_trait]
    impl Memory for UnusedMemory {
        async fn create_session(&self, _label: Option<&str>) -> Result<SessionId> {
            Err(unused())
        }
        async fn sessions(&self) -> Result<Vec<SessionInfo>> {
            Err(unused())
        }
        async fn session(&self, _session_id: SessionId) -> Result<Option<SessionInfo>> {
            Err(unused())
        }
        async fn head(&self, _session_id: SessionId) -> Result<Option<NodeId>> {
            Err(unused())
        }
        async fn append(&self, _session_id: SessionId, _message: &Message) -> Result<NodeId> {
            Err(unused())
        }
        async fn snapshot(&self, _session_id: SessionId, _label: &str) -> Result<NodeId> {
            Err(unused())
        }
        async fn branch(
            &self,
            _from_session: SessionId,
            _from_node: NodeId,
            _label: Option<&str>,
        ) -> Result<SessionId> {
            Err(unused())
        }
        async fn history(
            &self,
            _session_id: SessionId,
            _head: Option<NodeId>,
        ) -> Result<Vec<Message>> {
            Err(unused())
        }
        async fn path(&self, _node_id: NodeId) -> Result<Vec<Node>> {
            Err(unused())
        }
        async fn node(&self, _node_id: NodeId) -> Result<Option<Node>> {
            Err(unused())
        }
        async fn children(&self, _parent: NodeId) -> Result<Vec<Node>> {
            Err(unused())
        }
        async fn trim(&self, _session_id: SessionId, _o: TrimOptions) -> Result<TrimReport> {
            Err(unused())
        }
        async fn recall(&self, _query: RecallQuery) -> Result<Vec<RecallHit>> {
            Err(unused())
        }
        async fn raw_output(&self, _node_id: NodeId) -> Result<Option<String>> {
            Err(unused())
        }
        async fn stats(&self) -> Result<MemoryStats> {
            Err(unused())
        }
    }

    fn router() -> ModelRouter {
        let mut config = Config::default();
        config.harness.max_iterations = 7;
        config.provider.default = "deepseek".into();
        config.providers.insert(
            "deepseek".into(),
            ProviderConfig {
                kind: ProviderKind::OpenAi,
                default_model: Some("deepseek-chat".into()),
                ..Default::default()
            },
        );
        ModelRouter::new(config)
    }

    fn provider() -> Arc<dyn Provider> {
        Arc::new(MockProvider::new(
            "deepseek",
            "deepseek-chat",
            vec![ScriptedTurn::Text("done".into())],
        ))
    }

    fn build(spec: &AgentSpec, all_tools: &ToolRegistry) -> Result<AgentRuntime> {
        AgentRuntime::build(
            spec,
            &router(),
            provider(),
            all_tools,
            Arc::new(UnusedMemory),
        )
    }

    #[test]
    fn an_empty_whitelist_keeps_every_registered_tool() {
        let spec = AgentSpec {
            id: "tester".into(),
            system_prompt: "Body.".into(),
            ..Default::default()
        };
        let all = ToolRegistry::with_builtins();

        let runtime = build(&spec, &all).unwrap();
        assert_eq!(runtime.tools.len(), all.len());
        assert_eq!(runtime.tools.names(), all.names());
    }

    #[test]
    fn a_whitelist_is_applied() {
        let spec = AgentSpec {
            id: "tester".into(),
            tools: vec!["read_file".into(), "grep".into()],
            system_prompt: "Body.".into(),
            ..Default::default()
        };

        let runtime = build(&spec, &ToolRegistry::with_builtins()).unwrap();
        assert_eq!(runtime.tools.names(), vec!["grep", "read_file"]);
        assert!(runtime.tools.get("shell").is_none());
    }

    #[test]
    fn an_unknown_tool_in_the_whitelist_errors_and_names_it() {
        let spec = AgentSpec {
            id: "tester".into(),
            tools: vec!["read_file".into(), "reed_file".into()],
            system_prompt: "Body.".into(),
            ..Default::default()
        };

        let Err(err) = build(&spec, &ToolRegistry::with_builtins()) else {
            panic!("a whitelist naming an unregistered tool must fail the build");
        };
        assert!(err.to_string().contains("reed_file"), "{err}");
    }

    #[test]
    fn agent_config_carries_the_spec() {
        let spec = AgentSpec {
            id: "tester".into(),
            model: Some("deepseek/deepseek-reasoner".into()),
            temperature: Some(0.2),
            thinking_effort: Some("MAX".into()),
            max_tokens: Some(16_384),
            system_prompt: "You are terse.".into(),
            ..Default::default()
        };

        let runtime = build(&spec, &ToolRegistry::with_builtins()).unwrap();
        let config = runtime.agent_config(SessionId::new());

        assert_eq!(config.agent_id.as_str(), "tester");
        assert_eq!(config.model, "deepseek-reasoner");
        assert_eq!(config.system_prompt, "You are terse.");
        assert_eq!(config.temperature, Some(0.2));
        // Canonicalised to the supported spelling, so the loop carries a value
        // the gateway accepts.
        assert_eq!(config.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(config.max_iterations, 7);
        assert_eq!(runtime.model.source, crate::router::ModelSource::Agent);
    }

    #[test]
    fn an_unsupported_spec_effort_is_dropped_rather_than_carried() {
        let spec = AgentSpec {
            id: "tester".into(),
            thinking_effort: Some("bogus".into()),
            system_prompt: "Body.".into(),
            ..Default::default()
        };

        let runtime = build(&spec, &ToolRegistry::with_builtins()).unwrap();
        assert_eq!(
            runtime.agent_config(SessionId::new()).reasoning_effort,
            None
        );
    }

    #[test]
    fn the_budget_comes_from_the_spec_and_is_unbounded_without_one() {
        let bounded = AgentSpec {
            id: "tester".into(),
            max_tokens: Some(1_000),
            system_prompt: "Body.".into(),
            ..Default::default()
        };
        let runtime = build(&bounded, &ToolRegistry::with_builtins()).unwrap();
        assert_eq!(runtime.budget.remaining(), 1_000);
        assert_eq!(runtime.budget.state(), crate::budget::BudgetState::Normal);

        let unbounded = AgentSpec {
            id: "tester".into(),
            system_prompt: "Body.".into(),
            ..Default::default()
        };
        let runtime = build(&unbounded, &ToolRegistry::with_builtins()).unwrap();
        assert_eq!(runtime.budget.remaining(), u64::MAX);
    }

    #[test]
    fn an_unknown_provider_in_the_spec_fails_the_build() {
        let spec = AgentSpec {
            id: "tester".into(),
            model: Some("nope/some-model".into()),
            system_prompt: "Body.".into(),
            ..Default::default()
        };

        let Err(err) = build(&spec, &ToolRegistry::with_builtins()) else {
            panic!("an unknown provider must fail the build");
        };
        assert!(err.to_string().contains("nope"), "{err}");
    }
}
