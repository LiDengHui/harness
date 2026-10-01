//! Configuration resolution.
//!
//! Two layers are merged, project winning over global:
//!   1. `<home>/.harness/config.toml`
//!   2. `<workspace>/.harness/config.toml`
//!
//! Secrets are never stored in these files. A provider config names the
//! *environment variable* that holds its key, and the key is read at call time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, Result};

/// Name of the harness state directory, both global and per project.
pub const HARNESS_DIR: &str = ".harness";

/// Fully resolved harness configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub harness: HarnessSection,
    pub provider: ProviderSection,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub tools: ToolsConfig,
    pub memory: MemoryConfig,
    pub server: ServerConfig,
    pub mcp: McpConfig,
    pub guardrails: GuardrailsConfig,
    pub thinking: ThinkingConfig,
    pub context: ContextConfig,
    pub ui: UiConfig,
    pub verify: VerifyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessSection {
    /// Defaults to the process working directory when unset.
    pub workspace_root: Option<PathBuf>,
    /// Agent used when the CLI/UI does not name one.
    pub default_agent: String,
    /// Hard ceiling on ReAct turns per run.
    pub max_iterations: usize,
}

impl Default for HarnessSection {
    fn default() -> Self {
        Self {
            workspace_root: None,
            default_agent: "default".to_string(),
            max_iterations: 64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderSection {
    /// Key into `providers`.
    pub default: String,
    pub request_timeout_secs: u64,
    pub max_retries: u32,
}

impl Default for ProviderSection {
    fn default() -> Self {
        Self {
            default: "openai".to_string(),
            request_timeout_secs: 180,
            max_retries: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Any OpenAI-compatible `/chat/completions` endpoint.
    #[default]
    OpenAi,
    /// Scripted provider used by tests and offline demos.
    Mock,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// `kind` is canonical; `type` is accepted because gateway configs commonly
    /// spell it that way.
    #[serde(alias = "type")]
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    /// A literal key. Convenient for throwaway or gateway endpoints, but it puts
    /// the secret in a file, so keep it in a gitignored layer and prefer
    /// `api_key_env` everywhere else.
    pub api_key: Option<String>,
    /// Name of the environment variable holding the API key.
    pub api_key_env: Option<String>,
    pub default_model: Option<String>,
    /// Extra headers sent with every request; gateways and proxies require them.
    pub custom_headers: BTreeMap<String, String>,
}

/// Base URL assumed when a provider config omits one.
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

impl ProviderConfig {
    pub fn base_url(&self) -> &str {
        self.base_url.as_deref().unwrap_or(DEFAULT_OPENAI_BASE_URL)
    }

    /// Resolves the API key: an inline value wins, otherwise the named
    /// environment variable.
    ///
    /// Returns `Ok(None)` when nothing is configured or the variable is unset,
    /// which local endpoints (Ollama, llama.cpp) legitimately allow.
    pub fn api_key(&self) -> Result<Option<String>> {
        if let Some(key) = self.api_key.as_deref() {
            if !key.trim().is_empty() {
                tracing::warn!(
                    "provider config carries an inline api_key; use api_key_env so the secret stays out of the file"
                );
                return Ok(Some(key.to_string()));
            }
        }

        let Some(var) = self.api_key_env.as_deref() else {
            return Ok(None);
        };
        match std::env::var(var) {
            Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
            Ok(_) => Ok(None),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(HarnessError::Config(format!(
                "environment variable `{var}` is not valid UTF-8"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// Shell used by the `shell` tool. Auto-detected when unset.
    pub shell_program: Option<String>,
    pub shell_timeout_secs: u64,
    /// Tool output is truncated beyond this many bytes before entering context.
    pub max_output_bytes: usize,
    pub max_read_bytes: usize,
    /// Whether `web_fetch` may reach loopback, private, link-local and other
    /// non-public addresses.
    ///
    /// Off by default, and deliberately so: those addresses are services on
    /// this machine or on the network it sits in, not the web, so a fetched URL
    /// that points at one turns a prompt-injected instruction into a local port
    /// scan or a cloud-metadata credential read. Turn it on only for a workspace
    /// whose whole purpose is talking to a service next to it.
    pub web_fetch_allow_private_hosts: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            shell_program: None,
            shell_timeout_secs: 120,
            max_output_bytes: 65_536,
            max_read_bytes: 262_144,
            web_fetch_allow_private_hosts: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Relative paths resolve against the workspace root.
    pub db_path: PathBuf,
    pub embedding_dim: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            db_path: PathBuf::from(HARNESS_DIR).join("harness.db"),
            embedding_dim: 256,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Shared secret a client must present to reach the WebSocket bus, when set.
    ///
    /// `None` leaves the bus open, which is only safe behind a loopback bind:
    /// the bus can drive the `shell` tool, so anything that can reach it can run
    /// commands on this machine. The CLI sets one automatically whenever it is
    /// asked for a non-loopback bind, so exposing the port to a network is an
    /// explicit choice rather than an open door.
    pub token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8787,
            token: None,
        }
    }
}

/// Language the browser UI starts in, when a team has a house language to set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// BCP-47 tag the browser UI starts in. `zh-CN` by default; a user's own
    /// choice in the browser overrides it and is remembered locally.
    pub language: String,
}

/// The tag the UI falls back to, and the one every build ships strings for.
pub const DEFAULT_UI_LANGUAGE: &str = "zh-CN";

/// The BCP-47 tags the UI has a string bundle for.
///
/// A tag outside this set is replaced rather than passed through: a browser
/// handed a language it has no bundle for renders raw translation keys, which
/// looks more broken than the fallback.
pub const SUPPORTED_UI_LANGUAGES: &[&str] = &["zh-CN", "en"];

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            language: DEFAULT_UI_LANGUAGE.to_string(),
        }
    }
}

impl UiConfig {
    /// The configured tag as the UI can render it.
    ///
    /// Matching ignores case so `EN` and `en` are the same request, and the
    /// canonical spelling from [`SUPPORTED_UI_LANGUAGES`] is what comes back.
    pub fn resolved_language(&self) -> &'static str {
        SUPPORTED_UI_LANGUAGES
            .iter()
            .find(|supported| supported.eq_ignore_ascii_case(&self.language))
            .copied()
            .unwrap_or(DEFAULT_UI_LANGUAGE)
    }
}

/// Reasoning-effort levels the gateway accepts.
///
/// The list is a whitelist rather than a pass-through because the gateway
/// answers an unrecognised level with an opaque upstream failure; validating
/// here turns a typo into a value that is simply ignored, and never into a
/// failed run.
pub const SUPPORTED_THINKING_EFFORTS: &[&str] = &["low", "high", "max"];

/// Level used when neither the message nor the agent names one.
pub const DEFAULT_THINKING_EFFORT: &str = "high";

/// Maps a requested level onto one the gateway accepts, ignoring case.
///
/// Returns `None` for anything else — including an empty or blank string —
/// so callers can tell "no usable request" apart from a supported level and
/// fall back deliberately instead of sending the raw text upstream.
pub fn resolve_thinking_effort(requested: &str) -> Option<&'static str> {
    SUPPORTED_THINKING_EFFORTS
        .iter()
        .find(|supported| supported.eq_ignore_ascii_case(requested.trim()))
        .copied()
}

/// How much reasoning a run may spend, when nothing more specific says.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThinkingConfig {
    /// Default level for runs that name none, on the agent or the message.
    pub effort: String,
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self {
            effort: DEFAULT_THINKING_EFFORT.to_string(),
        }
    }
}

