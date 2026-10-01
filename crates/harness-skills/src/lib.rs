//! Skill loading with progressive disclosure.
//!
//! A skill is a pair of costs. Its name and description are paid on every
//! request because the model has to see them to decide whether the skill
//! applies; its body is paid only once the skill is activated. Keeping those two
//! numbers apart is the whole point of this crate: [`SkillRegistry::load`] reads
//! only the metadata, and the body stays on disk until
//! [`SkillRegistry::activate`] is asked for it.
//!
//! Discovery mirrors `harness-agent`: a global root, then the checked-in
//! workspace root, then the project's runtime state directory, with later roots
//! overriding earlier ones by name. A malformed file fails the whole load and
//! names its path, because a half-read skill is worse than a refused one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_core::{frontmatter, global_dir, project_dir, HarnessError, Result, TokenEstimator};
use serde::{Deserialize, Serialize};

/// The declarative frontmatter of a `SKILL.md`, before defaults are applied.
///
/// Separate from [`SkillSpec`] because the two required fields are still
/// optional here: `name` falls back to the file name, and the body is what
/// [`SkillSpec::from_markdown`] validates afterwards.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Frontmatter {
    name: String,
    description: String,
    model: Option<String>,
    #[serde(alias = "allowed-tools")]
    allowed_tools: Vec<String>,
}

/// One skill as declared by a `SKILL.md` file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillSpec {
    pub name: String,
    pub description: String,
    /// Model the skill prefers while active, in `provider/model` form.
    pub model: Option<String>,
    /// Narrows the tool whitelist while this skill is active. Empty means no narrowing.
    #[serde(alias = "allowed-tools")]
    pub allowed_tools: Vec<String>,
    /// The Markdown body: the instructions injected when the skill activates.
    pub instructions: String,
    pub source_path: PathBuf,
}

impl SkillSpec {
    /// Reads a document into a validated spec.
    ///
    /// `fallback_name` is the file stem, or the directory name for the
    /// `SKILL.md` layout, so a document that omits `name` still gets a stable
    /// handle. A missing description is an error rather than a default: the
    /// description is the only text the model sees before deciding to activate,
    /// so a skill without one can never be chosen.
    pub fn from_markdown(
        markdown: &str,
        source_path: PathBuf,
        fallback_name: &str,
    ) -> Result<Self> {
        let (fields, body) = frontmatter::parse::<Frontmatter>(markdown)
            .map_err(|err| invalid(&source_path, &format!("frontmatter is not valid: {err}")))?;

        if body.is_empty() {
            return Err(invalid(
                &source_path,
                "the Markdown body is empty, so the skill has no instructions",
            ));
        }

        let name = pick(fields.name, fallback_name);
        if name.is_empty() {
            return Err(invalid(
                &source_path,
                "no name: name the file or directory, or set `name` in the frontmatter",
            ));
        }

        let description = fields.description.trim();
        if description.is_empty() {
            return Err(invalid(
                &source_path,
                "no description: it is the only text the model sees when choosing whether to activate",
            ));
        }

        Ok(Self {
            name,
            description: description.to_string(),
            model: trimmed(fields.model),
            allowed_tools: fields
                .allowed_tools
                .into_iter()
                .filter_map(|tool| trimmed(Some(tool)))
                .collect(),
            instructions: body,
            source_path,
        })
    }

    /// Cost of the metadata that is always injected (name + description).
    pub fn metadata_tokens(&self, estimator: &dyn TokenEstimator) -> usize {
        estimator.estimate(&self.name) + estimator.estimate(&self.description)
    }

    /// Cost of the body that is only injected on activation.
    pub fn body_tokens(&self, estimator: &dyn TokenEstimator) -> usize {
        estimator.estimate(&self.instructions)
    }
}

/// A skill with its body pulled in, produced only when the skill activates.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivatedSkill {
    pub name: String,
    pub instructions: String,
    pub model: Option<String>,
    pub allowed_tools: Vec<String>,
}

/// Every skill discoverable from a set of directories.
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    by_name: BTreeMap<String, SkillSpec>,
}

