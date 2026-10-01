//! The hard token ceiling of one agent run.
//!
//! The budget is shared as an `Arc` and mutated from wherever usage is observed,
//! so it is atomics rather than a lock. Two signals come out of it: a one-shot
//! *degradation* hint (switch to the cheap model before the ceiling arrives) and
//! a hard *exhaustion* state.
//!
//! A `max_tokens` of `0` means **unbounded**: an agent with no declared ceiling
//! must still run, so zero is read as "no ceiling" rather than "no budget".

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use harness_core::TokenUsage;

use crate::spec::AgentSpec;

/// Fraction of the ceiling at which degradation kicks in.
const DEFAULT_DEGRADE_AT: f64 = 0.8;

/// Latch states. Entering `PENDING` records that the threshold was crossed;
/// moving to `TAKEN` records that the caller was told, so the hint fires exactly
/// once and is never re-armed.
const LATCH_IDLE: u8 = 0;
const LATCH_PENDING: u8 = 1;
const LATCH_TAKEN: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetState {
    Normal,
    Warning,
    Exhausted,
}

#[derive(Debug)]
pub struct TokenBudget {
    /// `0` means unbounded.
    max_tokens: u64,
    degrade_at: f64,
    spent: AtomicU64,
    degrade: AtomicU8,
}

impl TokenBudget {
    pub fn new(max_tokens: u64) -> Self {
        Self {
            max_tokens,
            degrade_at: DEFAULT_DEGRADE_AT,
            spent: AtomicU64::new(0),
            degrade: AtomicU8::new(LATCH_IDLE),
        }
    }

    /// Fraction of the budget at which degradation kicks in. Values outside
    /// `(0, 1]` fall back to the default rather than disabling the signal.
    pub fn with_degrade_at(mut self, fraction: f64) -> Self {
        if fraction.is_finite() && fraction > 0.0 {
            self.degrade_at = fraction.min(1.0);
        }
        self
    }

