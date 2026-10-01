use std::path::PathBuf;

use harness_core::{path, Config, Result};

/// Everything a command needs to run: where we are and how we are configured.
#[derive(Debug, Clone)]
pub struct AppContext {
    pub workspace_root: PathBuf,
    pub config: Config,
}

impl AppContext {
    /// Loads configuration for `workspace_root`, layering any extra files last.
    pub fn load(workspace_root: PathBuf, extra_configs: &[PathBuf]) -> Result<Self> {
        // Canonicalizing here is what makes the tools layer's containment check
        // meaningful, so it happens once, up front.
        let workspace_root = path::canonicalize(&workspace_root);

        let mut layers = Config::layer_paths(&workspace_root);
        layers.extend(extra_configs.iter().cloned());

        Ok(Self {
            workspace_root,
            config: Config::from_files(&layers)?,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.config.db_path(&self.workspace_root)
    }
}