impl ThinkingConfig {
    /// The configured level as a value the gateway accepts.
    ///
    /// An unsupported value is replaced rather than passed through, for the
    /// same reason [`resolve_thinking_effort`] exists: the gateway rejects an
    /// unknown level with an error that names nothing useful, so a typo in a
    /// config file must not be able to fail every run.
    pub fn resolved_effort(&self) -> &'static str {
        match resolve_thinking_effort(&self.effort) {
            Some(effort) => effort,
            None => {
                tracing::warn!(
                    configured = %self.effort,
                    default = DEFAULT_THINKING_EFFORT,
                    "unsupported thinking effort in the config; falling back to the default"
                );
                DEFAULT_THINKING_EFFORT
            }
        }
    }

    /// The level a run uses, highest precedence first: the per-message request,
    /// the agent's own declaration, then this config's default.
    ///
    /// A layer that names an unsupported level is skipped rather than allowed
    /// to fail the resolution, so the result is always one of
    /// [`SUPPORTED_THINKING_EFFORTS`] and is safe to put on the wire.
    pub fn resolve(&self, message: Option<&str>, agent: Option<&str>) -> &'static str {
        message
            .and_then(resolve_thinking_effort)
            .or_else(|| agent.and_then(resolve_thinking_effort))
            .unwrap_or_else(|| self.resolved_effort())
    }
}

