//! What happens when a subscriber stops reading.
//!
//! Every connection has a bounded outbound queue (`capacity` messages). The run
//! task's bridge fans an event out to every subscriber through that queue, and
//! `try_send` is the whole point: awaiting a full queue inside the bridge would
//! let one stalled browser tab freeze a run for everybody else watching the same
//! session. Growing the queue is the other tempting answer and it is the one that
//! turns a slow client into unbounded server memory.
//!
//! So the policy is *bounded stall, then disconnect*:
//!
//! | consecutive fulls | decision | why |
//! |---|---|---|
//! | `0` | [`Overflow::Wait`] | The first full queue is usually a burst that drains on its own. Stalling the bridge for at most `wait` once is cheaper than dropping a frame the client needs. |
//! | `1..max` | [`Overflow::Drop`] | Still full after that wait: this consumer is behind. Dropping keeps every *other* subscriber live, and the next successful send resets the count. |
//! | `>= max` | [`Overflow::CloseSubscriber`] | Not recovering after `max_consecutive_full` messages in a row. The connection is closed so its backlog can never grow server memory without bound. |
//!
//! The observable failure mode of a slow consumer is therefore: it is
//! disconnected after `max_consecutive_full` consecutive full queues, having had
//! one `BackpressurePolicy::wait` of grace. The farewell
//! [`ServerMessage::Error`] with code `backpressure` is a best-effort `try_send`
//! — by definition the queue was full, so the client usually sees only the close
//! frame. That is deliberate: a client that is not reading cannot be told
//! anything, and its session stays readable through the REST API.

use std::time::Duration;

use harness_core::ServerEnvelope;
use tokio::sync::mpsc::{self, error::TrySendError, Sender};

/// What to do with an outbound message when a subscriber's queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Wait,
    Drop,
    CloseSubscriber,
}

#[derive(Debug, Clone)]
pub struct BackpressurePolicy {
    pub capacity: usize,
    pub max_consecutive_full: u32,
    pub wait: Duration,
}

impl BackpressurePolicy {
    /// `try_send` first; on a full queue, decide from the consecutive-full count.
    pub fn on_full(&self, consecutive_full: u32) -> Overflow {
        if consecutive_full >= self.max_consecutive_full {
            Overflow::CloseSubscriber
        } else if consecutive_full == 0 {
            Overflow::Wait
        } else {
            Overflow::Drop
        }
    }
}

impl Default for BackpressurePolicy {
    /// A 256-message queue, eight consecutive fulls tolerated, five seconds of
    /// grace. The queue is large enough to absorb a burst of streaming deltas and
    /// the `wait` is long enough to cover a client that stalls on a garbage
    /// collection pause, not a laptop that was closed.
    fn default() -> Self {
        Self {
            capacity: 256,
            max_consecutive_full: 8,
            wait: Duration::from_secs(5),
        }
    }
}

/// What one delivery attempt ended up doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// The message is in the queue.
    Sent,
    /// The message was discarded because this consumer is behind.
    Dropped,
    /// The message could not be delivered and the consumer is being closed.
    Closed,
}

/// One subscriber's outbound queue plus the state the policy needs.
///
/// This is the testable half of the wiring: it owns no socket, so the decisions
/// above can be exercised with a real `mpsc` channel and a real full queue.
pub(crate) struct Outbox {
    tx: Sender<ServerEnvelope>,
    /// Sent, best effort, when the consumer is disconnected.
    farewell: ServerEnvelope,
    policy: BackpressurePolicy,
    consecutive_full: u32,
}

impl Outbox {
    pub(crate) fn new(
        tx: Sender<ServerEnvelope>,
        farewell: ServerEnvelope,
        policy: BackpressurePolicy,
    ) -> Self {
        Self {
            tx,
            farewell,
            policy,
            consecutive_full: 0,
        }
    }

