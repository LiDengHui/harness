//! Credentials that must not reach the model or the log verbatim.

use async_trait::async_trait;
use harness_core::{HarnessError, Result};
use regex::Regex;

use crate::{GuardContext, GuardVerdict, Guardrail};

/// A named shape worth redacting.
struct Pattern {
    kind: &'static str,
    regex: Regex,
}

/// Rewrites anything that looks like a credential into a placeholder.
///
/// The patterns are deliberately narrow. A false positive silently rewrites
/// text the user asked about, so a bare `sk-` prefix or a short `AKIA`
/// fragment is left alone until it reaches the length of a real key.
pub struct SecretScanner {
    patterns: Vec<Pattern>,
}

impl SecretScanner {
    pub fn new() -> Result<Self> {
        const SOURCES: &[(&str, &str)] = &[
            (
                "private_key",
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            ),
            ("aws_key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            (
                "jwt",
                r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
            ),
            ("bearer_token", r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}"),
            ("api_key", r"\bsk-[A-Za-z0-9_-]{16,}\b"),
        ];

        let mut patterns = Vec::with_capacity(SOURCES.len());
        for (kind, source) in SOURCES {
            let regex = Regex::new(source).map_err(|err| {
                HarnessError::Other(format!("guardrail pattern `{kind}` is invalid: {err}"))
            })?;
            patterns.push(Pattern { kind, regex });
        }
        Ok(Self { patterns })
    }
}

#[async_trait]
impl Guardrail for SecretScanner {
    fn name(&self) -> &str {
        "secret_scanner"
    }

    async fn inspect(&self, ctx: &GuardContext) -> Result<GuardVerdict> {
        let mut text = ctx.text.clone();
        let mut found: Vec<&'static str> = Vec::new();

        for pattern in &self.patterns {
            if !pattern.regex.is_match(&text) {
                continue;
            }
            found.push(pattern.kind);
            let placeholder = format!("[redacted:{}]", pattern.kind);
            text = pattern
                .regex
                .replace_all(&text, placeholder.as_str())
                .into_owned();
        }

        if found.is_empty() {
            return Ok(GuardVerdict::Allow);
        }
        Ok(GuardVerdict::Redact {
            text,
            detail: format!("redacted {}: {}", found.len(), found.join(", ")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
    const API_KEY: &str = "sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";

    async fn inspect(text: &str) -> GuardVerdict {
        SecretScanner::new()
            .unwrap()
            .inspect(&GuardContext::tool_result(text))
            .await
            .unwrap()
    }

    fn redacted(verdict: &GuardVerdict) -> &str {
        match verdict {
            GuardVerdict::Redact { text, .. } => text,
            other => panic!("expected a redaction, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_aws_access_key_is_redacted() {
        let verdict = inspect(&format!("aws_access_key_id = {AWS_KEY}")).await;
        let text = redacted(&verdict);
        assert!(!text.contains(AWS_KEY), "{text}");
        assert!(text.contains("[redacted:aws_key]"), "{text}");
    }

    #[tokio::test]
    async fn an_api_key_is_redacted() {
        let verdict = inspect(&format!("OPENAI_API_KEY={API_KEY}")).await;
        let text = redacted(&verdict);
        assert!(!text.contains(API_KEY), "{text}");
        assert!(text.contains("[redacted:api_key]"), "{text}");
    }

    #[tokio::test]
    async fn a_bearer_token_is_redacted() {
        let token = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let verdict = inspect(&format!("Authorization: Bearer {token}")).await;
        let text = redacted(&verdict);
        assert!(!text.contains(token), "{text}");
        assert!(text.contains("[redacted:bearer_token]"), "{text}");
    }

    #[tokio::test]
    async fn a_pem_private_key_block_is_redacted_whole() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA7\nZm9vYmFy\n-----END RSA PRIVATE KEY-----";
        let verdict = inspect(&format!("here it is:\n{pem}\nkeep it secret")).await;
        let text = redacted(&verdict);
        assert!(!text.contains("MIIEowIBAAKCAQEA7"), "{text}");
        assert!(text.contains("[redacted:private_key]"), "{text}");
        assert!(text.contains("keep it secret"), "{text}");
    }

    #[tokio::test]
    async fn a_jwt_is_redacted() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let verdict = inspect(&format!("token={jwt}")).await;
        let text = redacted(&verdict);
        assert!(!text.contains(jwt), "{text}");
        assert!(text.contains("[redacted:jwt]"), "{text}");
    }

    #[tokio::test]
    async fn ordinary_prose_is_left_alone() {
        let prose = "The quick brown fox jumps over the lazy dog; skim the file, then summarise it in three bullet points.";
        assert_eq!(inspect(prose).await, GuardVerdict::Allow);

        // A short `sk-` fragment is a word, not a key.
        assert_eq!(inspect("the task-sk- is a typo").await, GuardVerdict::Allow);
    }
}