/// In-flight conversation trimming for the agent loop.
///
/// The loop re-sends the whole conversation every turn, so a long investigation
/// grows until it hits the context limit or spends the budget on tool output the
/// model has already reasoned past. Past the threshold the loop elides the
/// *oldest* tool outputs before building the next request, keeping the recent
/// turns whole. This is the in-flight counterpart of the memory store's trim.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Character budget for the in-flight conversation, across all messages.
    /// `0` disables trimming entirely.
    ///
    /// The default matches `harness_agent::DEFAULT_CONTEXT_TRIM_THRESHOLD`,
    /// which the loop uses when no config is wired in; the two are the same
    /// policy stated once for the CLI/server path and once for a directly built
    /// loop.
    pub trim_threshold_chars: usize,
}

/// The character budget the loop trims at when the config names none.
///
/// `harness-core` cannot depend on `harness-agent` (the dependency runs the
/// other way), so the same number is stated in both crates. A change to
/// `harness_agent::DEFAULT_CONTEXT_TRIM_THRESHOLD` has to be mirrored here.
pub const DEFAULT_CONTEXT_TRIM_THRESHOLD_CHARS: usize = 200_000;

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            trim_threshold_chars: DEFAULT_CONTEXT_TRIM_THRESHOLD_CHARS,
        }
    }
}

/// Model Context Protocol servers, keyed by the name their tools are namespaced
/// under (`mcp__<name>__<tool>`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub servers: BTreeMap<String, McpServerConfig>,
}

impl McpConfig {
    /// Servers that are switched on, in name order.
    pub fn enabled(&self) -> impl Iterator<Item = (&str, &McpServerConfig)> {
        self.servers
            .iter()
            .filter(|(_, server)| server.enabled)
            .map(|(name, server)| (name.as_str(), server))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub enabled: bool,
    /// stdio transport: the program to launch.
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Streamable HTTP transport.
    pub url: Option<String>,
    /// Connect lazily on first tool call instead of at startup.
    pub lazy: bool,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            lazy: true,
        }
    }
}

/// Which wire the client uses to reach a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransportKind {
    Stdio,
    Http,
}

impl McpServerConfig {
    /// Resolves the transport, rejecting a config that names both or neither.
    ///
    /// The two are mutually exclusive because a server reached over HTTP has no
    /// process to launch, and a launched process has no URL to POST to; silently
    /// preferring one would hide a typo in the other.
    pub fn transport_kind(&self) -> Result<McpTransportKind> {
        match (
            non_empty(self.command.as_deref()),
            non_empty(self.url.as_deref()),
        ) {
            (Some(_), None) => Ok(McpTransportKind::Stdio),
            (None, Some(_)) => Ok(McpTransportKind::Http),
            (Some(_), Some(_)) => Err(HarnessError::Config(
                "mcp server sets both `command` and `url`; exactly one transport is allowed".into(),
            )),
            (None, None) => Err(HarnessError::Config(
                "mcp server sets neither `command` nor `url`; one transport is required".into(),
            )),
        }
    }
}

