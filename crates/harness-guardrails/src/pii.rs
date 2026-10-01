//! Personal data that should not be carried through a conversation.

use async_trait::async_trait;
use harness_core::{HarnessError, Result};
use regex::{Captures, Regex};

use crate::{GuardContext, GuardVerdict, Guardrail};

/// Rewrites email addresses, phone numbers and payment-card numbers.
///
/// Card numbers are checked with Luhn before they are flagged: any sixteen
/// digits with separators would otherwise match an order number or a build id.
pub struct PiiDetector {
    email: Regex,
    phone: Regex,
    card: Regex,
}

impl PiiDetector {
    pub fn new() -> Result<Self> {
        Ok(Self {
            email: compile(
                "email",
                r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}",
            )?,
            phone: compile("phone", r"\+?\(?\d[\d\s().-]{5,}\d")?,
            card: compile("card", r"\b\d(?:[ -]?\d){15}\b")?,
        })
    }
}

fn compile(kind: &str, source: &str) -> Result<Regex> {
    Regex::new(source)
        .map_err(|err| HarnessError::Other(format!("guardrail pattern `{kind}` is invalid: {err}")))
}

/// The Luhn checksum every payment-card number satisfies.
fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut double = false;
    for ch in digits.chars().rev() {
        let Some(mut value) = ch.to_digit(10) else {
            return false;
        };
        if double {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
        double = !double;
    }
    sum % 10 == 0
}

#[async_trait]
impl Guardrail for PiiDetector {
    fn name(&self) -> &str {
        "pii_detector"
    }

    async fn inspect(&self, ctx: &GuardContext) -> Result<GuardVerdict> {
        let mut text = ctx.text.clone();
        let mut found: Vec<&'static str> = Vec::new();

        if self.email.is_match(&text) {
            found.push("email");
            text = self
                .email
                .replace_all(&text, "[redacted:email]")
                .into_owned();
        }

        let mut cards = false;
        text = self
            .card
            .replace_all(&text, |caps: &Captures| {
                let matched = &caps[0];
                let digits: String = matched.chars().filter(char::is_ascii_digit).collect();
                if digits.len() == 16 && luhn(&digits) {
                    cards = true;
                    "[redacted:card]".to_string()
                } else {
                    matched.to_string()
                }
            })
            .into_owned();
        if cards {
            found.push("card");
        }

        let mut phones = false;
        text = self
            .phone
            .replace_all(&text, |caps: &Captures| {
                let matched = &caps[0];
                let digits = matched.chars().filter(char::is_ascii_digit).count();
                let separated = matched.starts_with('+')
                    || matched.chars().any(|c| matches!(c, '-' | ' ' | '(' | ')'));
                // Ten digits is the shortest national number; anything longer
                // than fifteen is not one. Requiring a separator or a leading
                // `+` keeps bare years and ids out of the net.
                if separated && (10..=15).contains(&digits) {
                    phones = true;
                    "[redacted:phone]".to_string()
                } else {
                    matched.to_string()
                }
            })
            .into_owned();
        if phones {
            found.push("phone");
        }

        if found.is_empty() {
            return Ok(GuardVerdict::Allow);
        }
        Ok(GuardVerdict::Redact {
            text,
            detail: format!("redacted {}", found.join(", ")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn inspect(text: &str) -> GuardVerdict {
        PiiDetector::new()
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
    async fn an_email_address_is_redacted() {
        let verdict = inspect("write to ada.lovelace@example.co.uk about it").await;
        let text = redacted(&verdict);
        assert!(!text.contains("ada.lovelace@example.co.uk"), "{text}");
        assert!(text.contains("[redacted:email]"), "{text}");
    }

    #[tokio::test]
    async fn a_formatted_phone_number_is_redacted() {
        let verdict = inspect("call (555) 123-4567 tomorrow").await;
        let text = redacted(&verdict);
        assert!(!text.contains("123-4567"), "{text}");
        assert!(text.contains("[redacted:phone]"), "{text}");
    }

    #[tokio::test]
    async fn a_card_number_that_passes_luhn_is_redacted() {
        let verdict = inspect("card 4111 1111 1111 1111 on file").await;
        let text = redacted(&verdict);
        assert!(!text.contains("4111"), "{text}");
        assert!(text.contains("[redacted:card]"), "{text}");
    }

    #[tokio::test]
    async fn a_random_sixteen_digit_number_is_not_flagged() {
        // Same shape as a card, but the checksum fails.
        let verdict = inspect("order 1234 5678 9012 3456 shipped").await;
        assert_eq!(verdict, GuardVerdict::Allow);
    }

    #[tokio::test]
    async fn ordinary_prose_and_short_numbers_are_left_alone() {
        let prose = "Release 1.2.3 shipped on 2026-09-30 with 47 fixes across 12 files.";
        assert_eq!(inspect(prose).await, GuardVerdict::Allow);
    }
}