    /// Deliver one envelope, never blocking longer than `policy.wait`.
    pub(crate) async fn send(&mut self, envelope: ServerEnvelope) -> Delivery {
        let mut envelope = envelope;
        loop {
            match self.tx.try_send(envelope) {
                Ok(()) => {
                    self.consecutive_full = 0;
                    return Delivery::Sent;
                }
                Err(TrySendError::Closed(_)) => return Delivery::Closed,
                Err(TrySendError::Full(returned)) => {
                    // `try_send` hands the message back; it is the same one this
                    // call was asked to deliver, so the retry needs no clone.
                    envelope = returned;
                    let decision = self.policy.on_full(self.consecutive_full);
                    self.consecutive_full = self.consecutive_full.saturating_add(1);

                    match decision {
                        Overflow::Wait => {
                            // A cancelled `send` never enqueued the message, so the
                            // clone that is still held is the one to retry with.
                            let waited = tokio::time::timeout(
                                self.policy.wait,
                                self.tx.send(envelope.clone()),
                            )
                            .await;
                            match waited {
                                Ok(Ok(())) => {
                                    self.consecutive_full = 0;
                                    return Delivery::Sent;
                                }
                                Ok(Err(_)) => return Delivery::Closed,
                                // Still full: loop, and let the incremented count
                                // decide what happens next.
                                Err(_elapsed) => {}
                            }
                        }
                        Overflow::Drop => return Delivery::Dropped,
                        Overflow::CloseSubscriber => {
                            self.close();
                            return Delivery::Closed;
                        }
                    }
                }
            }
        }
    }

    /// Best-effort farewell. When the queue is still full — which is the normal
    /// case at this point — the consumer learns nothing, and is disconnected
    /// anyway.
    pub(crate) fn close(&mut self) {
        let _ = self.tx.try_send(self.farewell.clone());
    }
}