/// What the agent loop screens before content enters the conversation.
///
/// The guards themselves live in `harness-guardrails`; this section only says
/// whether they run and how sensitive the loop detector is.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardrailsConfig {
    /// Runs the deterministic guard set — secrets, PII, injection fences, the
    /// tool policy — over every user message, assistant answer, tool call and
    /// tool result.
    pub enabled: bool,
    /// A tool called more than this many times with identical arguments inside
    /// one run is treated as a loop and refused.
    pub max_identical_tool_calls: usize,
}

impl Default for GuardrailsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_identical_tool_calls: 3,
        }
    }
}

/// Whether a run's result is checked before the run reports success.
///
/// Empty by default: verification is opt-in, so a run that configures nothing is
/// reported as unverified rather than as a success that was never checked.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VerifyConfig {
    /// Shell-free commands run in the workspace after a run, e.g.
    /// `[["cargo", "test"]]`. An empty list with `adversarial = false` means
    /// nothing is checked.
    pub checks: Vec<Vec<String>>,
    /// How long one check may run before it is killed and counted as failed.
    pub timeout_secs: u64,
    /// Ask the run's own model to review the result adversarially.
    pub adversarial: bool,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            checks: Vec::new(),
            timeout_secs: 120,
            adversarial: false,
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

/// Returns `$HARNESS_HOME`, or `~/.harness` when unset.
pub fn global_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("HARNESS_HOME") {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(HARNESS_DIR))
}

/// Returns the per-project harness state directory.
pub fn project_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(HARNESS_DIR)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

impl Config {
    /// Loads the global layer, then the project layer on top of it.
    /// The default layer chain, lowest precedence first.
    ///
    /// A `secrets.toml` sits directly after each `config.toml` so an API key can
    /// be supplied without ever being written into a tracked file.
    pub fn layer_paths(workspace_root: &Path) -> Vec<PathBuf> {
        let project = project_dir(workspace_root);
        let mut layers = Vec::new();
        if let Some(dir) = global_dir() {
            layers.push(dir.join("config.toml"));
            layers.push(dir.join("secrets.toml"));
        }
        layers.push(project.join("config.toml"));
        layers.push(project.join("secrets.toml"));
        layers
    }

    pub fn load(workspace_root: &Path) -> Result<Self> {
        Self::from_files(&Self::layer_paths(workspace_root))
    }

    /// Merges config files in order; missing files are skipped, later files win.
    pub fn from_files(paths: &[PathBuf]) -> Result<Self> {
        let mut merged = toml::Value::Table(Default::default());
        let mut found = false;

        for path in paths {
            let raw = match std::fs::read_to_string(path) {
                Ok(raw) => raw,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(HarnessError::Io(err)),
            };
            let value: toml::Value = toml::from_str(&raw).map_err(|err| {
                HarnessError::Config(format!("failed to parse {}: {err}", path.display()))
            })?;
            deep_merge(&mut merged, value);
            found = true;
        }

        if !found {
            tracing::debug!("no config files found, using defaults");
        }

        merged
            .try_into()
            .map_err(|err| HarnessError::Config(format!("invalid configuration: {err}")))
    }

    /// Resolves the active provider entry.
    pub fn active_provider(&self) -> Result<(&str, &ProviderConfig)> {
        let id = self.provider.default.as_str();
        let config = self.providers.get(id).ok_or_else(|| {
            HarnessError::Config(format!(
                "provider `{id}` is selected but not defined under [providers.{id}]"
            ))
        })?;
        Ok((id, config))
    }

