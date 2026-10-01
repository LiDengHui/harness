//! A bounded LRU cache in front of any provider.
//!
//! This is the project spec's "similar queries reuse the LLM response, a repeated
//! query costs 0 tokens" item. `CachedProvider` decorates another `Provider` and
//! answers a request it has already seen from memory instead of from the network.
//!
//! # What a hit is
//!
//! Only a *successful* turn is stored, and the whole turn is stored: the assembled
//! `ProviderResponse` together with the exact `ProviderEvent` deltas that produced
//! it. A hit therefore still streams — the caller's sink receives the same deltas
//! in the same order — and the returned response is the one the model actually
//! gave, reasoning deltas included, which a response-only cache could not replay.
//!
//! A hit is counted, not hidden. `CacheStats::hits` goes up and the hit returns the
//! *original* `usage`, so the caller's token accounting shows the saving. Reporting
//! a zero usage would be a lie: no tokens were spent on this call, but the answer
//! cost what it cost when it was first produced.
//!
//! # What this is not
//!
//! This is an optimisation for *repeated identical requests*, not a semantic
//! deduplicator. Two prompts that merely mean the same thing, or that share a
//! prefix, do not share an answer: the key covers the canonical form of the whole
//! request, so any difference in a message, a tool spec, the temperature or the
//! token ceiling is a miss. It is not a request coalescer either — callers that
//! miss on the same key at the same time each make their own call; only a caller
//! arriving after the first has stored the answer hits.
//!
//! # Where it is installed
//!
//! `AgentLoop::new` wraps its provider when `AgentConfig::response_cache_capacity`
//! is non-zero, so a caller that knows its requests repeat — a sub-agent fan-out
//! that hands several workers the same objective, or a replay from a memory
//! snapshot — can install it. It is off by default because the loop's own turns
//! never repeat: every request appends the previous turn's assistant message and
//! tool result, so the key differs each turn and the cache would only add
//! bookkeeping. The in-flight trim, not this, is what cuts a long run's bill.
//!
//! # The key
//!
//! [`canonical_key`] serialises the semantic request — model, every message (role,
//! content, tool-call ids/names/arguments, `tool_call_id`), the tool specs, the
//! temperature, `max_tokens` and the reasoning effort — into one JSON document,
//! and the cache key is FNV-1a over those bytes. Two normalisations are deliberate:
//!
//!   * the tool list is sorted, because the order tools are advertised in cannot
//!     change the answer;
//!   * floats are hashed as bit patterns, because `f32` is not `Hash` and a `NaN`
//!     temperature must not silently equal "unset".
//!
//! Message order and tool-call order are *not* normalised: they are part of the
//! transcript the model saw. Each entry also keeps its canonical string, so the
//! astronomically unlikely hash collision degrades to a miss rather than returning
//! another request's answer.
//!
//! # Why the LRU is hand-rolled
//!
//! `lru` is listed in the workspace manifest but is not a dependency of this crate,
//! and pulling it in would mean editing this crate's manifest for roughly thirty
//! lines of code. Recency here is a monotonic tick per entry and eviction scans for
//! the smallest tick; the cache is small by construction, so that linear scan costs
//! less than the bookkeeping an intrusive list would need.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use harness_core::{Message, Result};

use crate::provider::Provider;
use crate::request::{ChatRequest, ProviderEvent, ProviderResponse};

/// FNV-1a 64-bit: dependency-free, and stable across processes and Rust versions,
/// which `DefaultHasher` does not promise.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Cumulative counters for one cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStats {
    /// Requests answered from the cache.
    pub hits: u64,
    /// Requests that had to reach the inner provider.
    pub misses: u64,
    /// Responses currently held.
    pub entries: usize,
}

impl CacheStats {
    /// Hits as a fraction of all lookups; `0.0` before the first request.
    pub fn hit_rate(&self) -> f64 {
        let lookups = self.hits.saturating_add(self.misses);
        if lookups == 0 {
            0.0
        } else {
            self.hits as f64 / lookups as f64
        }
    }
}