/// The queue a connection's writer task drains.
pub(crate) fn channel(
    policy: &BackpressurePolicy,
) -> (Sender<ServerEnvelope>, mpsc::Receiver<ServerEnvelope>) {
    mpsc::channel(policy.capacity.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::{AgentId, ServerMessage, SessionId};

    fn envelope(text: &str) -> ServerEnvelope {
        ServerEnvelope::new(
            SessionId::new(),
            AgentId::new("tester").unwrap(),
            ServerMessage::AssistantChunk {
                text: text.to_string(),
            },
        )
    }

    fn farewell() -> ServerEnvelope {
        ServerEnvelope::new(
            SessionId::new(),
            AgentId::new("tester").unwrap(),
            ServerMessage::Error {
                code: "backpressure".into(),
                message: "subscriber too slow".into(),
            },
        )
    }

    fn policy(capacity: usize, max_consecutive_full: u32) -> BackpressurePolicy {
        BackpressurePolicy {
            capacity,
            max_consecutive_full,
            // Short enough to keep the tests fast; the default is what ships.
            wait: Duration::from_millis(5),
        }
    }

    #[test]
    fn the_default_policy_is_the_documented_one() {
        let default = BackpressurePolicy::default();
        assert_eq!(default.capacity, 256);
        assert_eq!(default.max_consecutive_full, 8);
        assert_eq!(default.wait, Duration::from_secs(5));
    }

    #[test]
    fn on_full_waits_once_then_drops_then_closes() {
        let policy = policy(4, 8);

        assert_eq!(policy.on_full(0), Overflow::Wait);
        assert_eq!(policy.on_full(1), Overflow::Drop);
        assert_eq!(policy.on_full(7), Overflow::Drop);
        assert_eq!(policy.on_full(8), Overflow::CloseSubscriber);
        assert_eq!(policy.on_full(9), Overflow::CloseSubscriber);
        assert_eq!(policy.on_full(u32::MAX), Overflow::CloseSubscriber);
    }

    #[test]
    fn a_policy_with_no_patience_closes_on_the_first_repeat() {
        let policy = policy(1, 1);
        assert_eq!(policy.on_full(0), Overflow::Wait);
        assert_eq!(policy.on_full(1), Overflow::CloseSubscriber);
    }

    #[tokio::test]
    async fn a_draining_consumer_is_never_disturbed() {
        let policy = policy(4, 2);
        let (tx, mut rx) = channel(&policy);
        let mut outbox = Outbox::new(tx, farewell(), policy);

        for index in 0..32 {
            assert_eq!(
                outbox.send(envelope(&index.to_string())).await,
                Delivery::Sent
            );
            let delivered = rx.try_recv().expect("the queue is not full");
            assert_eq!(delivered.message, envelope(&index.to_string()).message);
        }
        assert_eq!(outbox.consecutive_full, 0);
    }

    #[tokio::test]
    async fn a_stalled_consumer_is_dropped_then_closed() {
        let policy = policy(1, 4);
        let (tx, mut rx) = channel(&policy);
        let mut outbox = Outbox::new(tx, farewell(), policy);

        // Nothing is reading, so the queue stays full from here on.
        assert_eq!(outbox.send(envelope("fills")).await, Delivery::Sent);

        // The first full queue waits (and times out) before being dropped; every
        // later one is dropped immediately.
        assert_eq!(outbox.send(envelope("one")).await, Delivery::Dropped);
        assert_eq!(outbox.send(envelope("two")).await, Delivery::Dropped);
        assert_eq!(outbox.send(envelope("three")).await, Delivery::Dropped);

        // Reaching `max_consecutive_full` closes this consumer rather than
        // letting its backlog grow.
        assert_eq!(outbox.send(envelope("four")).await, Delivery::Closed);
        assert_eq!(outbox.send(envelope("five")).await, Delivery::Closed);

        // Only the message that fit was ever queued: the queue is still exactly
        // one message long, so the server has not buffered anything for a client
        // that stopped reading.
        let first = rx.try_recv().expect("the message that fit is still queued");
        assert_eq!(first.message, envelope("fills").message);
        assert!(
            rx.try_recv().is_err(),
            "a stalled consumer may not accumulate messages"
        );
    }

    #[tokio::test]
    async fn the_farewell_is_sent_when_there_is_room() {
        let policy = policy(1, 1);
        let (tx, mut rx) = channel(&policy);
        let mut outbox = Outbox::new(tx, farewell(), policy);

        outbox.close();
        let received = rx.try_recv().expect("the farewell fits");
        assert!(matches!(
            received.message,
            ServerMessage::Error { ref code, .. } if code == "backpressure"
        ));
    }

    #[tokio::test]
    async fn the_farewell_is_dropped_when_there_is_no_room() {
        let policy = policy(1, 1);
        let (tx, mut rx) = channel(&policy);
        let mut outbox = Outbox::new(tx, farewell(), policy);

        outbox.send(envelope("fills")).await;
        outbox.close();
        outbox.close();

        // Best effort by definition: a client that is not reading cannot be told
        // why it is being disconnected, and nothing new may be buffered for it.
        assert_eq!(
            rx.try_recv().ok().map(|env| env.message),
            Some(envelope("fills").message)
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_consumer_that_frees_room_during_the_wait_is_kept() {
        let policy = BackpressurePolicy {
            capacity: 1,
            max_consecutive_full: 2,
            wait: Duration::from_secs(5),
        };
        let (tx, mut rx) = channel(&policy);
        let mut outbox = Outbox::new(tx, farewell(), policy);

        assert_eq!(outbox.send(envelope("fills")).await, Delivery::Sent);

        // Drain while the sender is inside its bounded wait: the retry succeeds,
        // so the counter resets instead of climbing towards a disconnect. Both
        // halves run as one task so the receiver cannot be dropped while the
        // sender is still waiting for a permit.
        let (delivery, drained) = tokio::join!(outbox.send(envelope("second")), async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            rx.recv().await
        });

        assert_eq!(delivery, Delivery::Sent);
        assert_eq!(outbox.consecutive_full, 0);
        assert!(drained.is_some());
    }

    #[tokio::test]
    async fn a_closed_receiver_ends_the_subscription_without_waiting() {
        let policy = policy(1, 8);
        let (tx, rx) = channel(&policy);
        drop(rx);
        let mut outbox = Outbox::new(tx, farewell(), policy);

        assert_eq!(outbox.send(envelope("gone")).await, Delivery::Closed);
    }
}
