//! Turning configuration into a live provider.
//!
//! Both front ends need this: `harness run` for a single prompt and the server
//! for every session. Keeping one copy is what stops the two from disagreeing
//! about which configurations are legal — the keyless-local-endpoint rule in
//! particular is a policy decision, not a construction detail.

use std::sync::Arc;
use std::time::Duration;

use harness_core::{HarnessError, ProviderConfig, ProviderKind, Result};
use serde_json::json;

use crate::mock::{MockProvider, ScriptedTurn};
use crate::openai::OpenAiProvider;
use crate::provider::Provider;

/// Builds a provider from configuration. `Mock` needs no key; `OpenAi` refuses a
/// remote endpoint with no key rather than sending a request that 401s.
///
/// A keyless endpoint that resolves to the loopback interface is left alone: a
/// local server — Ollama, llama.cpp, vLLM — is a deliberate setup, and demanding
/// a key for it would make the offline path unusable.
pub fn from_config(
    id: &str,
    config: &ProviderConfig,
    model: &str,
    timeout: Duration,
    max_retries: u32,
) -> Result<Arc<dyn Provider>> {
    match config.kind {
        ProviderKind::Mock => Ok(Arc::new(MockProvider::new(id, model, mock_script()))),
        ProviderKind::OpenAi => {
            let api_key = config.api_key()?;
            if api_key.is_none() && !is_local_endpoint(config.base_url()) {
                return Err(HarnessError::Other(format!(
                    "provider `{id}` needs a key: export {} before running",
                    config
                        .api_key_env
                        .as_deref()
                        .unwrap_or("the configured API key variable")
                )));
            }

            let provider = OpenAiProvider::new(id, config, model, api_key, timeout, max_retries)?;
            Ok(Arc::new(provider))
        }
    }
}

/// A keyless endpoint is a local server, not a misconfiguration.
///
/// The host is taken from the authority, with bracketed IPv6 kept intact: a
/// naive split on `:` would turn `[::1]:8080` into `[`, which is how the
/// bracketed form went unrecognised before.
fn is_local_endpoint(base_url: &str) -> bool {
    let rest = base_url.split("://").nth(1).unwrap_or(base_url);
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = match authority.rfind(']') {
        Some(end) => &authority[..=end],
        None => authority.split(':').next().unwrap_or(authority),
    };
    matches!(
        host,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]"
    )
}

/// Exercises the full loop offline: one tool round-trip, then a final answer.
///
/// The tool call is what makes the mock useful as an acceptance fixture: a
/// script that only returned text would never touch the tool layer, the event
/// stream or the memory write.
fn mock_script() -> Vec<ScriptedTurn> {
    vec![
        ScriptedTurn::ToolCall {
            name: "read_file".into(),
            arguments: json!({ "path": "Cargo.toml" }),
        },
        ScriptedTurn::Text(
            "(mock provider) the tool round-trip completed. Set [provider] default to \
             `deepseek`, `openai`, `moonshot` or `ollama` for real answers."
                .into(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_config(kind: ProviderKind, base_url: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            kind,
            base_url: base_url.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn the_mock_provider_needs_no_key() {
        let provider = from_config(
            "mock",
            &provider_config(ProviderKind::Mock, None),
            "mock-1",
            Duration::from_secs(1),
            0,
        )
        .unwrap();

        assert_eq!(provider.id(), "mock");
        assert_eq!(provider.model(), "mock-1");
    }

    #[test]
    fn a_remote_openai_endpoint_without_a_key_is_refused() {
        let config = ProviderConfig {
            api_key_env: Some("HARNESS_TEST_PROVIDER_KEY_UNSET".into()),
            ..provider_config(ProviderKind::OpenAi, Some("https://api.example.com/v1"))
        };

        let err = from_config("remote", &config, "gpt-4o-mini", Duration::from_secs(1), 0)
            .err()
            .expect("a remote endpoint without a key must be refused");

        assert!(err.to_string().contains("needs a key"), "{err}");
        assert!(
            err.to_string().contains("HARNESS_TEST_PROVIDER_KEY_UNSET"),
            "the message must name the variable to export: {err}"
        );
    }

    #[test]
    fn a_keyless_local_endpoint_is_accepted() {
        let config = ProviderConfig {
            api_key_env: Some("HARNESS_TEST_PROVIDER_KEY_UNSET".into()),
            ..provider_config(ProviderKind::OpenAi, Some("http://localhost:11434/v1"))
        };

        let provider =
            from_config("ollama", &config, "qwen3-coder", Duration::from_secs(1), 0).unwrap();
        assert_eq!(provider.id(), "ollama");
    }

    #[test]
    fn local_host_spellings_are_recognised() {
        assert!(is_local_endpoint("http://localhost:11434/v1"));
        assert!(is_local_endpoint("http://127.0.0.1:8080/v1"));
        assert!(is_local_endpoint("http://[::1]:8080/v1"));
        assert!(is_local_endpoint("http://[::1]/v1"));
        assert!(is_local_endpoint("http://0.0.0.0:1234/v1"));
        assert!(is_local_endpoint("localhost:8080"));
        assert!(!is_local_endpoint("https://api.openai.com/v1"));
        assert!(!is_local_endpoint("http://localhost.example.com/v1"));
        assert!(!is_local_endpoint("https://api.deepseek.com/v1"));
    }

    #[test]
    fn the_mock_script_exercises_a_tool_round_trip() {
        let script = mock_script();
        assert_eq!(script.len(), 2);
        assert!(matches!(
            script.first(),
            Some(ScriptedTurn::ToolCall { name, .. }) if name == "read_file"
        ));
    }
}
