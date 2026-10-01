//! Named, reusable procedures: a task can be run through a workflow.
//!
//! A workflow is the third of three declarative layers, and the differences are
//! what make it worth its own crate:
//!
//! - an *agent* (`harness-agent`) says **who** does the work and what it may
//!   touch;
//! - a *skill* (`harness-skills`) is **prompt content** injected while active;
//! - a *plan* (`harness-orchestrator::Planner`) is generated **per task** and
//!   thrown away;
//! - a *workflow* is a **reusable stage graph** — a named procedure that is
//!   written once, chosen for a task, and then handed to the existing executor.
//!
//! That is why [`WorkflowSpec::to_graph`] exists: a workflow does not bring its
//! own scheduler, it compiles into the [`TaskGraph`] the `Executor` already
//! runs. The per-stage [`WorkflowStage::verify`] commands are kept on the spec
//! rather than on the graph because `TaskNode` has no such field — the caller
//! wires them into a `VerifierConfig`.
//!
//! Discovery mirrors `harness-agent` and `harness-skills`: a global root, then
//! the checked-in workspace root, then the project's runtime state directory,
//! with later roots overriding earlier ones by id. A missing directory is fine;
//! a malformed file fails the whole load and names its path, because a
//! half-read procedure is worse than a refused one.
//!
//! Selection ([`WorkflowRegistry::select`]) is deliberately cheap: the model
//! sees only each workflow's `id`, `name`, `when` and `description`, never the
//! stages or the guidance. That is the same progressive-disclosure split the
//! skill registry makes between metadata and body.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use harness_core::{frontmatter, global_dir, project_dir, HarnessError, Result};
use harness_orchestrator::{TaskGraph, TaskNode};
use serde::{Deserialize, Serialize};

mod select;

pub use select::SELECT_SYSTEM_PROMPT;

/// Suffix that marks a document as a workflow definition.
const WORKFLOW_SUFFIX: &str = ".workflow.md";

/// The declarative frontmatter of a `<id>.workflow.md`, before defaults.
///
/// Separate from [`WorkflowSpec`] because the body is the guidance rather than
/// a frontmatter field, and because `id`/`name` fall back to the file name.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct Frontmatter {
    id: String,
    name: String,
    description: String,
    /// When this workflow fits. The selector reads this and nothing else.
    when: String,
    stages: Vec<WorkflowStage>,
}

/// One stage of a workflow: a node of the graph plus how to check it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowStage {
    /// Slug, unique within the workflow, e.g. `implement`.
    pub id: String,
    /// What this stage must achieve, and what "done" means.
    pub objective: String,
    /// `.agent.md` id to run it, when the workflow names one.
    pub agent: Option<String>,
    /// Stage ids that must finish first. Must be acyclic and name real stages.
    pub depends_on: Vec<String>,
    /// Shell-free verification the orchestrator can run, e.g.
    /// `["cargo", "test"]`. Each entry is one command.
    pub verify: Vec<Vec<String>>,
}

/// One workflow as declared by a `<id>.workflow.md` file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    /// When this workflow fits. The selector reads this and nothing else, so it
    /// is the one field that decides whether the workflow is ever chosen.
    pub when: String,
    pub stages: Vec<WorkflowStage>,
    /// Markdown body: guidance for the run as a whole.
    pub guidance: String,
    pub source_path: PathBuf,
}

