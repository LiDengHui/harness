//! Loop detection: a tool called over and over with identical arguments.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use harness_core::{HarnessError, Result};

use crate::{GuardContext, GuardSource, GuardVerdict, Guardrail};

/// Refuses a tool call once the same `(name, arguments)` pair has been seen
/// more than `max_repeats` times inside one run.
///
/// A model that repeats a call is not making progress, and the loop would
/// otherwise keep spending tokens until the iteration ceiling. State is
/// per-run, so [`Guardrail::reset`] must be called at the start of each run.
pub struct BehaviorMonitor {
    max_repeats: usize,
    seen: Mutex<HashMap<String, usize>>,
}

impl BehaviorMonitor {
    pub fn new(max_repeats: usize) -> Self {
        Self {
            max_repeats,
            seen: Mutex::new(HashMap::new()),
        }
    }

    pub fn max_repeats(&self) -> usize {
        self.max_repeats
    }

    fn counts(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, usize>>> {
        self.seen
            .lock()
            .map_err(|_| HarnessError::Other("behavior monitor state is poisoned".into()))
    }
}

#[async_trait]
impl Guardrail for BehaviorMonitor {
    fn name(&self) -> &str {
        "behavior_monitor"
    }

    async fn inspect(&self, ctx: &GuardContext) -> Result<GuardVerdict> {
        if ctx.source != GuardSource::ToolCall {
            return Ok(GuardVerdict::Allow);
        }
        let name = ctx.tool_name.as_deref().unwrap_or_default();
        // The name and the arguments together identify the call; a newline
        // separator keeps the two from running into one another.
        let key = format!("{name}\n{}", ctx.text);

        let mut seen = self.counts()?;
        let count = seen.entry(key).or_insert(0);
        *count += 1;
        if *count > self.max_repeats {
            return Ok(GuardVerdict::Block {
                detail: format!("tool `{name}` called {count} times with identical arguments"),
            });
        }
        Ok(GuardVerdict::Allow)
    }

    fn reset(&self) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(path: &str) -> GuardContext {
        GuardContext::tool_call("read_file", json!({ "path": path }))
    }

    #[tokio::test]
    async fn the_call_after_the_limit_is_refused() {
        let monitor = BehaviorMonitor::new(2);

        assert_eq!(
            monitor.inspect(&call("a.txt")).await.unwrap(),
            GuardVerdict::Allow
        );
        assert_eq!(
            monitor.inspect(&call("a.txt")).await.unwrap(),
            GuardVerdict::Allow
        );

        let verdict = monitor.inspect(&call("a.txt")).await.unwrap();
        assert!(matches!(verdict, GuardVerdict::Block { .. }), "{verdict:?}");
    }

    #[tokio::test]
    async fn different_arguments_are_not_a_loop() {
        let monitor = BehaviorMonitor::new(1);

        assert_eq!(
            monitor.inspect(&call("a.txt")).await.unwrap(),
            GuardVerdict::Allow
        );
        assert_eq!(
            monitor.inspect(&call("b.txt")).await.unwrap(),
            GuardVerdict::Allow
        );
    }

    #[tokio::test]
    async fn reset_starts_a_fresh_run() {
        let monitor = BehaviorMonitor::new(1);
        assert_eq!(
            monitor.inspect(&call("a.txt")).await.unwrap(),
            GuardVerdict::Allow
        );
        assert!(matches!(
            monitor.inspect(&call("a.txt")).await.unwrap(),
            GuardVerdict::Block { .. }
        ));

        monitor.reset();
        assert_eq!(
            monitor.inspect(&call("a.txt")).await.unwrap(),
            GuardVerdict::Allow
        );
    }

    #[tokio::test]
    async fn only_tool_calls_are_counted() {
        let monitor = BehaviorMonitor::new(0);
        for _ in 0..3 {
            assert_eq!(
                monitor
                    .inspect(&GuardContext::tool_result("read_file"))
                    .await
                    .unwrap(),
                GuardVerdict::Allow
            );
        }
    }
}
