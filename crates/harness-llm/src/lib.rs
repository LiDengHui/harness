//! `harness-llm`: LLM provider abstraction.
//!
//! One streaming `Provider` trait covers every backend. `OpenAiProvider` speaks
//! the OpenAI `/chat/completions` protocol, which is also what Moonshot,
//! DeepSeek, vLLM, llama.cpp and Ollama's `/v1` shim speak, so a single client
//! reaches all of them. `MockProvider` replays a script so the agent loop, the
//! CLI and the test suite can run with no key and no network.
//!
//! Conversation messages are defined in `harness-core` and re-exported here, so
//! `harness_llm::Message` remains the natural path for provider users.
//!
//! [`from_config`] is the one place that decides which backend a provider id
//! names, so the CLI and the server accept and reject the same configurations.

pub mod cache;
pub mod factory;
pub mod mock;
pub mod openai;
pub mod provider;
pub mod request;

pub use cache::{CacheStats, CachedProvider};
pub use factory::from_config;
pub use harness_core::{Message, Role, ToolCall};
pub use mock::{MockProvider, ScriptedTurn};
pub use openai::OpenAiProvider;
pub use provider::Provider;
pub use request::{ChatRequest, ProviderEvent, ProviderResponse};