#[derive(Clone)]
struct Entry {
    /// The canonical form the key was hashed from, kept so that a hash collision
    /// is detectable instead of silently serving the wrong answer.
    canonical: String,
    response: ProviderResponse,
    events: Vec<ProviderEvent>,
    /// Tick of the last hit or insert; the smallest tick is evicted first.
    last_used: u64,
}

struct Inner {
    capacity: usize,
    entries: HashMap<u64, Entry>,
    /// Strictly increasing, so no two entries ever share a tick.
    clock: u64,
}

/// A provider that answers repeated identical requests without calling the inner
/// provider.
pub struct CachedProvider {
    inner: Arc<dyn Provider>,
    state: Mutex<Inner>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CachedProvider {
    /// Wraps `inner`, holding at most `capacity` responses.
    ///
    /// A capacity of zero is legal and disables caching: every request misses and
    /// nothing is ever stored.
    pub fn new(inner: Arc<dyn Provider>, capacity: usize) -> Self {
        Self {
            inner,
            state: Mutex::new(Inner {
                capacity,
                entries: HashMap::new(),
                clock: 0,
            }),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// A snapshot of the counters; cheap enough to call once per turn.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            entries: self.state().entries.len(),
        }
    }

    /// Drops every cached response.
    ///
    /// The counters survive deliberately: they measure how much the cache saved
    /// over the run, which forgetting the responses does not undo.
    pub fn clear(&self) {
        self.state().entries.clear();
    }

    /// Takes the lock without panicking on poison.
    ///
    /// A panic while the lock was held cannot leave a `HashMap` of cloned
    /// responses in a state worth refusing to serve, so the cache carries on with
    /// the inner value rather than failing every later request.
    fn state(&self) -> MutexGuard<'_, Inner> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The stored turn, freshly marked as most recently used, or `None` on a miss.
    fn lookup(&self, key: u64, canonical: &str) -> Option<Entry> {
        let mut state = self.state();
        let fresh = state
            .entries
            .get(&key)
            .is_some_and(|entry| entry.canonical == canonical);
        if !fresh {
            return None;
        }

        state.clock = state.clock.wrapping_add(1);
        let tick = state.clock;
        let entry = state.entries.get_mut(&key)?;
        entry.last_used = tick;
        Some(entry.clone())
    }

    fn insert(
        &self,
        key: u64,
        canonical: String,
        response: ProviderResponse,
        events: Vec<ProviderEvent>,
    ) {
        let mut state = self.state();
        state.clock = state.clock.wrapping_add(1);
        let last_used = state.clock;
        state.entries.insert(
            key,
            Entry {
                canonical,
                response,
                events,
                last_used,
            },
        );

        while state.entries.len() > state.capacity {
            let victim = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key);
            let Some(victim) = victim else { break };
            state.entries.remove(&victim);
        }
    }
}

#[async_trait]
impl Provider for CachedProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn stream(
        &self,
        request: ChatRequest,
        events: UnboundedSender<ProviderEvent>,
    ) -> Result<ProviderResponse> {
        let canonical = canonical_key(&request);
        let key = fnv1a(canonical.as_bytes());

        if let Some(entry) = self.lookup(key, &canonical) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            // Replaying into a sink the caller has already dropped is not a
            // failure: the answer is in hand either way.
            for event in &entry.events {
                let _ = events.send(event.clone());
            }
            return Ok(entry.response);
        }

        self.misses.fetch_add(1, Ordering::Relaxed);

        let (tap_tx, mut tap_rx) = unbounded_channel::<ProviderEvent>();
        let sink = events.clone();
        let recorded = Arc::new(Mutex::new(Vec::<ProviderEvent>::new()));
        let recorder = Arc::clone(&recorded);
        // The forwarder runs alongside the turn, so a miss still streams as the
        // deltas arrive while it records them for a later hit to replay.
        let forwarder = tokio::spawn(async move {
            while let Some(event) = tap_rx.recv().await {
                match recorder.lock() {
                    Ok(mut log) => log.push(event.clone()),
                    Err(poisoned) => poisoned.into_inner().push(event.clone()),
                }
                // A closed sink must not stop the recording, or a hit would later
                // replay a truncated stream.
                let _ = sink.send(event);
            }
        });

        let result = self.inner.stream(request, tap_tx).await;
        // The inner provider drops the tap sender before returning, so the
        // forwarder is done once the last event is through.
        let _ = forwarder.await;

        let response = result?;

        let log = match recorded.lock() {
            Ok(log) => log.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        self.insert(key, canonical, response.clone(), log);

        Ok(response)
    }
}