impl WorkflowSpec {
    /// Reads a document into a validated spec.
    ///
    /// `fallback_id` is the file stem (`<id>.workflow.md` → `<id>`), so a
    /// document that omits `id` still gets a stable handle. `when` and
    /// `description` are required rather than defaulted: together they are the
    /// only text the selector sees, so a workflow missing either can never be
    /// chosen. The stages are checked as a DAG here, with the file's path in the
    /// error, so a broken built-in is refused at load rather than at run time.
    pub fn from_markdown(text: &str, path: PathBuf, fallback_id: &str) -> Result<Self> {
        let (fields, body) = frontmatter::parse::<Frontmatter>(text)
            .map_err(|err| invalid(&path, &format!("frontmatter is not valid: {err}")))?;

        if body.is_empty() {
            return Err(invalid(
                &path,
                "the Markdown body is empty, so the workflow has no guidance",
            ));
        }

        let id = pick(fields.id, fallback_id);
        if id.is_empty() {
            return Err(invalid(
                &path,
                "no id: name the file `<id>.workflow.md`, or set `id` in the frontmatter",
            ));
        }
        let name = pick(fields.name, &id);

        let description = fields.description.trim();
        if description.is_empty() {
            return Err(invalid(
                &path,
                "no description: it is one of the two texts the selector reads when choosing",
            ));
        }

        let when = fields.when.trim();
        if when.is_empty() {
            return Err(invalid(
                &path,
                "no `when`: it is the only field that decides whether the workflow is ever chosen",
            ));
        }

        let stages = normalize_stages(&path, fields.stages)?;
        if stages.is_empty() {
            return Err(invalid(
                &path,
                "no stages: a workflow with nothing to run is not a procedure",
            ));
        }
        // The DAG rules are the executor's, so they are not re-implemented:
        // duplicate ids, empty objectives, dangling dependencies and cycles are
        // all refused by the same validator that will refuse them later.
        stage_graph(&stages, None)
            .validate()
            .map_err(|err| invalid(&path, &format!("the stages are not a valid graph: {err}")))?;

        Ok(Self {
            id,
            name,
            description: description.to_string(),
            when: when.to_string(),
            stages,
            guidance: body,
            source_path: path,
        })
    }

    /// The workflow's stages as a [`TaskGraph`], so an existing `Executor` can
    /// run it.
    ///
    /// Both pieces of the spec that only matter at run time travel with the
    /// graph: each stage's `verify` commands land on its [`TaskNode`], and the
    /// `guidance` body becomes the graph's. They are kept on the spec rather
    /// than on the graph's type because a workflow is a document, but the
    /// executor can only see a graph — dropping them here is what made both
    /// features decorative.
    pub fn to_graph(&self) -> Result<TaskGraph> {
        let graph = stage_graph(&self.stages, Some(self.guidance.as_str()));
        graph.validate()?;
        Ok(graph)
    }

    /// Refuses a stage whose `agent:` names something no `.agent.md` defines.
    ///
    /// `known` is passed in rather than looked up here because
    /// `harness-workflows` deliberately does not depend on `harness-agent` —
    /// the same reason [`WorkflowRegistry::load`] takes a workspace root instead
    /// of an agent registry. The error names both the stage and the missing
    /// agent, so a refused workflow says which line to fix.
    ///
    /// A stage with no `agent:` is fine: the executor resolves the configured
    /// default for it, exactly as it does for a node the planner built.
    pub fn validate_agents(&self, known: &BTreeSet<String>) -> Result<()> {
        for stage in &self.stages {
            let Some(agent) = stage.agent.as_deref() else {
                continue;
            };
            if known.contains(agent) {
                continue;
            }
            let available = if known.is_empty() {
                "(none)".to_string()
            } else {
                known.iter().cloned().collect::<Vec<_>>().join(", ")
            };
            return Err(invalid(
                &self.source_path,
                &format!(
                    "stage `{}` names agent `{agent}`, which no `.agent.md` defines; \
                     available: {available}",
                    stage.id
                ),
            ));
        }
        Ok(())
    }
}

/// A workflow's stages as graph nodes. Callers validate the result.
///
/// `guidance` is `Some` when the graph is about to be run and `None` when the
/// stages are only being validated, where the text would be discarded anyway.
fn stage_graph(stages: &[WorkflowStage], guidance: Option<&str>) -> TaskGraph {
    TaskGraph {
        nodes: stages
            .iter()
            .map(|stage| TaskNode {
                id: stage.id.clone(),
                objective: stage.objective.clone(),
                agent: stage.agent.clone(),
                depends_on: stage.depends_on.clone(),
                // A workflow claims no files: the planner's conflict check is
                // about parallel nodes racing on the same path, and a stage
                // here has no file list to declare.
                files: Vec::new(),
                verify: stage.verify.clone(),
            })
            .collect(),
        guidance: guidance.map(str::to_string),
    }
}