    /// Looks up a provider by id, falling back to the configured default entry.
    pub fn provider(&self, id: Option<&str>) -> Result<(String, &ProviderConfig)> {
        match id {
            None => self
                .active_provider()
                .map(|(id, cfg)| (id.to_string(), cfg)),
            Some(requested) => match self.providers.get(requested) {
                Some(cfg) => Ok((requested.to_string(), cfg)),
                None => Err(HarnessError::Config(format!(
                    "unknown provider `{requested}`; define [providers.{requested}]"
                ))),
            },
        }
    }

    /// Resolves the effective model for a provider.
    pub fn model_for(
        &self,
        provider: &ProviderConfig,
        override_model: Option<&str>,
    ) -> Result<String> {
        override_model
            .map(str::to_string)
            .or_else(|| provider.default_model.clone())
            .ok_or_else(|| {
                HarnessError::Config(
                    "no model configured; set `default_model` on the provider or pass --model"
                        .into(),
                )
            })
    }

    /// Workspace root: config value if set, otherwise the process working directory.
    pub fn workspace_root(&self) -> Result<PathBuf> {
        match &self.harness.workspace_root {
            Some(path) if path.is_absolute() => Ok(path.clone()),
            Some(path) => std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .map_err(HarnessError::Io),
            None => std::env::current_dir().map_err(HarnessError::Io),
        }
    }

    /// Absolute path to the memory database.
    pub fn db_path(&self, workspace_root: &Path) -> PathBuf {
        if self.memory.db_path.is_absolute() {
            self.memory.db_path.clone()
        } else {
            workspace_root.join(&self.memory.db_path)
        }
    }
}

