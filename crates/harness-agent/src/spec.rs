//! Declarative agent specifications (`*.agent.md`).
//!
//! An agent's capabilities are fixed by a versioned file rather than assembled
//! from runtime prompt fragments: the model, the tool whitelist, the skills and
//! the token ceiling all come from the frontmatter, and the Markdown body is the
//! system prompt. The frontmatter split itself lives in `harness-core`, so the
//! same `---` fenced format serves `.agent.md` and `SKILL.md`.

use std::path::{Path, PathBuf};

use harness_core::{frontmatter, HarnessError, PermissionMode, Result};
use serde::{Deserialize, Serialize};

/// One agent as declared by a file.
///
/// Every field is optional in the document; the defaults that matter (`id`,
/// `name`) are resolved in [`AgentSpec::from_markdown`] rather than by serde,
/// because they depend on the file name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSpec {
    /// Defaults to the file stem with its `.agent.md`/`.md` suffix stripped.
    pub id: String,
    /// Defaults to `id`.
    pub name: String,
    pub description: String,
    /// `"provider/model"`, e.g. `"deepseek/deepseek-chat"`. `None` inherits the
    /// router default.
    pub model: Option<String>,
    pub temperature: Option<f32>,
    /// Reasoning effort this agent asks for, e.g. `"max"`. `None` inherits the
    /// configured default; an unsupported value is ignored rather than sent.
    pub thinking_effort: Option<String>,
    /// Permission tier this agent asks to run under, e.g. `"always_ask"`.
    /// `None` inherits the configured default; an unrecognised value is ignored
    /// rather than carried, and the recognised spelling is what is kept.
    pub permission_mode: Option<String>,
    /// Tool whitelist. Empty means "every registered tool".
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub subagents: Vec<String>,
    /// Hard ceiling on tokens this agent may spend in one run. `None` is
    /// unbounded, as is `0`.
    pub max_tokens: Option<u64>,
    /// Model used when the budget crosses its degradation threshold.
    pub fallback_model: Option<String>,
    /// The Markdown body, used as the system prompt; surrounding blank lines are
    /// trimmed.
    pub system_prompt: String,
    /// Where it was loaded from, for `agent show` and error messages.
    pub source_path: PathBuf,
}

impl AgentSpec {
    /// Reads a document into a validated spec.
    ///
    /// `fallback_id` is the file stem, so a document that omits `id` still gets a
    /// stable, addressable name. Anything malformed — unparseable frontmatter, a
    /// field of the wrong type, an empty body — is reported against `source_path`
    /// instead of being silently defaulted, because a half-read agent is worse
    /// than a refused one.
    pub fn from_markdown(
        markdown: &str,
        source_path: PathBuf,
        fallback_id: &str,
    ) -> Result<AgentSpec> {
        let (mut spec, body) = frontmatter::parse::<Self>(markdown)
            .map_err(|err| invalid(&source_path, &format!("frontmatter is not valid: {err}")))?;

        if body.is_empty() {
            return Err(invalid(
                &source_path,
                "the Markdown body is empty, so the agent has no system prompt",
            ));
        }

        let id = pick(spec.id, fallback_id);
        if id.is_empty() {
            return Err(invalid(
                &source_path,
                "no id: name the file, or set `id` in the frontmatter",
            ));
        }
        let name = pick(spec.name, &id);

        spec.id = id;
        spec.name = name;
        spec.description = spec.description.trim().to_string();
        // Canonicalised here so an unrecognised spelling is dropped rather than
        // carried: a mode the harness does not know must not read as a choice.
        spec.permission_mode = spec
            .permission_mode
            .as_deref()
            .and_then(PermissionMode::parse)
            .map(|mode| mode.as_str().to_string());
        spec.system_prompt = body;
        spec.source_path = source_path;
        Ok(spec)
    }

    /// Splits `model` into `(provider, model)` on the **first** `/`.
    ///
    /// Model names legitimately contain slashes (`moonshot/kimi-k2/turbo`), so
    /// only the first one can be the provider separator. `None` means "use the
    /// default provider" and covers both cases: no model at all, and a bare model
    /// name such as `gpt-4o` — [`crate::router::ModelRouter`] reads the raw value
    /// when it needs to tell those two apart.
    pub fn model_ref(&self) -> Option<(&str, &str)> {
        let (provider, model) = self.model.as_deref()?.split_once('/')?;
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        Some((provider, model))
    }
}