/// Trims every field so that a saved file reads back identically.
fn normalize_stages(path: &Path, stages: Vec<WorkflowStage>) -> Result<Vec<WorkflowStage>> {
    let mut normalized = Vec::with_capacity(stages.len());

    for stage in stages {
        let mut verify = Vec::with_capacity(stage.verify.len());
        for command in stage.verify {
            let command: Vec<String> = command
                .into_iter()
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect();
            if command.is_empty() {
                return Err(invalid(
                    path,
                    "a `verify` entry is empty: each one is a command, e.g. [\"cargo\", \"test\"]",
                ));
            }
            verify.push(command);
        }

        normalized.push(WorkflowStage {
            id: stage.id.trim().to_string(),
            objective: stage.objective.trim().to_string(),
            agent: trimmed(stage.agent),
            depends_on: stage
                .depends_on
                .into_iter()
                .map(|dep| dep.trim().to_string())
                .filter(|dep| !dep.is_empty())
                .collect(),
            verify,
        });
    }

    Ok(normalized)
}

/// Every workflow discoverable from a set of directories.
#[derive(Debug, Clone, Default)]
pub struct WorkflowRegistry {
    by_id: BTreeMap<String, WorkflowSpec>,
    /// Agent ids a stage may name, when the caller installed an agent registry.
    ///
    /// `None` means "no agent registry was available", so `agent:` references
    /// are left unchecked — the structural validation still runs. `Some` (even
    /// empty) turns a reference to an unknown agent into a load/save error,
    /// which is what `harness-agent` already does for `subagents`. The split
    /// exists because this crate cannot depend on `harness-agent`, so the ids
    /// have to be handed in.
    known_agents: Option<BTreeSet<String>>,
}