/// Recursively merges `over` into `base`; tables merge key-wise, scalars replace.
pub fn deep_merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(base_table), toml::Value::Table(over_table)) => {
            for (key, over_value) in over_table {
                match base_table.get_mut(&key) {
                    Some(base_value) => deep_merge(base_value, over_value),
                    None => {
                        base_table.insert(key, over_value);
                    }
                }
            }
        }
        (base_slot, over_value) => *base_slot = over_value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn project_layer_overrides_global_and_tables_merge() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join("global.toml");
        let project = tmp.path().join("project.toml");

        write(
            &global,
            r#"
[harness]
default_agent = "global-agent"

[providers.openai]
default_model = "gpt-4o-mini"

[providers.local]
base_url = "http://localhost:11434/v1"
"#,
        );
        write(
            &project,
            r#"
[harness]
default_agent = "project-agent"

[providers.openai]
default_model = "gpt-4o"
"#,
        );

        let config = Config::from_files(&[global, project]).unwrap();

        assert_eq!(config.harness.default_agent, "project-agent");
        assert_eq!(
            config.providers["openai"].default_model.as_deref(),
            Some("gpt-4o")
        );
        // The provider defined only in the global layer survives the merge.
        assert_eq!(
            config.providers["local"].base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
    }

    #[test]
    fn missing_files_fall_back_to_defaults() {
        let config = Config::from_files(&[PathBuf::from("does/not/exist.toml")]).unwrap();
        assert_eq!(config.harness.max_iterations, 64);
        assert_eq!(config.server.port, 8787);
        assert!(config.providers.is_empty());
    }

    #[test]
    fn selecting_an_undefined_provider_is_an_error() {
        let config = Config::default();
        let err = config.active_provider().unwrap_err();
        assert!(err.to_string().contains("not defined"), "{err}");
    }

    #[test]
    fn api_key_is_read_from_the_named_env_var() {
        let var = "HARNESS_TEST_API_KEY_CONFIG_RS";
        std::env::set_var(var, "sk-test-123");

        let provider = ProviderConfig {
            api_key_env: Some(var.to_string()),
            ..Default::default()
        };
        assert_eq!(provider.api_key().unwrap().as_deref(), Some("sk-test-123"));
        assert_eq!(provider.base_url(), DEFAULT_OPENAI_BASE_URL);

        let unset = ProviderConfig {
            api_key_env: Some("HARNESS_TEST_API_KEY_DEFINITELY_UNSET".into()),
            ..Default::default()
        };
        assert!(unset.api_key().unwrap().is_none());

        let anonymous = ProviderConfig::default();
        assert!(anonymous.api_key().unwrap().is_none());

        std::env::remove_var(var);
    }

    #[test]
    fn gateway_config_spelling_is_accepted() {
        let parsed: Config = toml::from_str(
            r#"
[providers.opencode-go]
base_url = "https://opencode.ai/zen/go/v1"
type = "openai"
api_key = "sk-inline"
default_model = "some-model"

[providers.opencode-go.custom_headers]
X-Opencode-Session = "cf7e2cf3"

[providers.local]
type = "mock"
"#,
        )
        .unwrap();

        let gateway = &parsed.providers["opencode-go"];
        assert_eq!(gateway.kind, ProviderKind::OpenAi);
        assert_eq!(gateway.base_url(), "https://opencode.ai/zen/go/v1");
        assert_eq!(
            gateway
                .custom_headers
                .get("X-Opencode-Session")
                .map(String::as_str),
            Some("cf7e2cf3")
        );
        assert_eq!(parsed.providers["local"].kind, ProviderKind::Mock);
    }

    #[test]
    fn an_inline_key_takes_precedence_over_the_environment() {
        let var = "HARNESS_TEST_INLINE_KEY_PRECEDENCE";
        std::env::set_var(var, "from-env");

        let both = ProviderConfig {
            api_key: Some("from-file".into()),
            api_key_env: Some(var.into()),
            ..Default::default()
        };
        assert_eq!(both.api_key().unwrap().as_deref(), Some("from-file"));

        // A blank inline value must not shadow a usable environment variable.
        let blank_inline = ProviderConfig {
            api_key: Some("   ".into()),
            api_key_env: Some(var.into()),
            ..Default::default()
        };
        assert_eq!(blank_inline.api_key().unwrap().as_deref(), Some("from-env"));

        std::env::remove_var(var);
    }

    #[test]
    fn relative_db_path_resolves_against_workspace() {
        let config = Config::default();
        let resolved = config.db_path(Path::new("/work"));
        assert!(resolved.ends_with("harness.db"));
        assert!(
            resolved.starts_with("/work")
                || resolved.starts_with("\\work")
                || resolved.to_string_lossy().contains("work")
        );
    }

    #[test]
    fn mcp_servers_parse_stdio_and_http_transports() {
        let parsed: Config = toml::from_str(
            r#"
[mcp.servers.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[mcp.servers.files.env]
LOG_LEVEL = "debug"

[mcp.servers.remote]
url = "https://example.com/mcp"
lazy = false

[mcp.servers.off]
command = "agent-mcp"
enabled = false
"#,
        )
        .unwrap();

        let files = &parsed.mcp.servers["files"];
        assert_eq!(files.transport_kind().unwrap(), McpTransportKind::Stdio);
        assert_eq!(files.command.as_deref(), Some("npx"));
        assert_eq!(files.args.len(), 3);
        assert!(files.enabled, "enabled defaults to true");
        assert!(files.lazy, "lazy defaults to true");
        assert_eq!(
            files.env.get("LOG_LEVEL").map(String::as_str),
            Some("debug")
        );

        let remote = &parsed.mcp.servers["remote"];
        assert_eq!(remote.transport_kind().unwrap(), McpTransportKind::Http);
        assert_eq!(remote.url.as_deref(), Some("https://example.com/mcp"));
        assert!(!remote.lazy);
        assert!(remote.args.is_empty());

        assert!(!parsed.mcp.servers["off"].enabled);
        assert_eq!(parsed.mcp.enabled().count(), 2);
    }

    #[test]
    fn an_mcp_server_must_choose_exactly_one_transport() {
        let both = McpServerConfig {
            command: Some("npx".into()),
            url: Some("https://example.com/mcp".into()),
            ..Default::default()
        };
        let err = both.transport_kind().unwrap_err();
        assert!(matches!(err, HarnessError::Config(_)), "{err}");
        assert!(err.to_string().contains("both"), "{err}");

        let neither = McpServerConfig::default();
        let err = neither.transport_kind().unwrap_err();
        assert!(err.to_string().contains("neither"), "{err}");

        // A blank field is an absent field, not an empty program or URL.
        let blank = McpServerConfig {
            command: Some("   ".into()),
            ..Default::default()
        };
        assert!(blank.transport_kind().is_err());
    }

    #[test]
    fn guardrail_settings_parse_and_fall_back_to_defaults() {
        let parsed: Config = toml::from_str(
            r#"
[guardrails]
enabled = false
max_identical_tool_calls = 5
"#,
        )
        .unwrap();
        assert!(!parsed.guardrails.enabled);
        assert_eq!(parsed.guardrails.max_identical_tool_calls, 5);

        // A config that never mentions guardrails keeps them switched on.
        let bare = Config::default();
        assert!(bare.guardrails.enabled);
        assert_eq!(bare.guardrails.max_identical_tool_calls, 3);

        // A partial section fills the rest from the default.
        let partial: Config = toml::from_str("[guardrails]\nenabled = false\n").unwrap();
        assert!(!partial.guardrails.enabled);
        assert_eq!(partial.guardrails.max_identical_tool_calls, 3);
    }

    #[test]
    fn verify_settings_parse_and_fall_back_to_defaults() {
        let parsed: Config = toml::from_str(
            r#"
[verify]
checks = [["cargo", "test"]]
timeout_secs = 30
adversarial = true
"#,
        )
        .unwrap();
        assert_eq!(
            parsed.verify.checks,
            vec![vec!["cargo".to_string(), "test".to_string()]]
        );
        assert_eq!(parsed.verify.timeout_secs, 30);
        assert!(parsed.verify.adversarial);

        // Nothing configured means nothing is checked: verification is opt-in.
        let bare = Config::default();
        assert!(bare.verify.checks.is_empty());
        assert!(!bare.verify.adversarial);
        assert_eq!(bare.verify.timeout_secs, 120);

        // A partial section fills the rest from the default.
        let partial: Config =
            toml::from_str("[verify]\nchecks = [[\"git\", \"--version\"]]\n").unwrap();
        assert_eq!(partial.verify.checks.len(), 1);
        assert_eq!(partial.verify.timeout_secs, 120);
        assert!(!partial.verify.adversarial);
    }

    #[test]
    fn ui_language_parses_and_an_absent_section_defaults_to_chinese() {
        let parsed: Config = toml::from_str("[ui]\nlanguage = \"en\"\n").unwrap();
        assert_eq!(parsed.ui.language, "en");

        // A config that never mentions `[ui]` still names a language the UI ships.
        let bare = Config::default();
        assert_eq!(bare.ui.language, DEFAULT_UI_LANGUAGE);
    }

    #[test]
    fn a_ui_language_the_ui_cannot_render_falls_back() {
        let configured = |tag: &str| UiConfig {
            language: tag.to_string(),
        };

        assert_eq!(configured("en").resolved_language(), "en");
        assert_eq!(configured("EN").resolved_language(), "en");
        assert_eq!(configured("zh-cn").resolved_language(), "zh-CN");
        assert_eq!(configured("fr").resolved_language(), DEFAULT_UI_LANGUAGE);
        assert_eq!(configured("").resolved_language(), DEFAULT_UI_LANGUAGE);
        assert_eq!(UiConfig::default().resolved_language(), DEFAULT_UI_LANGUAGE);
    }

    #[test]
    fn mcp_servers_merge_across_layers() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join("global.toml");
        let project = tmp.path().join("project.toml");

        write(
            &global,
            r#"
[mcp.servers.files]
command = "npx"
args = ["-y", "server-filesystem"]
"#,
        );
        write(
            &project,
            r#"
[mcp.servers.files]
args = ["-y", "server-filesystem", "/work"]

[mcp.servers.remote]
url = "http://127.0.0.1:9999/mcp"
"#,
        );

        let config = Config::from_files(&[global, project]).unwrap();
        let files = &config.mcp.servers["files"];
        assert_eq!(files.command.as_deref(), Some("npx"));
        assert_eq!(files.args, vec!["-y", "server-filesystem", "/work"]);
        assert_eq!(
            config.mcp.servers["remote"].transport_kind().unwrap(),
            McpTransportKind::Http
        );
    }

    #[test]
    fn thinking_effort_maps_case_insensitively_and_rejects_anything_else() {
        assert_eq!(resolve_thinking_effort("low"), Some("low"));
        assert_eq!(resolve_thinking_effort("LOW"), Some("low"));
        assert_eq!(resolve_thinking_effort("max"), Some("max"));
        assert_eq!(resolve_thinking_effort("Max"), Some("max"));
        assert_eq!(resolve_thinking_effort(" high "), Some("high"));
        assert_eq!(resolve_thinking_effort("bogus"), None);
        assert_eq!(resolve_thinking_effort(""), None);
        assert_eq!(resolve_thinking_effort("   "), None);
    }

    #[test]
    fn the_thinking_section_parses_and_defaults_to_high() {
        let parsed: Config = toml::from_str("[thinking]\neffort = \"max\"\n").unwrap();
        assert_eq!(parsed.thinking.effort, "max");
        assert_eq!(parsed.thinking.resolved_effort(), "max");

        // A config that never mentions `[thinking]` still names a level the
        // gateway accepts.
        let bare = Config::default();
        assert_eq!(bare.thinking.effort, DEFAULT_THINKING_EFFORT);
        assert_eq!(bare.thinking.resolved_effort(), DEFAULT_THINKING_EFFORT);
    }

    #[test]
    fn an_unsupported_configured_effort_falls_back_to_the_default() {
        let config = ThinkingConfig {
            effort: "bogus".to_string(),
        };
        assert_eq!(config.resolved_effort(), DEFAULT_THINKING_EFFORT);
        assert_eq!(config.resolve(None, None), DEFAULT_THINKING_EFFORT);
    }

    #[test]
    fn a_message_effort_outranks_the_agent_and_the_config() {
        let config = ThinkingConfig {
            effort: "low".to_string(),
        };

        // Per-message wins over the agent's declaration, which wins over the
        // configured default.
        assert_eq!(config.resolve(Some("max"), Some("low")), "max");
        assert_eq!(config.resolve(None, Some("low")), "low");
        assert_eq!(config.resolve(None, None), "low");

        // An unrecognised request is skipped rather than sent: the next tier
        // answers, and the result is always a supported level.
        assert_eq!(config.resolve(Some("bogus"), Some("max")), "max");
        assert_eq!(config.resolve(Some("bogus"), Some("bogus")), "low");
    }

    #[test]
    fn the_context_section_parses_and_defaults_to_the_loop_threshold() {
        let parsed: Config = toml::from_str("[context]\ntrim_threshold_chars = 0\n").unwrap();
        assert_eq!(parsed.context.trim_threshold_chars, 0);

        // A config that never mentions `[context]` keeps the loop's own default,
        // so trimming is on unless an operator turns it off.
        let bare = Config::default();
        assert_eq!(
            bare.context.trim_threshold_chars,
            DEFAULT_CONTEXT_TRIM_THRESHOLD_CHARS
        );

        // A partial section fills the rest from the default.
        let partial: Config = toml::from_str("[context]\n").unwrap();
        assert_eq!(
            partial.context.trim_threshold_chars,
            DEFAULT_CONTEXT_TRIM_THRESHOLD_CHARS
        );
    }
}
