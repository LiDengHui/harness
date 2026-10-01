//! The task DAG the planner produces and the executor schedules.
//!
//! A graph is validated before anything is scheduled, so a malformed plan costs
//! nothing: an unknown dependency, a duplicate slug or a cycle is refused here
//! rather than discovered halfway through a run.

use std::collections::{BTreeMap, BTreeSet};

use harness_core::{HarnessError, Result};
use serde::{Deserialize, Serialize};

/// One unit of work, as the planner assigns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskNode {
    /// Planner-assigned slug, e.g. `add-parser`.
    pub id: String,
    /// What this node must achieve.
    pub objective: String,
    /// `.agent.md` id to run it, when the planner names one.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Files this node claims, so two nodes in the same layer can be shown to
    /// conflict before either runs.
    #[serde(default)]
    pub files: Vec<String>,
    /// Shell-free commands that decide whether this node's result is
    /// acceptable, e.g. `["cargo", "test"]`. A workflow stage declares these;
    /// a node the planner produced leaves it empty, in which case the
    /// executor's own [`crate::verify::VerifierConfig`] checks apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verify: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskGraph {
    pub nodes: Vec<TaskNode>,
    /// Guidance for the run as a whole, when the graph came from a workflow.
    ///
    /// It lives on the graph rather than on each node because it is the
    /// procedure's preamble, not a stage's objective: every node of the run
    /// reads the same text. A free-form plan has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

impl TaskGraph {
    /// Strict: rejects a duplicate id, an empty objective, a dependency that
    /// names no node, and any cycle. Reports the offending id in the message.
    pub fn validate(&self) -> Result<()> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for node in &self.nodes {
            if node.id.trim().is_empty() {
                return Err(invalid("a node has an empty id"));
            }
            if node.objective.trim().is_empty() {
                return Err(invalid(format!(
                    "node `{}` has an empty objective",
                    node.id
                )));
            }
            if !seen.insert(node.id.as_str()) {
                return Err(invalid(format!("duplicate node id `{}`", node.id)));
            }
        }

        let ids: BTreeSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        for node in &self.nodes {
            for dep in &node.depends_on {
                if !ids.contains(dep.as_str()) {
                    return Err(invalid(format!(
                        "node `{}` depends on `{dep}`, which names no node",
                        node.id
                    )));
                }
            }
        }