impl WorkflowRegistry {
    /// Directories consulted by [`WorkflowRegistry::load`], lowest precedence first.
    ///
    /// The checked-in `<workspace>/workflows` comes before
    /// `<workspace>/.harness/workflows`, exactly as with agents and skills, so a
    /// project can shadow a repository-level procedure without editing it.
    pub fn search_dirs(workspace_root: &Path) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(dir) = global_dir() {
            dirs.push(dir.join("workflows"));
        }
        dirs.push(workspace_root.join("workflows"));
        dirs.push(project_dir(workspace_root).join("workflows"));
        dirs
    }

    /// Loads `~/.harness/workflows/`, then `<workspace>/workflows/` and
    /// `<workspace>/.harness/workflows/`, so a project-level file overrides a
    /// global one with the same id.
    pub fn load(workspace_root: &Path) -> Result<Self> {
        Self::from_dirs(&Self::search_dirs(workspace_root))
    }

    /// Loads from explicit directories, later directories winning.
    pub fn from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let mut by_id = BTreeMap::new();

        for dir in dirs {
            for (path, fallback_id) in workflow_files(dir)? {
                let markdown = std::fs::read_to_string(&path)
                    .map_err(|err| invalid(&path, &format!("cannot be read: {err}")))?;
                let spec = WorkflowSpec::from_markdown(&markdown, path, &fallback_id)?;
                by_id.insert(spec.id.clone(), spec);
            }
        }

        Ok(Self {
            by_id,
            known_agents: None,
        })
    }

    /// Loads the workspace's workflows and checks every `agent:` against
    /// `agents`, refusing the whole load when one is missing.
    ///
    /// Fatal on purpose, matching the agent registry's own strictness: a
    /// workflow whose stage can never be staffed is worse than no workflow,
    /// because it looks runnable right up to the moment it is chosen. The
    /// caller supplies the ids (usually from `AgentRegistry::load`) so this
    /// crate does not have to depend on `harness-agent`.
    pub fn load_with_agents(
        workspace_root: &Path,
        agents: impl IntoIterator<Item = String>,
    ) -> Result<Self> {
        let registry =
            Self::from_dirs(&Self::search_dirs(workspace_root))?.with_known_agents(agents);
        registry.validate_agents()?;
        Ok(registry)
    }

    /// Installs the agent ids that `agent:` references are checked against.
    pub fn with_known_agents(mut self, agents: impl IntoIterator<Item = String>) -> Self {
        self.known_agents = Some(agents.into_iter().collect());
        self
    }

    /// Checks every workflow's stage agents, when an agent set was installed.
    ///
    /// A no-op for a registry built without [`WorkflowRegistry::with_known_agents`],
    /// so the loader's existing callers keep working.
    pub fn validate_agents(&self) -> Result<()> {
        let Some(known) = &self.known_agents else {
            return Ok(());
        };
        for spec in self.by_id.values() {
            spec.validate_agents(known)?;
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&WorkflowSpec> {
        self.by_id.get(id)
    }

    /// Sorted by id.
    pub fn list(&self) -> Vec<&WorkflowSpec> {
        self.by_id.values().collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.by_id.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Writes `spec` as a valid `<id>.workflow.md` under `dir` and returns the
    /// path.
    ///
    /// The document is rendered, then read back through
    /// [`WorkflowSpec::from_markdown`] *before* anything is written, so a spec
    /// that could not be loaded can never reach disk. An existing file is not
    /// overwritten: the caller asked for a new workflow, and silently replacing
    /// a checked-in built-in would be the kind of edit that is hard to notice,
    /// let alone undo.
    pub fn save_new(&self, spec: &WorkflowSpec, dir: &Path) -> Result<PathBuf> {
        let id = spec.id.trim();
        if id.is_empty() {
            return Err(invalid(
                &dir.join(WORKFLOW_SUFFIX),
                "no id: a workflow needs one to be addressable",
            ));
        }

        let path = dir.join(format!("{id}{WORKFLOW_SUFFIX}"));
        if path.exists() {
            return Err(invalid(
                &path,
                "a workflow with this id already exists; choose another id or remove the file",
            ));
        }

        let document = render(spec)?;
        // The round trip is not a test-only concern: it is the check that the
        // bytes about to be written are a loadable workflow.
        let parsed = WorkflowSpec::from_markdown(&document, path.clone(), id)?;
        // And when an agent registry was installed, the same check the loader
        // makes: a workflow that names an agent nobody defines would otherwise
        // be saved happily and fail only when someone tried to run it.
        if let Some(known) = &self.known_agents {
            parsed.validate_agents(known)?;
        }

        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, document)?;
        Ok(path)
    }
}

/// Renders a spec as a `<id>.workflow.md` document.
fn render(spec: &WorkflowSpec) -> Result<String> {
    let id = spec.id.trim();
    let frontmatter = Frontmatter {
        id: id.to_string(),
        name: pick(spec.name.clone(), id),
        description: spec.description.trim().to_string(),
        when: spec.when.trim().to_string(),
        stages: spec.stages.clone(),
    };

    let yaml = serde_norway::to_string(&frontmatter).map_err(|err| {
        HarnessError::Config(format!("the workflow could not be serialized: {err}"))
    })?;

    Ok(format!("---\n{yaml}---\n\n{}\n", spec.guidance.trim()))
}

/// Finds the workflow files directly under `dir`, as `(path, fallback id)`.
///
/// Only `<name>.workflow.md` counts. Unlike the skill registry, which accepts a
/// bare `.md`, the suffix is required here so that a `README.md` or a design
/// note dropped into `workflows/` is documentation rather than a broken
/// procedure. Not searched recursively: a `.workflow.md` in a nested directory
/// is not discovered.
fn workflow_files(dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    let entries = match std::fs::read_dir(dir) {
        // An absent directory just means nothing is declared there.
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(HarnessError::Io(err)),
    };

    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(WORKFLOW_SUFFIX) else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        // Own the stem before moving `path`, which it borrows from.
        let stem = stem.to_string();
        found.push((path, stem));
    }

    // `read_dir` order is filesystem-dependent; sorting keeps override
    // behaviour and error reporting stable.
    found.sort();
    Ok(found)
}

fn invalid(path: &Path, message: &str) -> HarnessError {
    // `harness-core` has no workflow-specific variant, and a malformed
    // declarative file is what a configuration error describes. The path is in
    // the message because that is what a refused load has to name; reusing
    // `InvalidAgentSpec` would have said "agent spec" about a workflow.
    HarnessError::Config(format!(
        "invalid workflow spec `{}`: {message}",
        path.display()
    ))
}

