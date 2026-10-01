use std::path::PathBuf;

/// Everything that can go wrong inside the harness.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("provider `{provider}` error: {message}")]
    Provider { provider: String, message: String },

    #[error("tool error: {0}")]
    Tool(String),

    #[error("unknown tool `{0}`")]
    ToolNotFound(String),

    #[error("tool call `{0}` was rejected")]
    ToolRejected(String),

    #[error("path escapes the workspace root: {}", .0.display())]
    PathEscape(PathBuf),

    #[error("agent `{0}` not found")]
    AgentNotFound(String),

    #[error("invalid agent spec `{}`: {message}", .path.display())]
    InvalidAgentSpec { path: PathBuf, message: String },

    #[error("skill `{0}` not found")]
    SkillNotFound(String),

    #[error("memory error: {0}")]
    Memory(String),

    #[error("token budget exceeded: {0}")]
    BudgetExceeded(String),

    #[error("sandbox error: {0}")]
    Sandbox(String),

    #[error("mcp error: {0}")]
    Mcp(String),

    #[error("guardrail `{name}` blocked the operation: {detail}")]
    GuardrailBlocked { name: String, detail: String },

    #[error("aborted: {0}")]
    Aborted(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("yaml error: {0}")]
    Yaml(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = HarnessError> = std::result::Result<T, E>;

impl From<serde_norway::Error> for HarnessError {
    fn from(err: serde_norway::Error) -> Self {
        HarnessError::Yaml(err.to_string())
    }
}