fn invalid(path: &Path, message: &str) -> HarnessError {
    HarnessError::InvalidAgentSpec {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}

fn pick(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::HarnessError;

    fn parse(markdown: &str, fallback_id: &str) -> Result<AgentSpec> {
        AgentSpec::from_markdown(markdown, PathBuf::from("agents/test.agent.md"), fallback_id)
    }

    #[test]
    fn a_full_document_round_trips() {
        let markdown = "\
---
id: backend-architect
name: Backend Architect
description: Designs backend systems.
model: deepseek/deepseek-chat
temperature: 0.2
thinking_effort: max
permission_mode: always_ask
tools:
  - read_file
  - grep
skills:
  - system-design
subagents:
  - code-reviewer
max_tokens: 16384
fallback_model: deepseek/deepseek-chat
---

You are an architect.
Prefer interfaces over implementations.
";
        let spec = parse(markdown, "ignored").unwrap();

        assert_eq!(spec.id, "backend-architect");
        assert_eq!(spec.name, "Backend Architect");
        assert_eq!(spec.description, "Designs backend systems.");
        assert_eq!(spec.model.as_deref(), Some("deepseek/deepseek-chat"));
        assert_eq!(spec.temperature, Some(0.2));
        assert_eq!(spec.thinking_effort.as_deref(), Some("max"));
        assert_eq!(spec.permission_mode.as_deref(), Some("always_ask"));
        assert_eq!(spec.tools, vec!["read_file", "grep"]);
        assert_eq!(spec.skills, vec!["system-design"]);
        assert_eq!(spec.subagents, vec!["code-reviewer"]);
        assert_eq!(spec.max_tokens, Some(16384));
        assert_eq!(
            spec.fallback_model.as_deref(),
            Some("deepseek/deepseek-chat")
        );
        assert_eq!(
            spec.system_prompt,
            "You are an architect.\nPrefer interfaces over implementations."
        );
        assert_eq!(spec.source_path, PathBuf::from("agents/test.agent.md"));

        let json = serde_json::to_value(&spec).unwrap();
        let back: AgentSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn a_minimal_document_derives_id_and_name_from_the_file_name() {
        let spec = parse(
            "---\ndescription: spare\n---\n\nDo the thing.\n",
            "spare-agent",
        )
        .unwrap();
        assert_eq!(spec.id, "spare-agent");
        assert_eq!(spec.name, "spare-agent");
        assert_eq!(spec.description, "spare");
        assert_eq!(spec.tools, Vec::<String>::new());
        assert_eq!(spec.max_tokens, None);
        assert_eq!(spec.system_prompt, "Do the thing.");
    }

    #[test]
    fn a_document_without_frontmatter_is_body_only() {
        let spec = parse("Just a prompt.\n", "plain").unwrap();
        assert_eq!(spec.id, "plain");
        assert_eq!(spec.name, "plain");
        assert_eq!(spec.system_prompt, "Just a prompt.");
    }

    #[test]
    fn an_explicit_name_wins_over_the_file_name() {
        let spec = parse("---\nname:  Spaced Out  \n---\nBody.", "file-stem").unwrap();
        assert_eq!(spec.id, "file-stem");
        assert_eq!(spec.name, "Spaced Out");
    }

    #[test]
    fn crlf_documents_parse() {
        let spec = parse(
            "---\r\nid: win\r\nname: Windows\r\n---\r\n\r\nBody line\r\n",
            "ignored",
        )
        .unwrap();
        assert_eq!(spec.id, "win");
        assert_eq!(spec.name, "Windows");
        assert_eq!(spec.system_prompt, "Body line");
    }

    #[test]
    fn unparseable_frontmatter_names_the_path() {
        let err = parse("---\nname: [unclosed\n---\nBody.", "broken").unwrap_err();
        match err {
            HarnessError::InvalidAgentSpec { path, message } => {
                assert!(path.ends_with("test.agent.md"), "{path:?}");
                assert!(message.contains("frontmatter"), "{message}");
            }
            other => panic!("expected InvalidAgentSpec, got {other:?}"),
        }
    }

    #[test]
    fn a_field_of_the_wrong_type_is_an_error() {
        let err = parse("---\nmax_tokens: many\n---\nBody.", "broken").unwrap_err();
        assert!(
            matches!(err, HarnessError::InvalidAgentSpec { .. }),
            "{err}"
        );

        let err = parse("---\ntools: read_file\n---\nBody.", "broken").unwrap_err();
        assert!(err.to_string().contains("test.agent.md"), "{err}");
    }

    #[test]
    fn an_empty_body_is_an_error() {
        let err = parse("---\nname: hollow\n---\n\n   \n", "hollow").unwrap_err();
        assert!(err.to_string().contains("system prompt"), "{err}");
        assert!(matches!(err, HarnessError::InvalidAgentSpec { .. }));
    }

    #[test]
    fn an_agent_with_no_id_anywhere_is_an_error() {
        let err = parse("Body only.", "   ").unwrap_err();
        assert!(err.to_string().contains("no id"), "{err}");
    }

    #[test]
    fn a_permission_mode_is_canonicalised_and_an_unknown_one_is_dropped() {
        let canonical = parse("---\npermission_mode: Always_Ask\n---\nBody.", "agent").unwrap();
        assert_eq!(canonical.permission_mode.as_deref(), Some("always_ask"));

        // An unrecognised mode is ignored rather than carried, so it cannot read
        // as a deliberate choice downstream.
        let unknown = parse("---\npermission_mode: sometimes\n---\nBody.", "agent").unwrap();
        assert_eq!(unknown.permission_mode, None);
    }

    #[test]
    fn model_ref_splits_on_the_first_slash_only() {
        let mut spec = AgentSpec {
            model: Some("deepseek/deepseek-chat".into()),
            ..Default::default()
        };
        assert_eq!(spec.model_ref(), Some(("deepseek", "deepseek-chat")));

        spec.model = Some("ollama/qwen3:32b/thinking".into());
        assert_eq!(spec.model_ref(), Some(("ollama", "qwen3:32b/thinking")));

        // A bare model name has no provider prefix, so the default provider
        // applies; the router reads `model` itself to recover the name.
        spec.model = Some("gpt-4o-mini".into());
        assert_eq!(spec.model_ref(), None);

        spec.model = Some("deepseek/".into());
        assert_eq!(spec.model_ref(), None);

        spec.model = None;
        assert_eq!(spec.model_ref(), None);
    }
}
