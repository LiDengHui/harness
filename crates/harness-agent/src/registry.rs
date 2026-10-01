//! Discovery of the `.agent.md` files that declare agents.
//!
//! The registry is a plain id-keyed map: loading is eager so a malformed file
//! fails the run at startup, with the offending path in the message, rather than
//! halfway through a session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_core::{global_dir, project_dir, HarnessError, Result};

use crate::spec::AgentSpec;

/// Suffix that marks a document as an agent definition.
const AGENT_SUFFIX: &str = ".agent.md";

/// Every agent discoverable from a set of directories.
#[derive(Debug, Clone, Default)]
pub struct AgentRegistry {
    by_id: BTreeMap<String, AgentSpec>,
}

/// Directories consulted by [`AgentRegistry::load`], lowest precedence first.
///
/// The checked-in `<workspace>/agents` directory comes before the project's
/// runtime state directory so a local `.harness/agents` can override a
/// repository-level declaration without editing it.
pub fn search_dirs(workspace_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = global_dir() {
        dirs.push(dir.join("agents"));
    }
    dirs.push(workspace_root.join("agents"));
    dirs.push(project_dir(workspace_root).join("agents"));
    dirs
}

impl AgentRegistry {
    /// Loads `~/.harness/agents/` first, then `<workspace>/agents/` and
    /// `<workspace>/.harness/agents/`, so a project-level file overrides a global
    /// one with the same id.
    pub fn load(workspace_root: &Path) -> Result<Self> {
        Self::from_dirs(&search_dirs(workspace_root))
    }

    /// Loads from explicit directories, later directories winning.
    pub fn from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let mut by_id = BTreeMap::new();

        for dir in dirs {
            let entries = match std::fs::read_dir(dir) {
                // An absent directory just means nothing is declared there.
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(HarnessError::Io(err)),
            };

            for entry in entries {
                let path = entry?.path();
                if !path.is_file() {
                    continue;
                }
                let Some(id) = default_id_of(&path) else {
                    continue;
                };
                let markdown = std::fs::read_to_string(&path).map_err(|err| {
                    HarnessError::InvalidAgentSpec {
                        path: path.clone(),
                        message: format!("cannot be read: {err}"),
                    }
                })?;
                let spec = AgentSpec::from_markdown(&markdown, path, &id)?;
                by_id.insert(spec.id.clone(), spec);
            }
        }

        Ok(Self { by_id })
    }

    pub fn get(&self, id: &str) -> Option<&AgentSpec> {
        self.by_id.get(id)
    }

    /// Sorted by id.
    pub fn list(&self) -> Vec<&AgentSpec> {
        self.by_id.values().collect()
    }

    /// The spec to use when the caller names none: the configured default, then
    /// an agent literally called "default", then the first one found.
    pub fn default_spec(&self, configured_default: &str) -> Option<&AgentSpec> {
        self.get(configured_default)
            .or_else(|| self.get("default"))
            .or_else(|| self.by_id.values().next())
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Specs whose ids are listed, rejecting unknown ids.
    ///
    /// The error is reported against the *declaring* file, because a dangling
    /// sub-agent reference is a defect in the parent document.
    pub fn subagents_of(&self, spec: &AgentSpec) -> Result<Vec<&AgentSpec>> {
        spec.subagents
            .iter()
            .map(|id| {
                self.get(id).ok_or_else(|| HarnessError::InvalidAgentSpec {
                    path: spec.source_path.clone(),
                    message: format!("sub-agent `{id}` is not defined"),
                })
            })
            .collect()
    }
}

