//! The seam through which a workflow is chosen for a task.
//!
//! The registry that actually holds workflows lives in a separate crate
//! (`harness-workflows`). It is not a dependency here — this trait is the
//! contract the server codes against, so the orchestrator and the server stay
//! buildable whether or not that crate is present.
//!
//! The adapter over that crate is a few lines: its
//! `WorkflowRegistry::load(root)` produces a registry whose
//! `select(task, provider, model).await -> Result<Option<&WorkflowSpec>>` is the
//! contract this trait mirrors, and `WorkflowSpec::to_graph()` is what fills
//! [`SelectedWorkflow::graph`]. Dropping this trait's `requested` parameter
//! leaves exactly the documented `select` signature.

use async_trait::async_trait;
use harness_core::Result;
use harness_llm::Provider;

use crate::graph::TaskGraph;

/// A workflow chosen for a task: its id and the graph it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedWorkflow {
    /// The workflow's id, carried to the client as `PlanCreated::workflow_id`.
    pub id: String,
    /// The graph the workflow expands to. Already validated by the caller.
    pub graph: TaskGraph,
}

/// Chooses the workflow a task should run, when the workspace declares any.
///
/// `Ok(None)` means no workflow fits and the caller should fall back to the
/// free-form planner. An `Err` is a broken registry rather than an empty one and
/// is reported to the client, because the client asked for a workflow and a
/// registry that cannot be read is not the same answer as one that holds none.
#[async_trait]
pub trait WorkflowSelector: Send + Sync {
    /// The workflow to run for `task`.
    ///
    /// `requested` is the id the client named, when it named one; `None` asks the
    /// selector to choose. A named id that resolves to nothing returns `Ok(None)`
    /// too — the caller distinguishes "choose" from "named" and reports the two
    /// cases differently.
    ///
    /// The signature deliberately mirrors `harness-workflows`'s
    /// `WorkflowRegistry::select(task, provider, model)`: an implementation over
    /// that registry forwards `task`, `provider` and `model` straight through,
    /// resolves a named id with `WorkflowRegistry::get`, and fills
    /// [`SelectedWorkflow::graph`] from `WorkflowSpec::to_graph`.
    async fn select(
        &self,
        task: &str,
        provider: &dyn Provider,
        model: &str,
        requested: Option<&str>,
    ) -> Result<Option<SelectedWorkflow>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::TaskNode;

    struct Fixed {
        spec: Option<SelectedWorkflow>,
    }

    #[async_trait]
    impl WorkflowSelector for Fixed {
        async fn select(
            &self,
            _task: &str,
            _provider: &dyn Provider,
            _model: &str,
            _requested: Option<&str>,
        ) -> Result<Option<SelectedWorkflow>> {
            Ok(self.spec.clone())
        }
    }

    fn graph() -> TaskGraph {
        TaskGraph {
            nodes: vec![TaskNode {
                id: "a".into(),
                objective: "do a".into(),
                agent: None,
                depends_on: Vec::new(),
                files: Vec::new(),
                verify: Vec::new(),
            }],
            guidance: None,
        }
    }

    #[tokio::test]
    async fn a_selector_can_answer_with_a_workflow_or_with_nothing() {
        let provider = harness_llm::MockProvider::new("mock", "mock-1", Vec::new());
        let chosen: Box<dyn WorkflowSelector> = Box::new(Fixed {
            spec: Some(SelectedWorkflow {
                id: "feature-build".into(),
                graph: graph(),
            }),
        });
        let selected = chosen
            .select("build a parser", &provider, "mock-1", None)
            .await
            .expect("the selector answers");
        assert_eq!(
            selected.as_ref().map(|workflow| workflow.id.as_str()),
            Some("feature-build")
        );

        let empty: Box<dyn WorkflowSelector> = Box::new(Fixed { spec: None });
        assert!(empty
            .select("build a parser", &provider, "mock-1", None)
            .await
            .expect("the selector answers")
            .is_none());
    }
}