/// Directories consulted by [`SkillRegistry::load`], lowest precedence first.
///
/// As with agents, the checked-in `<workspace>/skills` comes before
/// `<workspace>/.harness/skills` so a project can override a repository-level
/// skill without editing the file it was copied from.
pub fn search_dirs(workspace_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = global_dir() {
        dirs.push(dir.join("skills"));
    }
    dirs.push(workspace_root.join("skills"));
    dirs.push(project_dir(workspace_root).join("skills"));
    dirs
}

impl SkillRegistry {
    /// Loads `~/.harness/skills/`, then `<workspace>/skills/` and
    /// `<workspace>/.harness/skills/`, so a project-level file overrides a
    /// global one with the same name.
    pub fn load(workspace_root: &Path) -> Result<Self> {
        Self::from_dirs(&search_dirs(workspace_root))
    }

    /// Loads from explicit directories, later directories winning.
    pub fn from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let mut by_name = BTreeMap::new();

        for dir in dirs {
            for (path, fallback_name) in skill_files(dir)? {
                let markdown = std::fs::read_to_string(&path)
                    .map_err(|err| invalid(&path, &format!("cannot be read: {err}")))?;
                let spec = SkillSpec::from_markdown(&markdown, path, &fallback_name)?;
                by_name.insert(spec.name.clone(), spec);
            }
        }

        Ok(Self { by_name })
    }

    pub fn get(&self, name: &str) -> Option<&SkillSpec> {
        self.by_name.get(name)
    }

    /// Sorted by name.
    pub fn list(&self) -> Vec<&SkillSpec> {
        self.by_name.values().collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.by_name.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Resolves names to specs, rejecting unknown ones with `SkillNotFound`.
    pub fn resolve(&self, names: &[String]) -> Result<Vec<&SkillSpec>> {
        names
            .iter()
            .map(|name| {
                self.get(name)
                    .ok_or_else(|| HarnessError::SkillNotFound(name.clone()))
            })
            .collect()
    }

    /// Progressive disclosure: the index costs only metadata; this is the
    /// expensive step, and it happens only for the skills actually in use.
    pub fn activate(&self, names: &[String]) -> Result<Vec<ActivatedSkill>> {
        Ok(self
            .resolve(names)?
            .into_iter()
            .map(|spec| ActivatedSkill {
                name: spec.name.clone(),
                instructions: spec.instructions.clone(),
                model: spec.model.clone(),
                allowed_tools: spec.allowed_tools.clone(),
            })
            .collect())
    }
}

/// Finds the skill files directly under `dir`, as `(path, fallback name)`.
///
/// Two shapes are accepted, matching what agents already reference:
///
/// - `<dir>/<name>.md` — a bare document, named by its file stem. Not searched
///   recursively: a `.md` file in a nested directory is documentation, not a
///   skill.
/// - `<dir>/<name>/SKILL.md` — the documented layout, one level deep, named by
///   its directory because the file stem is always `SKILL`.
///
/// Any other file is ignored. Note that a stray `README.md` at the root of a
/// search directory *is* picked up and, having no description, fails the load —
/// the same trade the agent registry makes.
fn skill_files(dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    let entries = match std::fs::read_dir(dir) {
        // An absent directory just means nothing is declared there.
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(HarnessError::Io(err)),
    };

    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            let nested = path.join("SKILL.md");
            if nested.is_file() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    found.push((nested, name.to_string()));
                }
            }
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string);
        if let Some(stem) = stem.filter(|stem| !stem.is_empty()) {
            found.push((path, stem));
        }
    }

    // `read_dir` order is filesystem-dependent; sorting keeps override behaviour
    // and error reporting stable when two files imply the same name.
    found.sort();
    Ok(found)
}

