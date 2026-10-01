//! Token accounting.
//!
//! Real token counts come from the provider's usage report. Estimators exist for
//! the paths that must decide *before* a request is sent: context trimming,
//! retrieval budgets and the hard token budget gate.

use serde::{Deserialize, Serialize};

/// Rough per-message framing cost (role, delimiters) charged on top of the body.
pub const DEFAULT_MESSAGE_OVERHEAD: usize = 4;

/// Estimates the token cost of text without calling a tokenizer service.
pub trait TokenEstimator: Send + Sync + std::fmt::Debug {
    fn estimate(&self, text: &str) -> usize;

    /// Estimates a full message list, charging framing overhead per message.
    fn estimate_messages(&self, messages: &[&str]) -> usize {
        let body: usize = messages.iter().map(|m| self.estimate(m)).sum();
        body + messages.len() * DEFAULT_MESSAGE_OVERHEAD
    }
}

/// Character-class heuristic estimator.
///
/// Latin text costs roughly one token per four characters; CJK and other wide
/// scripts cost roughly one token per character. The weights are deliberately
/// conservative (they over-estimate) so budget gates fail safe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeuristicEstimator {
    pub latin_chars_per_token: f32,
    pub message_overhead: usize,
}

impl Default for HeuristicEstimator {
    fn default() -> Self {
        Self {
            latin_chars_per_token: 4.0,
            message_overhead: DEFAULT_MESSAGE_OVERHEAD,
        }
    }
}

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F     // Hangul Jamo
        | 0x2E80..=0x303E   // CJK radicals, Kangxi, CJK symbols
        | 0x3041..=0x33FF   // Hiragana, Katakana, CJK compatibility
        | 0x3400..=0x4DBF   // CJK extension A
        | 0x4E00..=0x9FFF   // CJK unified ideographs
        | 0xA000..=0xA4CF   // Yi
        | 0xAC00..=0xD7A3   // Hangul syllables
        | 0xF900..=0xFAFF   // CJK compatibility ideographs
        | 0xFE30..=0xFE6F   // CJK compatibility forms
        | 0xFF00..=0xFF60   // Fullwidth forms
        | 0x20000..=0x3FFFD // CJK extensions B and beyond
    )
}

impl TokenEstimator for HeuristicEstimator {
    fn estimate(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }

        let mut latin = 0usize;
        let mut wide = 0usize;
        for ch in text.chars() {
            if is_wide(ch) {
                wide += 1;
            } else {
                latin += 1;
            }
        }

        let latin_tokens = latin as f32 / self.latin_chars_per_token.max(1.0);
        (latin_tokens + wide as f32).ceil() as usize
    }

    fn estimate_messages(&self, messages: &[&str]) -> usize {
        let body: usize = messages.iter().map(|m| self.estimate(m)).sum();
        body + messages.len() * self.message_overhead
    }
}

/// Estimated and reported token counts for one model interaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl TokenUsage {
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }

    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_text_costs_about_a_quarter_per_char() {
        let est = HeuristicEstimator::default();
        assert_eq!(est.estimate(""), 0);
        assert_eq!(est.estimate("hello world"), 3);
    }

    #[test]
    fn cjk_costs_about_one_token_per_char() {
        let est = HeuristicEstimator::default();
        assert_eq!(est.estimate("你好世界"), 4);
        assert_eq!(est.estimate("设计一个后端架构"), 8);
    }

    #[test]
    fn message_overhead_is_charged_per_message() {
        let est = HeuristicEstimator::default();
        let one = est.estimate_messages(&["hello world"]);
        let two = est.estimate_messages(&["hello world", "hello world"]);
        assert_eq!(two - one, 3 + DEFAULT_MESSAGE_OVERHEAD);
    }
}