/// The canonical form of a request's semantics.
///
/// Everything that can change the answer is in here and nothing that cannot: no
/// ids, no timestamps, no ordering the model cannot observe.
fn canonical_key(request: &ChatRequest) -> String {
    let messages: Vec<Value> = request.messages.iter().map(canonical_message).collect();

    // Sorting the tools makes the key independent of the order they were
    // advertised in, which the model cannot observe.
    let mut tools: Vec<(String, Value)> = request
        .tools
        .iter()
        .map(|tool| {
            let value = json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            });
            (value.to_string(), value)
        })
        .collect();
    tools.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    let tools: Vec<Value> = tools.into_iter().map(|(_, value)| value).collect();

    json!({
        "model": request.model,
        "messages": messages,
        "tools": tools,
        // Bit patterns, because `f32` is not `Hash` and `NaN` must not compare
        // equal to an unset temperature.
        "temperature": request.temperature.map(f32::to_bits),
        "max_tokens": request.max_tokens,
        // Part of the key: the same prompt at `low` and at `max` are different
        // requests, and sharing an answer between them would make the level a
        // no-op whenever the cache is in front.
        "reasoning_effort": request.reasoning_effort,
    })
    .to_string()
}

fn canonical_message(message: &Message) -> Value {
    let tool_calls: Vec<Value> = message
        .tool_calls
        .iter()
        .map(|call| {
            json!({
                "id": call.id,
                "name": call.name,
                "arguments": call.arguments,
            })
        })
        .collect();

    json!({
        "role": message.role.as_str(),
        "content": message.content,
        "tool_calls": tool_calls,
        "tool_call_id": message.tool_call_id,
        "name": message.name,
    })
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockProvider, ScriptedTurn};
    use harness_core::{HarnessError, Message, Role, ToolCall, ToolSpec};
    use tokio::sync::mpsc::UnboundedReceiver;

    fn cached(
        capacity: usize,
        script: Vec<ScriptedTurn>,
    ) -> (Arc<CachedProvider>, Arc<MockProvider>) {
        let mock = Arc::new(MockProvider::new("mock", "mock-model", script));
        let cached = Arc::new(CachedProvider::new(mock.clone(), capacity));
        (cached, mock)
    }

    fn request(text: &str) -> ChatRequest {
        ChatRequest::new("mock-model", vec![Message::user(text)])
    }

    /// A request that exercises every field the key covers.
    fn rich_request(mutate: impl FnOnce(&mut ChatRequest)) -> ChatRequest {
        let mut request = ChatRequest::new(
            "mock-model",
            vec![
                Message::system("be terse"),
                Message::user("hello"),
                Message::assistant_tool_calls(vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: json!({ "path": "src/main.rs" }),
                }]),
                Message::tool_result("call_1", "fn main() {}"),
            ],
        );
        request.tools = vec![ToolSpec::new(
            "read_file",
            "reads a file",
            json!({ "type": "object" }),
        )];
        request.temperature = Some(0.2);
        request.max_tokens = Some(512);
        mutate(&mut request);
        request
    }

    async fn drain(mut rx: UnboundedReceiver<ProviderEvent>) -> Vec<ProviderEvent> {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    /// One turn, with the outcome left unchecked for the tests that assert on it.
    async fn call_raw(
        provider: &CachedProvider,
        request: ChatRequest,
    ) -> (Result<ProviderResponse>, Vec<ProviderEvent>) {
        let (tx, rx) = unbounded_channel();
        let response = provider.stream(request, tx).await;
        let events = drain(rx).await;
        (response, events)
    }

    /// One turn that is expected to succeed.
    async fn call(
        provider: &CachedProvider,
        request: ChatRequest,
    ) -> (ProviderResponse, Vec<ProviderEvent>) {
        let (response, events) = call_raw(provider, request).await;
        (response.unwrap(), events)
    }

    #[test]
    fn the_decorator_reports_the_inner_identity() {
        let (provider, _mock) = cached(4, Vec::new());
        assert_eq!(provider.id(), "mock");
        assert_eq!(provider.model(), "mock-model");
    }

    #[tokio::test]
    async fn a_repeated_request_never_reaches_the_provider() {
        let (provider, mock) = cached(8, vec![ScriptedTurn::Text("hello".into())]);

        let (first, _) = call(&provider, request("hi")).await;
        let (second, _) = call(&provider, request("hi")).await;

        assert_eq!(first.message.text(), "hello");
        assert_eq!(second.message.text(), "hello");
        assert_eq!(
            mock.call_count(),
            1,
            "the second call should have been a hit"
        );
        assert_eq!(
            provider.stats(),
            CacheStats {
                hits: 1,
                misses: 1,
                entries: 1
            }
        );
    }

    #[tokio::test]
    async fn a_hit_replays_the_same_deltas() {
        let text = "streaming is genuinely exercised";
        let (provider, mock) = cached(8, vec![ScriptedTurn::Text(text.into())]);

        let (_, missed) = call(&provider, request("hi")).await;
        let (response, replayed) = call(&provider, request("hi")).await;

        assert!(
            missed
                .iter()
                .filter(|event| matches!(event, ProviderEvent::TextDelta(_)))
                .count()
                > 1,
            "the miss should have streamed several deltas: {missed:?}"
        );
        assert_eq!(replayed, missed, "a hit must replay the miss verbatim");
        assert_eq!(mock.call_count(), 1);

        let usage = response.usage;
        assert!(matches!(
            replayed.last(),
            Some(ProviderEvent::Usage(last)) if *last == usage
        ));
    }

    #[tokio::test]
    async fn a_hit_replays_reasoning_the_response_does_not_carry() {
        let (provider, mock) = cached(8, vec![ScriptedTurn::Thinking("weighing options".into())]);

        let (missed, _) = call(&provider, request("hi")).await;
        let (_, replayed) = call(&provider, request("hi")).await;

        assert!(missed.message.text().is_empty());
        assert_eq!(replayed.len(), 2);
        assert_eq!(
            replayed[0],
            ProviderEvent::ThinkingDelta("weighing options".into())
        );
        assert!(matches!(replayed[1], ProviderEvent::Usage(_)));
        assert_eq!(mock.call_count(), 1);
    }

    #[tokio::test]
    async fn a_hit_reports_the_original_usage_rather_than_a_zero() {
        let (provider, _mock) = cached(8, vec![ScriptedTurn::Text("hello there".into())]);

        let (missed, _) = call(&provider, request("hi")).await;
        let (hit, _) = call(&provider, request("hi")).await;

        assert!(missed.usage.total() > 0);
        assert_eq!(hit.usage, missed.usage, "a hit must not fabricate a zero");
        assert_eq!(hit.finish_reason, missed.finish_reason);
        assert_eq!(hit.tool_calls, missed.tool_calls);
    }

    #[tokio::test]
    async fn reordering_the_tool_list_is_the_same_key() {
        let (provider, mock) = cached(8, vec![ScriptedTurn::Text("done".into())]);
        let read = ToolSpec::new("read_file", "reads", json!({ "type": "object" }));
        let write = ToolSpec::new("write_file", "writes", json!({ "type": "object" }));

        let mut first = request("hi");
        first.tools = vec![read.clone(), write.clone()];
        let mut second = request("hi");
        second.tools = vec![write, read];

        assert_eq!(canonical_key(&first), canonical_key(&second));
        call(&provider, first).await;
        call(&provider, second).await;
        assert_eq!(mock.call_count(), 1);
        assert_eq!(provider.stats().hits, 1);
    }

    #[tokio::test]
    async fn a_changed_message_is_a_different_key() {
        let (provider, mock) = cached(
            8,
            vec![
                ScriptedTurn::Text("first".into()),
                ScriptedTurn::Text("second".into()),
            ],
        );

        let (first, _) = call(&provider, request("hi")).await;
        let (second, _) = call(&provider, request("hello")).await;

        assert_eq!(first.message.text(), "first");
        assert_eq!(second.message.text(), "second");
        assert_eq!(mock.call_count(), 2);
        assert_eq!(
            provider.stats(),
            CacheStats {
                hits: 0,
                misses: 2,
                entries: 2
            }
        );
    }

    #[test]
    fn independently_built_identical_requests_share_a_key() {
        assert_eq!(
            canonical_key(&rich_request(|_| {})),
            canonical_key(&rich_request(|_| {}))
        );
    }

    /// One single-field change to a request, named so a failure says which field.
    type Mutation = fn(&mut ChatRequest);

    #[test]
    fn every_semantic_field_changes_the_key() {
        let baseline = canonical_key(&rich_request(|_| {}));

        let mutations: Vec<(&str, Mutation)> = vec![
            ("model", |r| r.model = "other-model".into()),
            ("message role", |r| r.messages[1].role = Role::System),
            ("message content", |r| {
                r.messages[1].content = Some("hello!".into())
            }),
            ("missing message content", |r| r.messages[1].content = None),
            ("message name", |r| {
                r.messages[1].name = Some("harness".into())
            }),
            ("message count", |r| {
                r.messages.pop();
            }),
            ("message order", |r| r.messages.swap(0, 1)),
            ("tool call id", |r| {
                r.messages[2].tool_calls[0].id = "call_2".into()
            }),
            ("tool call name", |r| {
                r.messages[2].tool_calls[0].name = "write_file".into()
            }),
            ("tool call arguments", |r| {
                r.messages[2].tool_calls[0].arguments = json!({ "path": "src/lib.rs" })
            }),
            ("tool result id", |r| {
                r.messages[3].tool_call_id = Some("call_2".into())
            }),
            ("tool result body", |r| {
                r.messages[3].content = Some("fn other() {}".into())
            }),
            ("tool name", |r| r.tools[0].name = "open_file".into()),
            ("tool description", |r| {
                r.tools[0].description = "reads a file twice".into()
            }),
            ("tool parameters", |r| {
                r.tools[0].parameters = json!({ "type": "object", "required": [] })
            }),
            ("an extra tool", |r| {
                r.tools.push(ToolSpec::new("shell", "runs", json!({})))
            }),
            ("temperature", |r| r.temperature = Some(0.7)),
            ("unset temperature", |r| r.temperature = None),
            ("max tokens", |r| r.max_tokens = Some(1024)),
            ("unset max tokens", |r| r.max_tokens = None),
            ("reasoning effort", |r| {
                r.reasoning_effort = Some("max".into())
            }),
        ];

        for (field, mutate) in mutations {
            let changed = canonical_key(&rich_request(mutate));
            assert_ne!(
                changed, baseline,
                "changing the {field} must change the key"
            );
        }
    }

    #[tokio::test]
    async fn a_different_reasoning_effort_is_a_different_key() {
        let (provider, mock) = cached(
            8,
            vec![
                ScriptedTurn::Text("shallow".into()),
                ScriptedTurn::Text("deep".into()),
            ],
        );

        let mut low = request("hi");
        low.reasoning_effort = Some("low".into());
        let mut max = request("hi");
        max.reasoning_effort = Some("max".into());

        let (first, _) = call(&provider, low.clone()).await;
        let (second, _) = call(&provider, max).await;
        assert_eq!(first.message.text(), "shallow");
        assert_eq!(second.message.text(), "deep");
        assert_eq!(mock.call_count(), 2, "the two levels must not share a key");

        // The same level still hits, so the extra field did not disable caching.
        call(&provider, low).await;
        assert_eq!(provider.stats().hits, 1);
    }

    #[tokio::test]
    async fn a_failed_turn_is_never_cached() {
        let (provider, mock) = cached(
            8,
            vec![
                ScriptedTurn::Fail("upstream exploded".into()),
                ScriptedTurn::Text("recovered".into()),
            ],
        );

        let (first, first_events) = call_raw(&provider, request("hi")).await;
        match first.unwrap_err() {
            HarnessError::Provider { provider, message } => {
                assert_eq!(provider, "mock");
                assert_eq!(message, "upstream exploded");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(
            first_events.is_empty(),
            "a failed turn emits nothing to replay"
        );
        assert_eq!(provider.stats().entries, 0);

        let (second, _) = call(&provider, request("hi")).await;
        assert_eq!(second.message.text(), "recovered");
        assert_eq!(mock.call_count(), 2);
        assert_eq!(
            provider.stats(),
            CacheStats {
                hits: 0,
                misses: 2,
                entries: 1
            }
        );
    }

    #[tokio::test]
    async fn the_least_recently_used_entry_is_evicted_at_capacity() {
        let (provider, _mock) = cached(
            2,
            vec![
                ScriptedTurn::Text("a".into()),
                ScriptedTurn::Text("b".into()),
                ScriptedTurn::Text("c".into()),
                ScriptedTurn::Text("d".into()),
            ],
        );

        call(&provider, request("a")).await;
        call(&provider, request("b")).await;
        call(&provider, request("a")).await;
        call(&provider, request("c")).await;

        assert_eq!(
            provider.stats(),
            CacheStats {
                hits: 1,
                misses: 3,
                entries: 2
            }
        );

        // `a` survived the insert that removed `b`, because the hit refreshed it.
        let (survivor, _) = call(&provider, request("a")).await;
        assert_eq!(survivor.message.text(), "a");
        assert_eq!(provider.stats().hits, 2);

        // `b` is gone: it was the entry the LRU dropped, so this misses afresh.
        let (evicted, _) = call(&provider, request("b")).await;
        assert_eq!(evicted.message.text(), "d");
        assert_eq!(provider.stats().misses, 4);
    }

    #[tokio::test]
    async fn a_zero_capacity_cache_never_holds_anything() {
        let (provider, mock) = cached(
            0,
            vec![
                ScriptedTurn::Text("a".into()),
                ScriptedTurn::Text("b".into()),
            ],
        );

        call(&provider, request("hi")).await;
        call(&provider, request("hi")).await;

        assert_eq!(mock.call_count(), 2);
        assert_eq!(
            provider.stats(),
            CacheStats {
                hits: 0,
                misses: 2,
                entries: 0
            }
        );
    }

    #[tokio::test]
    async fn clear_drops_entries_but_keeps_the_counters() {
        let (provider, mock) = cached(
            8,
            vec![
                ScriptedTurn::Text("a".into()),
                ScriptedTurn::Text("b".into()),
            ],
        );

        call(&provider, request("hi")).await;
        assert_eq!(provider.stats().entries, 1);

        provider.clear();
        assert_eq!(provider.stats().entries, 0);

        call(&provider, request("hi")).await;
        assert_eq!(mock.call_count(), 2);
        assert_eq!(
            provider.stats(),
            CacheStats {
                hits: 0,
                misses: 2,
                entries: 1
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_all_hit_once_the_answer_is_stored() {
        let (provider, mock) = cached(8, vec![ScriptedTurn::Text("shared".into())]);
        call(&provider, request("hi")).await;

        let mut tasks = Vec::new();
        for _ in 0..16 {
            let provider = Arc::clone(&provider);
            tasks.push(tokio::spawn(async move {
                let (tx, rx) = unbounded_channel();
                let response = provider.stream(request("hi"), tx).await;
                let events = drain(rx).await;
                (response, events)
            }));
        }

        for task in tasks {
            let (response, events) = task.await.unwrap();
            assert_eq!(response.unwrap().message.text(), "shared");
            assert!(!events.is_empty(), "a hit must still stream");
        }

        assert_eq!(mock.call_count(), 1, "concurrent hits must not call out");
        assert_eq!(
            provider.stats(),
            CacheStats {
                hits: 16,
                misses: 1,
                entries: 1
            }
        );
    }

    #[test]
    fn hit_rate_is_zero_before_any_lookup_and_a_fraction_after() {
        assert_eq!(CacheStats::default().hit_rate(), 0.0);
        assert_eq!(
            CacheStats {
                hits: 1,
                misses: 3,
                entries: 0
            }
            .hit_rate(),
            0.25
        );
    }

    #[test]
    fn the_hash_is_fnv1a_and_separates_inputs() {
        assert_eq!(fnv1a(b""), FNV_OFFSET_BASIS);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv1a(b"a"), fnv1a(b"b"));
    }
}
