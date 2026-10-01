//! Model selection: id and model name only, never a live provider.
//!
//! Precedence, highest first:
//!   1. runtime override      2. the agent's own `model`
//!   3. an active skill's `model`   4. the system default from config
//!
//! Keeping resolution separate from construction is what makes the choice
//! auditable: `ResolvedModel::source` records which tier won, and no key or
//! socket is touched until the caller builds the provider.

use harness_core::{Config, ProviderConfig, Result};

use crate::spec::AgentSpec;

/// Which tier supplied the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    Override,
    Agent,
    Skill,
    Default,
}

impl ModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Agent => "agent",
            Self::Skill => "skill",
            Self::Default => "default",
        }
    }
}

/// A provider id plus a model name, already checked against the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub provider_id: String,
    pub model: String,
    pub source: ModelSource,
}

#[derive(Debug, Clone)]
pub struct ModelRouter {
    config: Config,
}

impl ModelRouter {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Resolves the model for one run.
    ///
    /// `overrides` is the ordered list of runtime/model hints; the first
    /// non-blank entry wins, and an entry naming an unknown provider is an error
    /// rather than a silent skip — a typo on the command line should be visible.
    pub fn resolve(
        &self,
        spec: Option<&AgentSpec>,
        skill_model: Option<&str>,
        overrides: &[String],
    ) -> Result<ResolvedModel> {
        for hint in overrides {
            if !hint.trim().is_empty() {
                return self.resolve_hint(hint.trim(), ModelSource::Override);
            }
        }

        if let Some(spec) = spec {
            if let Some(model) = spec.model.as_deref().map(str::trim) {
                if !model.is_empty() {
                    return self.resolve_hint(model, ModelSource::Agent);
                }
            }
        }

        if let Some(model) = skill_model.map(str::trim) {
            if !model.is_empty() {
                return self.resolve_hint(model, ModelSource::Skill);
            }
        }

        self.default_model()
    }

    /// The configured ReAct turn ceiling, carried here so a runtime assembled
    /// from this router can build its loop config without a second config handle.
    pub fn max_iterations(&self) -> usize {
        self.config.harness.max_iterations
    }

    fn default_model(&self) -> Result<ResolvedModel> {
        let (provider_id, provider) = self.default_provider()?;
        let model = self.config.model_for(provider, None)?;
        Ok(ResolvedModel {
            provider_id,
            model,
            source: ModelSource::Default,
        })
    }

    /// A `"provider/model"` value selects that provider; a bare model name keeps
    /// the default provider.
    fn resolve_hint(&self, value: &str, source: ModelSource) -> Result<ResolvedModel> {
        match value.split_once('/') {
            Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
                let (provider_id, _) = self.config.provider(Some(provider))?;
                Ok(ResolvedModel {
                    provider_id,
                    model: model.to_string(),
                    source,
                })
            }
            _ => Ok(ResolvedModel {
                provider_id: self.default_provider()?.0,
                model: value.to_string(),
                source,
            }),
        }
    }

    fn default_provider(&self) -> Result<(String, &ProviderConfig)> {
        self.config.provider(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::{ProviderConfig, ProviderKind};

    fn config() -> Config {
        let mut config = Config::default();
        config.harness.max_iterations = 9;
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
        config
    }

    fn spec_with(model: Option<&str>) -> AgentSpec {
        AgentSpec {
            id: "tester".into(),
            model: model.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn an_override_beats_everything() {
        let router = ModelRouter::new(config());
        let spec = spec_with(Some("deepseek/deepseek-chat"));

        let resolved = router
            .resolve(
                Some(&spec),
                Some("moonshot/kimi-k2"),
                &["deepseek/deepseek-reasoner".into()],
            )
            .unwrap();

        assert_eq!(resolved.source, ModelSource::Override);
        assert_eq!(resolved.provider_id, "deepseek");
        assert_eq!(resolved.model, "deepseek-reasoner");
    }

    #[test]
    fn a_blank_override_is_skipped() {
        let router = ModelRouter::new(config());
        let spec = spec_with(Some("deepseek/deepseek-chat"));

        let resolved = router
            .resolve(Some(&spec), None, &["  ".into(), String::new()])
            .unwrap();

        assert_eq!(resolved.source, ModelSource::Agent);
        assert_eq!(resolved.model, "deepseek-chat");
    }

    #[test]
    fn the_agent_model_beats_the_skill_model() {
        let router = ModelRouter::new(config());
        let spec = spec_with(Some("deepseek/deepseek-chat"));

        let resolved = router
            .resolve(Some(&spec), Some("deepseek/deepseek-reasoner"), &[])
            .unwrap();

        assert_eq!(resolved.source, ModelSource::Agent);
        assert_eq!(resolved.model, "deepseek-chat");
    }

    #[test]
    fn a_skill_model_is_used_when_the_agent_names_none() {
        let router = ModelRouter::new(config());
        let spec = spec_with(None);

        let resolved = router
            .resolve(Some(&spec), Some("deepseek/deepseek-reasoner"), &[])
            .unwrap();

        assert_eq!(resolved.source, ModelSource::Skill);
        assert_eq!(resolved.provider_id, "deepseek");
        assert_eq!(resolved.model, "deepseek-reasoner");
    }

    #[test]
    fn the_config_default_wins_when_nothing_else_names_a_model() {
        let router = ModelRouter::new(config());

        let resolved = router.resolve(None, None, &[]).unwrap();
        assert_eq!(resolved.source, ModelSource::Default);
        assert_eq!(resolved.provider_id, "mock");
        assert_eq!(resolved.model, "mock-1");

        let spec = spec_with(Some("   "));
        let resolved = router.resolve(Some(&spec), None, &[]).unwrap();
        assert_eq!(resolved.source, ModelSource::Default);
        assert_eq!(resolved.model, "mock-1");
    }

    #[test]
    fn a_bare_model_name_keeps_the_default_provider() {
        let router = ModelRouter::new(config());

        let resolved = router
            .resolve(Some(&spec_with(Some("gpt-4o"))), None, &[])
            .unwrap();
        assert_eq!(resolved.provider_id, "mock");
        assert_eq!(resolved.model, "gpt-4o");
        assert_eq!(resolved.source, ModelSource::Agent);
    }

    #[test]
    fn a_model_name_may_contain_slashes() {
        let router = ModelRouter::new(config());

        let resolved = router
            .resolve(Some(&spec_with(Some("deepseek/v3/turbo"))), None, &[])
            .unwrap();
        assert_eq!(resolved.provider_id, "deepseek");
        assert_eq!(resolved.model, "v3/turbo");
    }

    #[test]
    fn an_unknown_provider_is_an_error_naming_it() {
        let router = ModelRouter::new(config());

        let err = router
            .resolve(Some(&spec_with(Some("nope/some-model"))), None, &[])
            .unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");

        let err = router
            .resolve(None, None, &["nope/other".into()])
            .unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }

    #[test]
    fn a_config_without_a_default_provider_fails_loudly() {
        let router = ModelRouter::new(Config::default());
        let err = router.resolve(None, None, &[]).unwrap_err();
        assert!(err.to_string().contains("openai"), "{err}");
    }

    #[test]
    fn max_iterations_comes_from_the_harness_section() {
        assert_eq!(ModelRouter::new(config()).max_iterations(), 9);
    }
}