/// The id implied by a file name: `backend-architect.agent.md` → `backend-architect`.
fn default_id_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(AGENT_SUFFIX)
        .or_else(|| name.strip_suffix(".md"))?;
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn ids(registry: &AgentRegistry) -> Vec<&str> {
        registry
            .list()
            .iter()
            .map(|spec| spec.id.as_str())
            .collect()
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let registry = AgentRegistry::from_dirs(&[PathBuf::from("does/not/exist")]).unwrap();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.get("anything").is_none());
        assert!(registry.list().is_empty());
    }

    #[test]
    fn ids_come_from_the_file_name_and_the_suffix_is_optional() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        write(&dir, "backend-architect.agent.md", "Architect body.");
        write(&dir, "code-reviewer.md", "Reviewer body.");
        write(&dir, "notes.txt", "not an agent");
        write(
            &dir,
            "README.md",
            "not an agent either, but it is a .md file",
        );

        let registry = AgentRegistry::from_dirs(&[dir]).unwrap();
        assert_eq!(
            ids(&registry),
            vec!["README", "backend-architect", "code-reviewer"]
        );
        assert_eq!(
            registry.get("backend-architect").unwrap().system_prompt,
            "Architect body."
        );
    }

    #[test]
    fn a_file_declaring_an_id_wins_over_its_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        write(
            &dir,
            "wrong-name.agent.md",
            "---\nid: right-name\n---\nBody.",
        );

        let registry = AgentRegistry::from_dirs(&[dir]).unwrap();
        assert_eq!(ids(&registry), vec!["right-name"]);
    }

    #[test]
    fn a_later_directory_overrides_an_earlier_one() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join("global");
        let project = tmp.path().join("project");
        let other = tmp.path().join("other");

        write(&global, "default.agent.md", "Global default.");
        write(&global, "only-global.agent.md", "Only global.");
        write(&project, "default.agent.md", "Project default.");
        write(&other, "only-other.agent.md", "Only other.");

        let registry = AgentRegistry::from_dirs(&[global, project.clone(), other]).unwrap();

        assert_eq!(ids(&registry), vec!["default", "only-global", "only-other"]);
        let winner = registry.get("default").unwrap();
        assert_eq!(winner.system_prompt, "Project default.");
        assert!(winner.source_path.starts_with(&project));
    }

    #[test]
    fn a_malformed_file_fails_the_whole_load_with_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        write(&dir, "good.agent.md", "Fine.");
        write(&dir, "bad.agent.md", "---\nmax_tokens: lots\n---\nBody.");

        let err = AgentRegistry::from_dirs(std::slice::from_ref(&dir)).unwrap_err();
        assert!(
            matches!(err, HarnessError::InvalidAgentSpec { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("bad.agent.md"), "{err}");
    }

    #[test]
    fn an_agent_declaring_no_body_fails_the_load() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        write(&dir, "hollow.agent.md", "---\nname: Hollow\n---\n");

        let err = AgentRegistry::from_dirs(&[dir]).unwrap_err();
        assert!(err.to_string().contains("hollow.agent.md"), "{err}");
    }

    #[test]
    fn the_default_spec_follows_the_configured_name_then_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        write(&dir, "aaa.agent.md", "First by id.");
        write(&dir, "default.agent.md", "The default one.");
        write(&dir, "zeta.agent.md", "Last by id.");

        let registry = AgentRegistry::from_dirs(&[dir]).unwrap();
        assert_eq!(
            registry.default_spec("zeta").unwrap().id,
            "zeta",
            "a configured default wins"
        );
        assert_eq!(registry.default_spec("missing").unwrap().id, "default");
        assert_eq!(registry.default_spec("").unwrap().id, "default");

        let without_default = AgentRegistry::from_dirs(&[tmp.path().join("empty")]).unwrap();
        assert!(without_default.default_spec("missing").is_none());
    }

    #[test]
    fn subagents_of_resolves_ids_and_rejects_unknown_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        write(
            &dir,
            "parent.agent.md",
            "---\nsubagents:\n  - child\n  - ghost\n---\nParent body.",
        );
        write(&dir, "child.agent.md", "Child body.");

        let registry = AgentRegistry::from_dirs(&[dir]).unwrap();
        let parent = registry.get("parent").unwrap();

        let err = registry.subagents_of(parent).unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        assert!(err.to_string().contains("parent.agent.md"), "{err}");

        let mut fixed = parent.clone();
        fixed.subagents = vec!["child".into()];
        let resolved = registry.subagents_of(&fixed).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "child");
    }

    #[test]
    fn search_dirs_ends_with_the_project_state_directory() {
        let root = Path::new("/work/project");
        let dirs = search_dirs(root);
        assert!(dirs.last().unwrap().ends_with(".harness/agents"));
        assert!(dirs.iter().any(|dir| dir.ends_with("agents")));
        assert!(dirs.len() >= 2);
    }
}
