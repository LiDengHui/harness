//! The tool layer: the [`Tool`] trait, a [`ToolRegistry`], and the builtin
//! filesystem, shell and web tools.
//!
//! Every filesystem path a builtin touches goes through [`ToolContext::resolve`],
//! which refuses anything that resolves outside the workspace root. That one
//! guard is what makes it safe to hand an autonomous agent a writing tool.

mod args;
pub mod builtin;

// `#[harness_macros::tool]` expands to code that names `::harness_tools` by
// absolute path, so the crate has to be able to refer to itself under its own
// name. The alias is used by the generated `Tool` impls in `builtin`.
extern crate self as harness_tools;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harness_core::{object_schema, Result, ToolSpec, ToolsConfig};
use serde_json::{json, Value};

/// The meta-tool that carries the cheap tool index when schemas are deferred.
///
/// It is not a registered [`Tool`]: the agent loop answers it directly, because
/// its result is the schemas of the tools it names rather than any workspace
/// effect. Keeping the name here lets the loop and the registry agree on it.
pub const REQUEST_TOOLS: &str = "request_tools";

/// How much of a tool description the index repeats before eliding the rest.
///
/// The index is a hint, not the schema: the full description still arrives when
/// the tool is activated. A long first sentence is clipped so one tool cannot
/// dominate the index the way its full schema would have dominated the request.
const INDEX_SUMMARY_CHARS: usize = 96;

/// What a single tool call produced.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    pub metadata: Value,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            metadata: json!({}),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            is_error: true,
            metadata: json!({}),
        }
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Cooperative cancellation, shared between the agent loop and running tools.
#[derive(Debug, Clone, Default)]
pub struct AbortFlag(Arc<AtomicBool>);

impl AbortFlag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Sink for progress messages a long-running tool emits while it works.
pub type ProgressSender = tokio::sync::mpsc::UnboundedSender<String>;

/// Everything a tool needs beyond its arguments: where it may touch the disk,
/// the limits it must respect, and how it reports progress.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace_root: PathBuf,
    pub config: ToolsConfig,
    pub abort: AbortFlag,
    pub progress: Option<ProgressSender>,
}

impl ToolContext {
    pub fn new(workspace_root: impl Into<PathBuf>, config: ToolsConfig) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            config,
            abort: AbortFlag::new(),
            progress: None,
        }
    }

    /// Containment-checked path resolution. EVERY filesystem tool goes through this.
    pub fn resolve(&self, path: &Path) -> Result<PathBuf> {
        harness_core::path::resolve_within(&self.workspace_root, path)
    }

    pub fn emit_progress(&self, message: impl Into<String>) {
        if let Some(sender) = &self.progress {
            let _ = sender.send(message.into());
        }
    }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput>;
}