    pub fn record(&self, usage: TokenUsage) {
        let total = self.spent.fetch_add(usage.total(), Ordering::SeqCst) + usage.total();
        if self.max_tokens > 0 && total >= self.degrade_threshold() {
            let _ = self.degrade.compare_exchange(
                LATCH_IDLE,
                LATCH_PENDING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
    }

    pub fn spent(&self) -> u64 {
        self.spent.load(Ordering::SeqCst)
    }

    /// `u64::MAX` while the budget is unbounded, because no request can ever be
    /// refused for lack of room.
    pub fn remaining(&self) -> u64 {
        if self.max_tokens == 0 {
            u64::MAX
        } else {
            self.max_tokens.saturating_sub(self.spent())
        }
    }

    /// True when no ceiling is configured, so `remaining` is a sentinel rather
    /// than a real quantity.
    pub fn is_unbounded(&self) -> bool {
        self.max_tokens == 0
    }

    pub fn state(&self) -> BudgetState {
        if self.max_tokens == 0 {
            return BudgetState::Normal;
        }
        let spent = self.spent();
        if spent >= self.max_tokens {
            BudgetState::Exhausted
        } else if spent >= self.degrade_threshold() {
            BudgetState::Warning
        } else {
            BudgetState::Normal
        }
    }

    /// Returns `true` exactly once after the degradation threshold is crossed,
    /// and `false` on every later call.
    ///
    /// The hint is a decision, not a level: taking it once is what keeps a
    /// caller from re-deciding to degrade after it already did.
    pub fn should_degrade(&self) -> bool {
        self.degrade
            .compare_exchange(
                LATCH_PENDING,
                LATCH_TAKEN,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    /// The model to switch to, if the spec named one.
    ///
    /// Callers gate this on [`TokenBudget::should_degrade`] or
    /// [`TokenBudget::state`]; asking for it does not consume the latch.
    pub fn degraded_model(&self, spec: &AgentSpec) -> Option<String> {
        spec.fallback_model.clone()
    }

    fn degrade_threshold(&self) -> u64 {
        if self.max_tokens == 0 {
            return u64::MAX;
        }
        let raw = self.max_tokens as f64 * self.degrade_at;
        // Round up, so 80% of 100 degrades at 80 rather than at 79.
        raw.ceil().clamp(1.0, u64::MAX as f64) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::TokenUsage;

    fn spec_with_fallback(model: Option<&str>) -> AgentSpec {
        AgentSpec {
            id: "tester".into(),
            fallback_model: model.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn a_zero_ceiling_means_unbounded() {
        let budget = TokenBudget::new(0);
        budget.record(TokenUsage::new(1_000_000, 1_000_000));

        assert_eq!(budget.state(), BudgetState::Normal);
        assert!(!budget.should_degrade());
        assert_eq!(budget.remaining(), u64::MAX);
        assert_eq!(budget.spent(), 2_000_000);
    }

    #[test]
    fn the_degradation_hint_fires_exactly_once() {
        let budget = TokenBudget::new(100);

        budget.record(TokenUsage::new(79, 0));
        assert_eq!(budget.state(), BudgetState::Normal);
        assert!(!budget.should_degrade());

        budget.record(TokenUsage::new(1, 0));
        assert_eq!(budget.state(), BudgetState::Warning);
        assert!(budget.should_degrade(), "the threshold was crossed");
        assert!(!budget.should_degrade(), "the latch is take-once");
        assert!(!budget.should_degrade());

        // Spending more does not re-arm the hint.
        budget.record(TokenUsage::new(5, 0));
        assert!(!budget.should_degrade());
        assert_eq!(budget.state(), BudgetState::Warning);
    }

    #[test]
    fn a_single_large_step_also_arms_the_hint() {
        let budget = TokenBudget::new(100);
        budget.record(TokenUsage::new(10_000, 0));

        assert_eq!(budget.state(), BudgetState::Exhausted);
        assert!(budget.should_degrade());
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn exhaustion_only_arrives_at_the_ceiling() {
        let budget = TokenBudget::new(100);

        budget.record(TokenUsage::new(80, 0));
        assert_eq!(budget.state(), BudgetState::Warning);
        assert_eq!(budget.remaining(), 20);

        budget.record(TokenUsage::new(19, 0));
        assert_eq!(budget.state(), BudgetState::Warning, "99 of 100 is not out");

        budget.record(TokenUsage::new(1, 0));
        assert_eq!(budget.state(), BudgetState::Exhausted);
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn the_threshold_follows_the_configured_fraction() {
        let budget = TokenBudget::new(100).with_degrade_at(0.5);
        budget.record(TokenUsage::new(50, 0));
        assert_eq!(budget.state(), BudgetState::Warning);

        // A non-finite fraction must not silently disable or invert the signal.
        let budget = TokenBudget::new(100).with_degrade_at(f64::NAN);
        budget.record(TokenUsage::new(80, 0));
        assert_eq!(budget.state(), BudgetState::Warning);

        let budget = TokenBudget::new(100).with_degrade_at(-1.0);
        budget.record(TokenUsage::new(80, 0));
        assert_eq!(budget.state(), BudgetState::Warning);
    }

    #[test]
    fn the_degraded_model_is_the_spec_fallback() {
        let budget = TokenBudget::new(100);
        assert_eq!(
            budget.degraded_model(&spec_with_fallback(Some("mock-2"))),
            Some("mock-2".to_string())
        );
        assert_eq!(budget.degraded_model(&spec_with_fallback(None)), None);

        // Asking does not consume the hint.
        budget.record(TokenUsage::new(80, 0));
        assert_eq!(
            budget.degraded_model(&spec_with_fallback(Some("mock-2"))),
            Some("mock-2".to_string())
        );
        assert!(budget.should_degrade());
    }

    #[test]
    fn usage_is_shared_across_clones_of_the_arc() {
        let budget = std::sync::Arc::new(TokenBudget::new(100));
        let clone = std::sync::Arc::clone(&budget);

        clone.record(TokenUsage::new(30, 10));
        budget.record(TokenUsage::new(10, 10));

        assert_eq!(budget.spent(), 60);
        assert_eq!(clone.remaining(), 40);
    }
}
