//! The inbound control channel of a running agent loop.
//!
//! Steering is what separates this design from a one-shot SSE stream: the user
//! can inject an instruction, approve a gated tool call, or abort, all while the
//! agent is mid-run. The loop drains this channel at every turn boundary.

use harness_core::Priority;
use tokio::sync::mpsc;

/// Something the outside world wants a running agent to do.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    /// Guidance injected into the conversation at the next turn boundary.
    Steering { text: String, priority: Priority },
    /// Stop the run; the session itself stays alive.
    Abort { reason: Option<String> },
    /// Answer for a `tool_approval` request raised by the permission policy.
    ToolApproval {
        tool_call_id: String,
        approved: bool,
        reason: Option<String>,
    },
}

impl ControlMessage {
    pub fn is_abort(&self) -> bool {
        matches!(self, ControlMessage::Abort { .. })
    }
}

/// Cheap, cloneable sender handed to the UI and the CLI.
#[derive(Debug, Clone)]
pub struct ControlHandle {
    tx: mpsc::UnboundedSender<ControlMessage>,
}

impl ControlHandle {
    /// Creates a connected handle/channel pair.
    pub fn channel() -> (ControlHandle, ControlChannel) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ControlHandle { tx }, ControlChannel { rx })
    }

    /// Returns `false` when the receiving loop has already finished.
    pub fn steer(&self, text: impl Into<String>, priority: Priority) -> bool {
        self.tx
            .send(ControlMessage::Steering {
                text: text.into(),
                priority,
            })
            .is_ok()
    }

    pub fn abort(&self, reason: Option<String>) -> bool {
        self.tx.send(ControlMessage::Abort { reason }).is_ok()
    }

    pub fn approve(
        &self,
        tool_call_id: impl Into<String>,
        approved: bool,
        reason: Option<String>,
    ) -> bool {
        self.tx
            .send(ControlMessage::ToolApproval {
                tool_call_id: tool_call_id.into(),
                approved,
                reason,
            })
            .is_ok()
    }
}

/// The receiving half, owned by the running agent loop.
#[derive(Debug)]
pub struct ControlChannel {
    rx: mpsc::UnboundedReceiver<ControlMessage>,
}

impl ControlChannel {
    pub async fn recv(&mut self) -> Option<ControlMessage> {
        self.rx.recv().await
    }

    /// Non-blocking drain, used to batch messages already queued at a boundary.
    pub fn try_drain(&mut self) -> Vec<ControlMessage> {
        let mut drained = Vec::new();
        while let Ok(message) = self.rx.try_recv() {
            drained.push(message);
        }
        drained
    }

    pub(crate) fn raw(&mut self) -> &mut mpsc::UnboundedReceiver<ControlMessage> {
        &mut self.rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_arrive_in_the_order_they_were_sent() {
        let (handle, mut channel) = ControlHandle::channel();

        assert!(handle.steer("focus on the failing test", Priority::High));
        assert!(handle.approve("call_1", true, None));
        assert!(handle.abort(Some("user pressed stop".into())));

        assert_eq!(
            channel.recv().await,
            Some(ControlMessage::Steering {
                text: "focus on the failing test".into(),
                priority: Priority::High,
            })
        );
        assert_eq!(
            channel.recv().await,
            Some(ControlMessage::ToolApproval {
                tool_call_id: "call_1".into(),
                approved: true,
                reason: None,
            })
        );
        let last = channel.recv().await.unwrap();
        assert!(last.is_abort());
    }

    #[tokio::test]
    async fn sending_to_a_finished_loop_reports_failure() {
        let (handle, channel) = ControlHandle::channel();
        drop(channel);
        assert!(!handle.abort(None));
        assert!(!handle.steer("too late", Priority::Normal));
    }

    #[tokio::test]
    async fn drain_collects_everything_already_queued() {
        let (handle, mut channel) = ControlHandle::channel();
        handle.steer("one", Priority::Normal);
        handle.steer("two", Priority::Normal);
        assert!(channel.try_drain().len() == 2);
        assert!(channel.try_drain().is_empty());
    }
}