/// Name-keyed set of tools, ordered so that [`ToolRegistry::specs`] and
/// [`ToolRegistry::names`] are stable across runs.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        for tool in builtin::all() {
            registry.register(tool);
        }
        registry
    }

    /// Whitelist by name; unknown names are ignored rather than an error so a
    /// config listing a tool this build does not ship still starts.
    pub fn filter(&self, names: &[String]) -> ToolRegistry {
        let mut registry = Self::new();
        for name in names {
            if let Some(tool) = self.tools.get(name) {
                registry.register(tool.clone());
            }
        }
        registry
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    /// The full spec of one registered tool, by name.
    ///
    /// The deferred-schema path needs this to hand back a tool's schema after
    /// the model asks for it, without exposing the private tool map.
    pub fn spec(&self, name: &str) -> Option<ToolSpec> {
        self.tools.get(name).map(|tool| tool.spec())
    }

    /// The specs to advertise when schemas are deferred.
    ///
    /// The cheap index always leads, then the full schema of every tool the
    /// model has already activated, in name order so the advertised list is
    /// byte-identical from turn to turn — which is what keeps a provider's
    /// prefix cache warm. An unactivated tool costs only its index line.
    pub fn advertised_specs(&self, activated: &HashSet<String>) -> Vec<ToolSpec> {
        let mut specs = Vec::with_capacity(activated.len() + 1);
        specs.push(self.index_spec());
        for (name, tool) in &self.tools {
            if activated.contains(name) {
                specs.push(tool.spec());
            }
        }
        specs
    }

    /// One spec that names every tool without shipping its schema.
    ///
    /// The model calls it with the names it needs; the loop answers with those
    /// tools' full schemas and advertises them from then on. This is what lets
    /// a turn that uses no tools pay for a few hundred bytes instead of every
    /// builtin's description and JSON schema.
    pub fn index_spec(&self) -> ToolSpec {
        let mut description = String::from(
            "Activate one or more tools by name, then call them. The tools available in \
             this workspace are:\n",
        );
        for (name, tool) in &self.tools {
            let _ = writeln!(
                description,
                "- `{name}`: {}",
                index_summary(&tool.spec().description)
            );
        }
        description.push_str(
            "Pass the names you need in `names`; their full schemas are returned and stay \
             callable for the rest of the run. A tool may also be called directly and is \
             activated on first use.",
        );
        ToolSpec::new(
            REQUEST_TOOLS,
            description,
            object_schema(
                json!({
                    "names": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Names of the tools to activate."
                    }
                }),
                &["names"],
            ),
        )
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// The first sentence of a tool description, clipped to [`INDEX_SUMMARY_CHARS`].
///
/// The index repeats just enough for the model to recognise a tool; the full
/// description is only paid for once the tool is activated.
fn index_summary(description: &str) -> String {
    let sentence = description
        .find(". ")
        .map(|end| &description[..end])
        .unwrap_or(description);
    if sentence.chars().count() <= INDEX_SUMMARY_CHARS {
        return sentence.to_string();
    }
    let clipped: String = sentence.chars().take(INDEX_SUMMARY_CHARS - 3).collect();
    format!("{clipped}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_constructors_set_the_error_flag() {
        let ok = ToolOutput::text("fine").with_metadata(json!({ "a": 1 }));
        assert!(!ok.is_error);
        assert_eq!(ok.metadata["a"], 1);

        let bad = ToolOutput::error("boom");
        assert!(bad.is_error);
        assert_eq!(bad.metadata, json!({}));
    }

    #[test]
    fn abort_flag_is_shared_between_clones() {
        let flag = AbortFlag::new();
        let clone = flag.clone();
        assert!(!clone.is_aborted());
        flag.abort();
        assert!(clone.is_aborted());
    }

    #[test]
    fn registry_exposes_the_builtins_by_name() {
        let registry = ToolRegistry::with_builtins();
        assert_eq!(registry.len(), 7);
        assert_eq!(
            registry.names(),
            vec![
                "edit_file",
                "grep",
                "list_dir",
                "read_file",
                "shell",
                "web_fetch",
                "write_file",
            ]
        );
        assert_eq!(registry.specs().len(), 7);

        let only_read = registry.filter(&["read_file".to_string()]);
        assert_eq!(only_read.names(), vec!["read_file"]);
        assert!(only_read.get("read_file").is_some());
        assert!(registry.get("nope").is_none());
        assert!(ToolRegistry::new().is_empty());
    }

    #[test]
    fn the_tool_index_names_every_tool_but_ships_no_schema() {
        let registry = ToolRegistry::with_builtins();
        let index = registry.index_spec();
        assert_eq!(index.name, REQUEST_TOOLS);
        for name in registry.names() {
            assert!(
                index.description.contains(&format!("`{name}`")),
                "tool `{name}` is missing from the index"
            );
        }
        assert_eq!(index.parameters["required"][0], "names");

        // The index exists to be cheaper than the schemas it replaces.
        use harness_core::HeuristicEstimator;
        let estimator = HeuristicEstimator::default();
        let full: usize = registry
            .specs()
            .iter()
            .map(|spec| spec.advertised_tokens(&estimator))
            .sum();
        assert!(
            index.advertised_tokens(&estimator) < full / 2,
            "index {} must be well under the full set {}",
            index.advertised_tokens(&estimator),
            full
        );
    }

    #[test]
    fn advertised_specs_is_the_index_plus_the_activated_tools() {
        let registry = ToolRegistry::with_builtins();

        let none = registry.advertised_specs(&HashSet::new());
        assert_eq!(none.len(), 1);
        assert_eq!(none[0].name, REQUEST_TOOLS);

        let mut activated = HashSet::new();
        activated.insert("read_file".to_string());
        activated.insert("grep".to_string());
        let some = registry.advertised_specs(&activated);
        let names: Vec<&str> = some.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, vec![REQUEST_TOOLS, "grep", "read_file"]);

        assert_eq!(
            registry.spec("read_file").map(|spec| spec.name),
            Some("read_file".to_string())
        );
        assert!(registry.spec("nope").is_none());
    }

    #[test]
    fn index_summaries_are_the_first_sentence_and_bounded() {
        assert_eq!(
            index_summary("Read a file. Fails when missing."),
            "Read a file"
        );
        let long = "a".repeat(200);
        let summary = index_summary(&long);
        assert!(summary.chars().count() <= INDEX_SUMMARY_CHARS);
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn every_builtin_description_says_what_it_does_and_how_it_fails() {
        for spec in ToolRegistry::with_builtins().specs() {
            let description = &spec.description;
            assert!(
                description.len() >= 200,
                "{}: description is too terse ({} chars): {description}",
                spec.name,
                description.len()
            );
            assert!(
                description.contains("Prefer"),
                "{}: description never says when to prefer it: {description}",
                spec.name
            );
            assert!(
                description.contains("Fails") || description.contains("fails"),
                "{}: description never says how it fails: {description}",
                spec.name
            );
        }
    }
}
