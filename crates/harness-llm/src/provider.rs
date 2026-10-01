//! The one abstraction every backend implements.

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use harness_core::Result;

use crate::request::{ChatRequest, ProviderEvent, ProviderResponse};

/// A streaming chat backend.
///
/// `stream` is the only entry point: it pushes incremental events as they arrive
/// and returns the assembled turn. An implementation must not fail midway
/// without having emitted part of the answer unnoticed — callers decide what to
/// do with a partial turn, and a silently retried turn would duplicate output.
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;

    fn model(&self) -> &str;

    async fn stream(
        &self,
        request: ChatRequest,
        events: UnboundedSender<ProviderEvent>,
    ) -> Result<ProviderResponse>;
}
