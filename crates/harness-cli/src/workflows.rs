//! The adapter that connects `harness-workflows` to the orchestrator's seam.
//!
//! [`harness_orchestrator::WorkflowSelector`] is the contract the server codes
//! against; it exists so the orchestrator and the server stay buildable whether
//! or not the workflow registry is present. The registry cannot implement it
//! directly: `harness-workflows` depends on `harness-orchestrator`, so the
//! adapter has to live on this side of that edge. It is deliberately tiny —
//! resolve, compile to a graph, hand back — because the interesting policy
//! (which workflow fits) is the registry's, not this crate's.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harness_agent::AgentRegistry;
use harness_core::Result;
use harness_llm::Provider;
use harness_orchestrator::{SelectedWorkflow, WorkflowSelector};
use harness_workflows::WorkflowRegistry;

/// Chooses a workflow from a [`WorkflowRegistry`].
pub(crate) struct RegistrySelector {
    registry: Arc<WorkflowRegistry>,
}

impl RegistrySelector {
    pub(crate) fn new(registry: Arc<WorkflowRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl WorkflowSelector for RegistrySelector {
    async fn select(
        &self,
        task: &str,
        provider: &dyn Provider,
        model: &str,
        requested: Option<&str>,
    ) -> Result<Option<SelectedWorkflow>> {
        // A named id is resolved without a model call: the client already made
        // the choice, and asking the selector to confirm it would spend tokens
        // to re-derive an answer that is already known. A named id that
        // resolves to nothing returns `Ok(None)`, which the caller reports as
        // "no such workflow" rather than as "nothing fits".
        let spec = match requested {
            Some(id) => self.registry.get(id),
            None => self.registry.select(task, provider, model).await?,
        };

        let Some(spec) = spec else {
            return Ok(None);
        };
        let graph = spec.to_graph()?;
        tracing::debug!(
            workflow = %spec.id,
            nodes = graph.nodes.len(),
            "workflow selected"
        );
        Ok(Some(SelectedWorkflow {
            id: spec.id.clone(),
            graph,
        }))
    }
}

/// Loads the workspace's workflows, naming a malformed one or a missing agent.
///
/// The load is fallible because a half-read procedure is worse than a refused
/// one: a malformed file fails the whole load and names its path, exactly as it
/// does for agents and skills. The agent registry is loaded first so a stage
/// naming an agent nobody defines is refused here too — the same fatal check
/// `harness-agent` makes for a dangling sub-agent reference, and the reason a
/// workflow that could never be staffed cannot be saved through the API. The
/// caller decides whether the failure is fatal; for a server it is, because a
/// registry that cannot be read is not an empty one.
pub(crate) fn load_registry(workspace_root: &Path) -> Result<Arc<WorkflowRegistry>> {
    let agents = AgentRegistry::load(workspace_root)?;
    let known: BTreeSet<String> = agents.list().iter().map(|spec| spec.id.clone()).collect();
    Ok(Arc::new(WorkflowRegistry::load_with_agents(
        workspace_root,
        known,
    )?))
}
