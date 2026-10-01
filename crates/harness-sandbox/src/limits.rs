//! Budgets a single plugin call has to fit inside.

use std::time::Duration;

/// Ceilings applied to every plugin call.
///
/// The defaults are deliberately modest: a plugin that needs more should be
/// asked for more explicitly rather than inheriting whatever the machine
/// happens to allow.
#[derive(Debug, Clone)]
pub struct SandboxLimits {
    /// Wall-clock ceiling for one call.
    pub timeout: Duration,
    /// Linear-memory ceiling in bytes.
    pub max_memory_bytes: usize,
    /// Cap on bytes a plugin may read from the host in one call.
    ///
    /// This bound is symmetric: it also sizes the reply buffer the host hands
    /// to a plugin's entry point, so it is the most a plugin can hand back in a
    /// single call as well as the most it can take in.
    pub max_read_bytes: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_memory_bytes: 16 * 1024 * 1024,
            max_read_bytes: 64 * 1024,
        }
    }
}
