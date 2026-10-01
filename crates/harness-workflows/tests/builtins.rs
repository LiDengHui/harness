//! The checked-in `workflows/` directory, validated as a whole.
//!
//! These are the files a user actually gets, so they are held to the same bar
//! as code: every one must load, every one must compile to a runnable graph,
//! and every `agent:` must name an agent that exists. A built-in that fails to
//! parse would otherwise only surface the first time someone tried to use it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use harness_workflows::{WorkflowRegistry, WorkflowSpec};

/// The repository root, two levels up from `crates/harness-workflows`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .to_path_buf()
}

fn built_ins() -> WorkflowRegistry {
    WorkflowRegistry::from_dirs(&[repo_root().join("workflows")])
        .expect("the checked-in workflows must load")
}

#[test]
fn every_built_in_loads_and_builds_a_graph() {
    let registry = built_ins();
    assert!(
        registry.len() >= 12,
        "expected at least 12 built-ins, found {}: {:?}",
        registry.len(),
        registry.names()
    );

    for spec in registry.list() {
        assert!(!spec.when.trim().is_empty(), "{}: empty `when`", spec.id);
        assert!(
            !spec.description.trim().is_empty(),
            "{}: empty description",
            spec.id
        );
        assert!(
            !spec.guidance.trim().is_empty(),
            "{}: empty guidance",
            spec.id
        );
        assert!(
            (2..=5).contains(&spec.stages.len()),
            "{}: {} stages, expected 2 to 5",
            spec.id,
            spec.stages.len()
        );

        let graph = spec
            .to_graph()
            .unwrap_or_else(|err| panic!("{} does not compile to a graph: {err}", spec.id));
        assert_eq!(
            graph.nodes.len(),
            spec.stages.len(),
            "{}: a stage was dropped",
            spec.id
        );
        assert!(
            graph.layers().is_ok(),
            "{}: the stages do not layer",
            spec.id
        );
    }
}

#[test]
fn the_expected_built_ins_are_present() {
    let registry = built_ins();
    let expected = [
        "add-tests",
        "api-contract-design",
        "bug-fix",
        "code-review",
        "dependency-upgrade",
        "docs-write",
        "feature-implement",
        "incident-triage",
        "migration-plan",
        "perf-investigate",
        "refactor",
        "release-prep",
        "repo-onboard",
        "security-audit",
    ];

    for id in expected {
        assert!(
            registry.get(id).is_some(),
            "missing built-in `{id}`; found {:?}",
            registry.names()
        );
    }
    assert!(registry.len() >= expected.len());
}

#[test]
fn each_built_in_has_a_distinct_when() {
    // `when` is the selector's only signal: two workflows sharing one would be
    // indistinguishable to it.
    let mut seen = BTreeSet::new();
    for spec in built_ins().list() {
        assert!(
            seen.insert(spec.when.clone()),
            "{} shares its `when` with another workflow",
            spec.id
        );
    }
}

#[test]
fn every_built_in_round_trips_through_save_new() {
    // The real files are the ones the serializer has to survive: multi-stage
    // graphs, nested `verify` sequences and Chinese prose.
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("workflows");
    let empty = WorkflowRegistry::default();
    let loaded = built_ins();

    for spec in loaded.list() {
        let path = empty
            .save_new(spec, &dir)
            .unwrap_or_else(|err| panic!("{}: {err}", spec.id));
        let text = std::fs::read_to_string(&path).expect("a readable file");
        let reloaded = WorkflowSpec::from_markdown(&text, path.clone(), &spec.id)
            .unwrap_or_else(|err| panic!("{}: {err}", spec.id));

        assert_eq!(reloaded.id, spec.id);
        assert_eq!(reloaded.name, spec.name, "{}: name changed", spec.id);
        assert_eq!(
            reloaded.description, spec.description,
            "{}: description changed",
            spec.id
        );
        assert_eq!(reloaded.when, spec.when, "{}: `when` changed", spec.id);
        assert_eq!(reloaded.stages, spec.stages, "{}: stages changed", spec.id);
        assert_eq!(
            reloaded.guidance, spec.guidance,
            "{}: guidance changed",
            spec.id
        );
    }
}

#[test]
fn stage_agents_are_declared_agents() {
    let root = repo_root();
    let mut known = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("agents")).expect("agents/ must exist") {
        let name = entry
            .expect("a readable entry")
            .file_name()
            .into_string()
            .expect("a utf-8 file name");
        if let Some(stem) = name.strip_suffix(".agent.md") {
            known.insert(stem.to_string());
        }
    }
    assert!(!known.is_empty(), "no agents were found");

    for spec in built_ins().list() {
        for stage in &spec.stages {
            if let Some(agent) = &stage.agent {
                assert!(
                    known.contains(agent),
                    "{}: stage `{}` names agent `{agent}`, which is not declared in agents/ ({known:?})",
                    spec.id,
                    stage.id
                );
            }
        }
    }
}