        // A cycle is any node Kahn's algorithm cannot retire.
        let mut indegree: BTreeMap<&str, usize> = self
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.depends_on.len()))
            .collect();
        let mut ready: Vec<&str> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut retired = 0usize;

        while let Some(id) = ready.pop() {
            retired += 1;
            for node in &self.nodes {
                if node.depends_on.iter().any(|dep| dep == id) {
                    if let Some(degree) = indegree.get_mut(node.id.as_str()) {
                        *degree -= 1;
                        if *degree == 0 {
                            ready.push(node.id.as_str());
                        }
                    }
                }
            }
        }

        if retired != self.nodes.len() {
            let stuck = indegree
                .iter()
                .find(|(_, degree)| **degree > 0)
                .map(|(id, _)| *id)
                .unwrap_or("<unknown>");
            return Err(invalid(format!(
                "the graph contains a cycle through `{stuck}`"
            )));
        }

        Ok(())
    }

    /// Dependency layers: every node in layer N may run in parallel, and all of
    /// layer N-1 has finished. Deterministic order within a layer (by id).
    pub fn layers(&self) -> Result<Vec<Vec<&TaskNode>>> {
        self.validate()?;

        let mut indegree: BTreeMap<&str, usize> = self
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.depends_on.len()))
            .collect();
        let mut current: Vec<&str> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| *id)
            .collect();
        current.sort_unstable();

        let mut layers = Vec::new();
        while !current.is_empty() {
            let mut layer: Vec<&TaskNode> = current.iter().filter_map(|id| self.get(id)).collect();
            layer.sort_by(|a, b| a.id.cmp(&b.id));
            layers.push(layer);

            let mut next: Vec<&str> = Vec::new();
            for id in &current {
                for node in &self.nodes {
                    if node.depends_on.iter().any(|dep| dep == id) {
                        if let Some(degree) = indegree.get_mut(node.id.as_str()) {
                            *degree -= 1;
                            if *degree == 0 {
                                next.push(node.id.as_str());
                            }
                        }
                    }
                }
            }
            next.sort_unstable();
            current = next;
        }

        Ok(layers)
    }

    pub fn get(&self, id: &str) -> Option<&TaskNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Nodes in the same layer that claim the same file.
    ///
    /// A non-empty result is a scheduling hazard the caller can surface before
    /// spending any tokens. Each entry is `(file, first node id, second node id)`
    /// with the pair ordered by id. A graph that cannot be layered (a cycle) has
    /// no hazard to report, so it yields nothing here; `validate` is the check
    /// that refuses such a graph.
    pub fn file_conflicts(&self) -> Vec<(String, String, String)> {
        let Ok(layers) = self.layers() else {
            return Vec::new();
        };

        let mut conflicts = Vec::new();
        for layer in layers {
            let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
            for node in layer {
                for file in &node.files {
                    match owner.get(file.as_str()) {
                        Some(first) => {
                            conflicts.push((file.clone(), (*first).to_string(), node.id.clone()));
                        }
                        None => {
                            owner.insert(file.as_str(), node.id.as_str());
                        }
                    }
                }
            }
        }
        conflicts
    }
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::Other(format!("invalid task graph: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, deps: &[&str]) -> TaskNode {
        TaskNode {
            id: id.into(),
            objective: format!("do {id}"),
            agent: None,
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            files: Vec::new(),
            verify: Vec::new(),
        }
    }

    fn graph(nodes: Vec<TaskNode>) -> TaskGraph {
        TaskGraph {
            nodes,
            guidance: None,
        }
    }

    /// a -> b, a -> c, b + c -> d
    fn diamond() -> TaskGraph {
        graph(vec![
            node("a", &[]),
            node("b", &["a"]),
            node("c", &["a"]),
            node("d", &["b", "c"]),
        ])
    }

    #[test]
    fn a_well_formed_graph_validates() {
        assert!(diamond().validate().is_ok());
        assert!(graph(vec![]).validate().is_ok());
    }

    #[test]
    fn a_duplicate_id_is_rejected_and_named() {
        let err = graph(vec![node("dup", &[]), node("dup", &[])])
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("dup"), "{err}");
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn an_empty_objective_is_rejected_and_named() {
        let mut broken = node("hollow", &[]);
        broken.objective = "   ".into();
        let err = graph(vec![broken]).validate().unwrap_err();
        assert!(err.to_string().contains("hollow"), "{err}");
        assert!(err.to_string().contains("objective"), "{err}");
    }

    #[test]
    fn an_empty_id_is_rejected() {
        let mut broken = node("x", &[]);
        broken.id = "  ".into();
        let err = graph(vec![broken]).validate().unwrap_err();
        assert!(err.to_string().contains("empty id"), "{err}");
    }

    #[test]
    fn a_dangling_dependency_is_rejected_and_named() {
        let err = graph(vec![node("a", &["ghost"])]).validate().unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        assert!(err.to_string().contains('a'), "{err}");
    }

    #[test]
    fn a_self_dependency_is_a_cycle() {
        let err = graph(vec![node("a", &["a"])]).validate().unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
        assert!(err.to_string().contains('a'), "{err}");
    }

    #[test]
    fn a_cycle_is_rejected_and_named() {
        let err = graph(vec![
            node("a", &["c"]),
            node("b", &["a"]),
            node("c", &["b"]),
        ])
        .validate()
        .unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn layers_of_a_diamond_have_the_expected_shape() {
        let graph = diamond();
        let layers = graph.layers().unwrap();
        let ids: Vec<Vec<&str>> = layers
            .iter()
            .map(|layer| layer.iter().map(|n| n.id.as_str()).collect())
            .collect();
        assert_eq!(ids, vec![vec!["a"], vec!["b", "c"], vec!["d"]]);
    }

    #[test]
    fn a_layer_is_ordered_by_id_regardless_of_declaration_order() {
        let graph = graph(vec![node("z", &[]), node("m", &[]), node("a", &[])]);
        let layers = graph.layers().unwrap();
        let ids: Vec<&str> = layers[0].iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "m", "z"]);
    }

    #[test]
    fn independent_nodes_share_one_layer() {
        let graph = graph(vec![node("a", &[]), node("b", &[]), node("c", &["a", "b"])]);
        let layers = graph.layers().unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].len(), 2);
        assert_eq!(layers[1].len(), 1);
    }

    #[test]
    fn get_finds_a_node_by_id() {
        let graph = diamond();
        assert_eq!(graph.get("b").map(|n| n.id.as_str()), Some("b"));
        assert!(graph.get("nope").is_none());
    }

    #[test]
    fn a_file_claimed_twice_in_one_layer_is_a_conflict() {
        let mut b = node("b", &["a"]);
        b.files = vec!["src/lib.rs".into()];
        let mut c = node("c", &["a"]);
        c.files = vec!["src/lib.rs".into()];

        let graph = graph(vec![node("a", &[]), b, c]);
        assert_eq!(
            graph.file_conflicts(),
            vec![("src/lib.rs".to_string(), "b".to_string(), "c".to_string())]
        );
    }

    #[test]
    fn the_same_file_in_different_layers_is_not_a_conflict() {
        let mut a = node("a", &[]);
        a.files = vec!["src/lib.rs".into()];
        let mut b = node("b", &["a"]);
        b.files = vec!["src/lib.rs".into()];

        assert!(graph(vec![a, b]).file_conflicts().is_empty());
    }

    #[test]
    fn distinct_files_are_not_a_conflict() {
        let mut b = node("b", &["a"]);
        b.files = vec!["src/b.rs".into()];
        let mut c = node("c", &["a"]);
        c.files = vec!["src/c.rs".into()];

        assert!(graph(vec![node("a", &[]), b, c])
            .file_conflicts()
            .is_empty());
    }

    #[test]
    fn a_cyclic_graph_reports_no_conflicts_instead_of_failing() {
        let graph = graph(vec![node("a", &["b"]), node("b", &["a"])]);
        assert!(graph.file_conflicts().is_empty());
    }

    #[test]
    fn nodes_round_trip_through_json() {
        let graph = diamond();
        let encoded = serde_json::to_string(&graph).unwrap();
        let decoded: TaskGraph = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, graph);
    }

    #[test]
    fn optional_node_fields_may_be_omitted_by_the_planner() {
        let decoded: TaskGraph =
            serde_json::from_str(r#"{"nodes":[{"id":"a","objective":"do a"}]}"#).unwrap();
        assert_eq!(decoded.nodes[0].agent, None);
        assert!(decoded.nodes[0].depends_on.is_empty());
        assert!(decoded.nodes[0].files.is_empty());
        assert!(
            decoded.nodes[0].verify.is_empty(),
            "a planner node declares no checks"
        );
        assert_eq!(decoded.guidance, None);
    }

    #[test]
    fn a_workflow_node_carries_its_declared_checks_and_the_runs_guidance() {
        let mut with_checks = node("a", &[]);
        with_checks.verify = vec![vec!["cargo".into(), "test".into()]];
        let graph = TaskGraph {
            nodes: vec![with_checks],
            guidance: Some("Verify before you edit.".into()),
        };

        let json = serde_json::to_value(&graph).unwrap();
        assert_eq!(json["guidance"], "Verify before you edit.");
        assert_eq!(json["nodes"][0]["verify"][0][0], "cargo");

        let back: TaskGraph = serde_json::from_value(json).unwrap();
        assert_eq!(back, graph);
    }

    #[test]
    fn an_empty_check_list_and_absent_guidance_are_not_serialized() {
        // A planner-produced graph must not grow fields that mean nothing, or
        // every plan the model reads back would carry empty scaffolding.
        let json = serde_json::to_value(graph(vec![node("a", &[])])).unwrap();
        assert!(json.get("guidance").is_none(), "{json}");
        assert!(json["nodes"][0].get("verify").is_none(), "{json}");
    }
}