fn pick(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn trimmed(value: Option<String>) -> Option<String> {
    let value = value?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
---
id: sample
name: Sample Procedure
description: A procedure used by the tests.
when: When a test needs a workflow.
stages:
  - id: first
    objective: Do the first thing.
    agent: default
    verify:
      - [\"cargo\", \"check\"]
  - id: second
    objective: Do the second thing.
    depends_on:
      - first
---

Run the stages in order.
";

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn parse(markdown: &str, fallback_id: &str) -> Result<WorkflowSpec> {
        WorkflowSpec::from_markdown(
            markdown,
            PathBuf::from("workflows/test.workflow.md"),
            fallback_id,
        )
    }

    fn stage(id: &str, deps: &[&str]) -> WorkflowStage {
        WorkflowStage {
            id: id.into(),
            objective: format!("do {id}"),
            agent: None,
            depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
            verify: Vec::new(),
        }
    }

    fn registry_of(specs: Vec<WorkflowSpec>) -> WorkflowRegistry {
        WorkflowRegistry {
            by_id: specs.into_iter().map(|s| (s.id.clone(), s)).collect(),
            known_agents: None,
        }
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let registry = WorkflowRegistry::from_dirs(&[PathBuf::from("does/not/exist")]).unwrap();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.get("anything").is_none());
        assert!(registry.list().is_empty());
        assert!(registry.names().is_empty());
    }

    #[test]
    fn a_full_document_parses_into_a_spec() {
        let spec = parse(SAMPLE, "ignored").unwrap();

        assert_eq!(spec.id, "sample");
        assert_eq!(spec.name, "Sample Procedure");
        assert_eq!(spec.description, "A procedure used by the tests.");
        assert_eq!(spec.when, "When a test needs a workflow.");
        assert_eq!(spec.guidance, "Run the stages in order.");
        assert!(spec.source_path.ends_with("test.workflow.md"));

        assert_eq!(spec.stages.len(), 2);
        assert_eq!(spec.stages[0].id, "first");
        assert_eq!(spec.stages[0].agent.as_deref(), Some("default"));
        assert_eq!(spec.stages[0].verify, vec![vec!["cargo", "check"]]);
        assert!(spec.stages[0].depends_on.is_empty());
        assert_eq!(spec.stages[1].depends_on, vec!["first"]);
        assert_eq!(spec.stages[1].agent, None);
        assert!(spec.stages[1].verify.is_empty());
    }

    #[test]
    fn id_and_name_default_to_the_file_name() {
        let spec = parse(
            "---\ndescription: d\nwhen: w\nstages:\n  - id: a\n    objective: do a\n---\n\nBody.\n",
            "from-file",
        )
        .unwrap();
        assert_eq!(spec.id, "from-file");
        assert_eq!(spec.name, "from-file");
    }

    #[test]
    fn an_explicit_id_wins_over_the_file_name() {
        let spec = parse(
            "---\nid: declared\ndescription: d\nwhen: w\nstages:\n  - id: a\n    objective: do a\n---\n\nBody.\n",
            "from-file",
        )
        .unwrap();
        assert_eq!(spec.id, "declared");
        assert_eq!(spec.name, "declared", "name falls back to the resolved id");
    }

    #[test]
    fn whitespace_is_trimmed_everywhere() {
        let markdown = "\
---
id:  spaced
name: '  Spaced Name  '
description: '  described  '
when: '  when it fits  '
stages:
  - id: '  a  '
    objective: '  do a  '
    agent: '  default  '
    depends_on: ['  b  ', '   ']
    verify: [['  cargo  ', '  test  ']]
  - id: b
    objective: do b
---

  Body text.
";
        let spec = parse(markdown, "ignored").unwrap();
        assert_eq!(spec.id, "spaced");
        assert_eq!(spec.name, "Spaced Name");
        assert_eq!(spec.description, "described");
        assert_eq!(spec.when, "when it fits");
        assert_eq!(spec.guidance, "Body text.");
        assert_eq!(spec.stages[0].id, "a");
        assert_eq!(spec.stages[0].objective, "do a");
        assert_eq!(spec.stages[0].agent.as_deref(), Some("default"));
        assert_eq!(
            spec.stages[0].depends_on,
            vec!["b"],
            "blank deps are dropped"
        );
        assert_eq!(spec.stages[0].verify, vec![vec!["cargo", "test"]]);
    }

    #[test]
    fn a_malformed_file_fails_the_whole_load_with_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        write(
            &dir,
            "good.workflow.md",
            "---\ndescription: fine\nwhen: w\nstages:\n  - id: a\n    objective: do a\n---\n\nBody.\n",
        );
        write(
            &dir,
            "bad.workflow.md",
            "---\ndescription: [unclosed\n---\n\nBody.\n",
        );

        let err = WorkflowRegistry::from_dirs(std::slice::from_ref(&dir)).unwrap_err();
        assert!(matches!(err, HarnessError::Config(_)), "{err}");
        assert!(err.to_string().contains("bad.workflow.md"), "{err}");
    }

    #[test]
    fn a_document_missing_when_description_or_body_fails_the_load() {
        let cases = [
            (
                "no-when",
                "---\ndescription: d\nstages:\n  - id: a\n    objective: x\n---\n\nBody.\n",
                "when",
            ),
            (
                "no-description",
                "---\nwhen: w\nstages:\n  - id: a\n    objective: x\n---\n\nBody.\n",
                "description",
            ),
            (
                "no-body",
                "---\ndescription: d\nwhen: w\nstages:\n  - id: a\n    objective: x\n---\n\n   \n",
                "guidance",
            ),
            (
                "no-stages",
                "---\ndescription: d\nwhen: w\n---\n\nBody.\n",
                "stages",
            ),
        ];

        for (id, markdown, needle) in cases {
            let err = parse(markdown, id).unwrap_err();
            assert!(err.to_string().contains(needle), "{id}: {err}");
        }
    }

    #[test]
    fn a_stage_dependency_naming_no_stage_is_rejected_and_named() {
        let markdown = "\
---
description: d
when: w
stages:
  - id: a
    objective: do a
    depends_on: [ghost]
---

Body.
";
        let err = parse(markdown, "dangling").unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        assert!(err.to_string().contains("test.workflow.md"), "{err}");
    }

    #[test]
    fn a_cycle_between_stages_is_rejected() {
        let markdown = "\
---
description: d
when: w
stages:
  - id: a
    objective: do a
    depends_on: [c]
  - id: b
    objective: do b
    depends_on: [a]
  - id: c
    objective: do c
    depends_on: [b]
---

Body.
";
        let err = parse(markdown, "cyclic").unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn a_duplicate_stage_id_is_rejected() {
        let markdown = "\
---
description: d
when: w
stages:
  - id: a
    objective: one
  - id: a
    objective: two
---

Body.
";
        let err = parse(markdown, "duplicate").unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn an_empty_verify_command_is_rejected() {
        let markdown = "\
---
description: d
when: w
stages:
  - id: a
    objective: do a
    verify:
      - []
---

Body.
";
        let err = parse(markdown, "empty-verify").unwrap_err();
        assert!(err.to_string().contains("verify"), "{err}");
    }

    #[test]
    fn to_graph_produces_a_valid_task_graph() {
        let spec = parse(SAMPLE, "ignored").unwrap();
        let graph = spec.to_graph().unwrap();

        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.get("first").unwrap().objective, "Do the first thing.");
        assert_eq!(
            graph.get("first").unwrap().agent.as_deref(),
            Some("default")
        );
        assert_eq!(graph.get("second").unwrap().depends_on, vec!["first"]);
        // The two run-time-only pieces of the spec travel with the graph: the
        // stage's checks and the run's guidance. Dropping them is what made
        // both features decorative.
        assert_eq!(
            graph.get("first").unwrap().verify,
            vec![vec!["cargo".to_string(), "check".to_string()]],
            "the stage's verify commands must reach its node"
        );
        assert!(
            graph.get("second").unwrap().verify.is_empty(),
            "a stage that declares no check must carry none"
        );
        assert_eq!(
            graph.guidance.as_deref(),
            Some("Run the stages in order."),
            "the workflow's guidance must reach the graph"
        );

        let layers = graph.layers().unwrap();
        let ids: Vec<Vec<&str>> = layers
            .iter()
            .map(|layer| layer.iter().map(|node| node.id.as_str()).collect())
            .collect();
        assert_eq!(ids, vec![vec!["first"], vec!["second"]]);
    }

    #[test]
    fn to_graph_refuses_a_spec_built_by_hand_with_a_broken_stage() {
        let spec = WorkflowSpec {
            id: "hand-built".into(),
            name: "Hand built".into(),
            description: "d".into(),
            when: "w".into(),
            stages: vec![stage("a", &["ghost"])],
            guidance: "Body.".into(),
            source_path: PathBuf::from("nowhere"),
        };
        assert!(spec.to_graph().is_err());
    }

    #[test]
    fn a_later_directory_overrides_an_earlier_one() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join("global");
        let project = tmp.path().join("project");
        let other = tmp.path().join("other");

        let body = |id: &str, name: &str| {
            format!(
                "---\nid: {id}\nname: {name}\ndescription: d\nwhen: w\nstages:\n  - id: a\n    objective: do a\n---\n\nBody.\n"
            )
        };

        write(&global, "shared.workflow.md", &body("shared", "Global"));
        write(
            &global,
            "only-global.workflow.md",
            &body("only-global", "G"),
        );
        write(&project, "shared.workflow.md", &body("shared", "Project"));
        write(&other, "only-other.workflow.md", &body("only-other", "O"));

        let registry = WorkflowRegistry::from_dirs(&[global, project.clone(), other]).unwrap();

        assert_eq!(
            registry.names(),
            vec!["only-global", "only-other", "shared"]
        );
        let winner = registry.get("shared").unwrap();
        assert_eq!(winner.name, "Project");
        assert!(winner.source_path.starts_with(&project));
    }

    #[test]
    fn a_file_without_the_suffix_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        write(
            &dir,
            "notes.md",
            "---\ndescription: not a workflow\n---\n\nJust prose.\n",
        );
        write(&dir, "README.md", "Documentation, not a procedure.\n");

        let registry = WorkflowRegistry::from_dirs(&[dir]).unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn search_dirs_lists_the_three_roots_in_precedence_order() {
        let root = Path::new("/work/project");
        let dirs = WorkflowRegistry::search_dirs(root);

        // The global root is optional (it needs a home directory), so it is
        // asserted only when present; the two workspace roots are always there
        // and always in this order.
        assert!(dirs.len() == 2 || dirs.len() == 3, "{dirs:?}");
        assert_eq!(
            &dirs[dirs.len() - 2..],
            [root.join("workflows"), root.join(".harness/workflows")],
            "the checked-in root comes before the project state directory: {dirs:?}"
        );
        assert!(
            dirs.iter().all(|dir| dir.ends_with("workflows")),
            "{dirs:?}"
        );
    }

    #[test]
    fn save_new_round_trips_through_from_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        let registry = WorkflowRegistry::default();

        let original = parse(SAMPLE, "ignored").unwrap();
        let path = registry.save_new(&original, &dir).unwrap();

        assert!(path.ends_with("sample.workflow.md"), "{path:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---\n"), "{text}");
        assert!(text.contains("stages:"), "{text}");

        let reloaded = WorkflowSpec::from_markdown(&text, path.clone(), "sample").unwrap();
        assert_eq!(reloaded.id, original.id);
        assert_eq!(reloaded.name, original.name);
        assert_eq!(reloaded.description, original.description);
        assert_eq!(reloaded.when, original.when);
        assert_eq!(reloaded.stages, original.stages);
        assert_eq!(reloaded.guidance, original.guidance);
        assert_eq!(reloaded.source_path, path);
    }

    #[test]
    fn save_new_writes_a_file_a_registry_can_load() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        let spec = parse(SAMPLE, "ignored").unwrap();
        WorkflowRegistry::default().save_new(&spec, &dir).unwrap();

        let registry = WorkflowRegistry::from_dirs(&[dir]).unwrap();
        assert_eq!(registry.names(), vec!["sample"]);
        assert!(registry.get("sample").unwrap().to_graph().is_ok());
    }

    #[test]
    fn save_new_refuses_to_overwrite_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        write(
            &dir,
            "sample.workflow.md",
            "---\ndescription: existing\n---\n\nBody.\n",
        );

        let spec = parse(SAMPLE, "ignored").unwrap();
        let err = WorkflowRegistry::default()
            .save_new(&spec, &dir)
            .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        let untouched = std::fs::read_to_string(dir.join("sample.workflow.md")).unwrap();
        assert!(untouched.contains("existing"), "{untouched}");
    }

    #[test]
    fn save_new_refuses_an_invalid_spec_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");

        let mut spec = parse(SAMPLE, "ignored").unwrap();
        spec.when = "   ".into();

        let err = WorkflowRegistry::default()
            .save_new(&spec, &dir)
            .unwrap_err();
        assert!(err.to_string().contains("when"), "{err}");
        assert!(
            !dir.join("sample.workflow.md").exists(),
            "nothing is written for a spec that cannot be loaded"
        );
    }

    #[test]
    fn a_spec_round_trips_through_serde() {
        let spec = parse(SAMPLE, "ignored").unwrap();
        let json = serde_json::to_value(&spec).unwrap();
        let back: WorkflowSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn a_registry_lists_specs_sorted_by_id() {
        let spec = |id: &str| WorkflowSpec {
            id: id.into(),
            name: id.into(),
            description: "d".into(),
            when: "w".into(),
            stages: vec![stage("a", &[])],
            guidance: "Body.".into(),
            source_path: PathBuf::from("nowhere"),
        };
        let registry = registry_of(vec![spec("zeta"), spec("alpha"), spec("mid")]);

        assert_eq!(registry.names(), vec!["alpha", "mid", "zeta"]);
        let listed: Vec<&str> = registry
            .list()
            .iter()
            .map(|spec| spec.id.as_str())
            .collect();
        assert_eq!(listed, vec!["alpha", "mid", "zeta"]);
        assert_eq!(registry.len(), 3);
        assert!(!registry.is_empty());
    }

    #[test]
    fn a_stage_agent_that_is_not_defined_is_refused_and_named() {
        // `SAMPLE`'s first stage names `default`, which nothing here defines.
        let spec = parse(SAMPLE, "ignored").unwrap();
        let known: BTreeSet<String> = ["other".to_string()].into_iter().collect();

        let err = spec.validate_agents(&known).unwrap_err();
        assert!(err.to_string().contains("default"), "{err}");
        assert!(err.to_string().contains("first"), "{err}");
        assert!(err.to_string().contains("test.workflow.md"), "{err}");
    }

    #[test]
    fn a_stage_agent_that_is_defined_passes() {
        let spec = parse(SAMPLE, "ignored").unwrap();
        let known: BTreeSet<String> = ["default".to_string()].into_iter().collect();
        assert!(spec.validate_agents(&known).is_ok());
    }

    #[test]
    fn an_agent_aware_registry_refuses_to_save_a_workflow_with_a_missing_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        let registry = WorkflowRegistry::default().with_known_agents(["other".to_string()]);

        let spec = parse(SAMPLE, "ignored").unwrap();
        let err = registry.save_new(&spec, &dir).unwrap_err();
        assert!(err.to_string().contains("default"), "{err}");
        assert!(
            !dir.join("sample.workflow.md").exists(),
            "a workflow naming an undefined agent must not reach disk"
        );
    }

    #[test]
    fn an_agent_aware_load_is_fatal_on_a_missing_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        write(&dir, "sample.workflow.md", SAMPLE);

        // The same file loads when agents are not checked...
        assert!(WorkflowRegistry::from_dirs(std::slice::from_ref(&dir)).is_ok());

        // ...and fails, naming the file and the agent, when they are.
        let err =
            WorkflowRegistry::load_with_agents(tmp.path(), ["other".to_string()]).unwrap_err();
        assert!(err.to_string().contains("default"), "{err}");
        assert!(err.to_string().contains("sample.workflow.md"), "{err}");

        // A registry without an installed agent set still refuses nothing.
        let plain = WorkflowRegistry::from_dirs(&[dir]).unwrap();
        assert!(plain.validate_agents().is_ok());
    }
}