fn invalid(path: &Path, message: &str) -> HarnessError {
    // There is no skill-specific variant in `harness-core`, and a malformed
    // declarative Markdown spec is exactly what this one describes; it carries
    // the path, which is what the error needs to name.
    HarnessError::InvalidAgentSpec {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
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
    use harness_core::HeuristicEstimator;
    use std::slice;

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(dir)).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn names(registry: &SkillRegistry) -> Vec<String> {
        registry.names()
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let registry = SkillRegistry::from_dirs(&[PathBuf::from("does/not/exist")]).unwrap();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.get("anything").is_none());
        assert!(registry.list().is_empty());
    }

    #[test]
    fn both_file_shapes_are_discovered() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        write(
            &dir,
            "bare-name.md",
            "---\ndescription: A bare document.\n---\n\nBare body.\n",
        );
        write(
            &dir,
            "in-a-directory/SKILL.md",
            "---\ndescription: The documented layout.\n---\n\nNested body.\n",
        );
        write(&dir, "notes.txt", "not a skill");
        write(&dir, "deeper/ignored.md", "also not found at this level");

        let registry = SkillRegistry::from_dirs(slice::from_ref(&dir)).unwrap();
        assert_eq!(names(&registry), vec!["bare-name", "in-a-directory"]);

        let nested = registry.get("in-a-directory").unwrap();
        assert_eq!(nested.description, "The documented layout.");
        assert_eq!(nested.instructions, "Nested body.");
        assert!(nested.source_path.ends_with("in-a-directory/SKILL.md"));
    }

    #[test]
    fn a_frontmatter_name_wins_over_the_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        write(
            &dir,
            "wrong-name/SKILL.md",
            "---\nname: right-name\ndescription: Renamed.\n---\n\nBody.\n",
        );

        let registry = SkillRegistry::from_dirs(&[dir]).unwrap();
        assert_eq!(names(&registry), vec!["right-name"]);
    }

    #[test]
    fn optional_fields_parse_in_both_spellings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        write(
            &dir,
            "hyphen.md",
            "---\ndescription: Hyphens.\nmodel: deepseek/deepseek-reasoner\nallowed-tools:\n  - read_file\n  - grep\n---\n\nBody.\n",
        );
        write(
            &dir,
            "underscore.md",
            "---\ndescription: Underscores.\nallowed_tools:\n  - read_file\n---\n\nBody.\n",
        );
        write(
            &dir,
            "blank.md",
            "---\ndescription: Blank values.\nmodel: \"  \"\nallowed-tools:\n  - \"  \"\n---\n\nBody.\n",
        );

        let registry = SkillRegistry::from_dirs(&[dir]).unwrap();

        let hyphen = registry.get("hyphen").unwrap();
        assert_eq!(
            hyphen.model.as_deref(),
            Some("deepseek/deepseek-reasoner"),
            "`allowed-tools` is accepted alongside `allowed_tools`"
        );
        assert_eq!(hyphen.allowed_tools, vec!["read_file", "grep"]);

        assert_eq!(
            registry.get("underscore").unwrap().allowed_tools,
            vec!["read_file"]
        );

        let blank = registry.get("blank").unwrap();
        assert_eq!(blank.model, None, "a whitespace model is not a model");
        assert!(blank.allowed_tools.is_empty());
    }

    #[test]
    fn a_later_directory_overrides_an_earlier_one() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join("global");
        let project = tmp.path().join("project");
        let other = tmp.path().join("other");

        write(
            &global,
            "shared/SKILL.md",
            "---\ndescription: Global.\n---\n\nGlobal body.\n",
        );
        write(
            &global,
            "only-global/SKILL.md",
            "---\ndescription: Only global.\n---\n\nBody.\n",
        );
        write(
            &project,
            "shared/SKILL.md",
            "---\ndescription: Project.\n---\n\nProject body.\n",
        );
        write(
            &other,
            "only-other/SKILL.md",
            "---\ndescription: Only other.\n---\n\nBody.\n",
        );

        let registry = SkillRegistry::from_dirs(&[global, project.clone(), other]).unwrap();

        assert_eq!(
            names(&registry),
            vec!["only-global", "only-other", "shared"]
        );
        let winner = registry.get("shared").unwrap();
        assert_eq!(winner.instructions, "Project body.");
        assert!(winner.source_path.starts_with(&project));
    }

    #[test]
    fn a_malformed_file_fails_the_whole_load_with_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        write(
            &dir,
            "good/SKILL.md",
            "---\ndescription: Fine.\n---\n\nBody.\n",
        );
        write(
            &dir,
            "bad/SKILL.md",
            "---\ndescription: [unclosed\n---\n\nBody.\n",
        );

        let err = SkillRegistry::from_dirs(slice::from_ref(&dir)).unwrap_err();
        assert!(
            matches!(err, HarnessError::InvalidAgentSpec { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("bad"), "{err}");
    }

    #[test]
    fn a_skill_missing_its_description_or_body_fails_the_load() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");

        write(&dir, "hollow/SKILL.md", "---\ndescription: Has one.\n---\n");
        let err = SkillRegistry::from_dirs(slice::from_ref(&dir)).unwrap_err();
        assert!(err.to_string().contains("hollow"), "{err}");
        assert!(err.to_string().contains("no instructions"), "{err}");

        let dir2 = tmp.path().join("skills2");
        write(
            &dir2,
            "silent/SKILL.md",
            "---\nname: silent\n---\n\nBody.\n",
        );
        let err = SkillRegistry::from_dirs(&[dir2]).unwrap_err();
        assert!(err.to_string().contains("no description"), "{err}");
    }

    #[test]
    fn resolve_and_activate_reject_unknown_names() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        write(
            &dir,
            "known/SKILL.md",
            "---\ndescription: Known.\nmodel: deepseek/deepseek-chat\nallowed-tools:\n  - read_file\n---\n\nKnown body.\n",
        );

        let registry = SkillRegistry::from_dirs(&[dir]).unwrap();

        let err = registry.resolve(&["ghost".to_string()]).unwrap_err();
        assert!(
            matches!(&err, HarnessError::SkillNotFound(name) if name == "ghost"),
            "{err}"
        );
        assert!(registry.activate(&["ghost".to_string()]).is_err());

        let activated = registry.activate(&["known".to_string()]).unwrap();
        assert_eq!(activated.len(), 1);
        assert_eq!(activated[0].name, "known");
        assert_eq!(activated[0].instructions, "Known body.");
        assert_eq!(
            activated[0].model.as_deref(),
            Some("deepseek/deepseek-chat")
        );
        assert_eq!(activated[0].allowed_tools, vec!["read_file"]);
        assert_eq!(
            registry.resolve(&["known".to_string()]).unwrap()[0].name,
            registry.get("known").unwrap().name
        );
    }

    #[test]
    fn metadata_is_cheaper_than_the_body() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skills");
        let body = "A rule.\n".repeat(40);
        write(
            &dir,
            "long/SKILL.md",
            &format!("---\ndescription: Short.\n---\n\n{body}"),
        );

        let registry = SkillRegistry::from_dirs(&[dir]).unwrap();
        let spec = registry.get("long").unwrap();
        let estimator = HeuristicEstimator::default();

        // The numbers the CLI prints are the whole rationale for the split.
        assert!(spec.metadata_tokens(&estimator) < spec.body_tokens(&estimator));
        assert_eq!(
            spec.body_tokens(&estimator),
            estimator.estimate(&spec.instructions)
        );
    }

    #[test]
    fn a_spec_round_trips_through_serde() {
        let spec = SkillSpec::from_markdown(
            "---\ndescription: Round trip.\nallowed-tools:\n  - read_file\n---\n\nBody.\n",
            PathBuf::from("skills/round/SKILL.md"),
            "round",
        )
        .unwrap();

        let json = serde_json::to_value(&spec).unwrap();
        let back: SkillSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn search_dirs_ends_with_the_project_state_directory() {
        let root = Path::new("/work/project");
        let dirs = search_dirs(root);
        assert!(dirs.last().unwrap().ends_with(".harness/skills"));
        assert!(dirs.len() >= 2);
        assert!(dirs.iter().any(|dir| dir.ends_with("skills")));
    }
}
