//! What a plugin declares about itself, and what the host is willing to grant.

use serde::{Deserialize, Serialize};

use harness_core::{HarnessError, Result};

/// What a plugin is allowed to do. Enforced by the host, not by the plugin.
///
/// The plugin never sees this list: it can only find out about it by calling a
/// host function and being refused. That direction matters — a plugin binary is
/// untrusted input, so every check has to live on the host side of the boundary,
/// where the guest cannot reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    FileRead,
    FileWrite,
    NetworkAccess,
    ShellExec,
}

impl Capability {
    /// The manifest spelling of this capability, e.g. `"file_read"`.
    ///
    /// Spelled out rather than derived from the serde rename so that error
    /// messages and manifest parsing cannot drift apart silently; a test pins
    /// the two together.
    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::FileRead => "file_read",
            Capability::FileWrite => "file_write",
            Capability::NetworkAccess => "network_access",
            Capability::ShellExec => "shell_exec",
        }
    }

    /// The host function this capability unlocks, or `None` when nothing yet
    /// needs it. [`Capability::ShellExec`] is such a reservation: the enum
    /// exists so manifests can be written ahead of the `harness.exec` import,
    /// and declaring it currently grants nothing.
    pub const fn host_function(self) -> Option<&'static str> {
        match self {
            Capability::FileRead => Some("harness.read_file"),
            Capability::FileWrite => Some("harness.write_file"),
            Capability::NetworkAccess => Some("harness.http_get"),
            Capability::ShellExec => None,
        }
    }
}

/// A plugin's self-description. Supplied by the caller, because the host has no
/// way to turn a plugin's own claims into a trust decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    /// Exported function to call, e.g. `"run"`.
    pub entry: String,
    pub capabilities: Vec<Capability>,
}

impl PluginManifest {
    /// Parses a manifest from `.plugin.toml` text.
    pub fn from_toml(text: &str) -> Result<Self> {
        let manifest: PluginManifest = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Rejects manifests the host could not act on sensibly.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(HarnessError::Sandbox(
                "plugin manifest has an empty name".to_string(),
            ));
        }
        if self.entry.trim().is_empty() {
            return Err(HarnessError::Sandbox(format!(
                "plugin `{}` has an empty entry point",
                self.name
            )));
        }
        Ok(())
    }

    /// True when the manifest grants `capability`.
    pub fn grants(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_names_match_their_serde_spelling() {
        for capability in [
            Capability::FileRead,
            Capability::FileWrite,
            Capability::NetworkAccess,
            Capability::ShellExec,
        ] {
            let json = serde_json::to_string(&capability).unwrap();
            assert_eq!(json, format!("\"{}\"", capability.as_str()));
            assert_eq!(
                serde_json::from_str::<Capability>(&json).unwrap(),
                capability
            );
        }
    }

    #[test]
    fn manifests_round_trip_through_toml() {
        let manifest = PluginManifest::from_toml(
            r#"
            name = "reader"
            version = "1.2.3"
            entry = "run"
            capabilities = ["file_read", "network_access"]
            "#,
        )
        .unwrap();

        assert_eq!(manifest.name, "reader");
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(manifest.entry, "run");
        assert!(manifest.grants(Capability::FileRead));
        assert!(manifest.grants(Capability::NetworkAccess));
        assert!(!manifest.grants(Capability::FileWrite));
    }

    #[test]
    fn unknown_capabilities_are_rejected_rather_than_ignored() {
        let err = PluginManifest::from_toml(
            r#"
            name = "typo"
            version = "1.0.0"
            entry = "run"
            capabilities = ["file_reed"]
            "#,
        )
        .unwrap_err();

        assert!(matches!(err, HarnessError::Toml(_)), "{err}");
    }

    #[test]
    fn empty_names_and_entry_points_are_refused() {
        let manifest = PluginManifest {
            name: "  ".to_string(),
            version: "1.0.0".to_string(),
            entry: "run".to_string(),
            capabilities: Vec::new(),
        };
        assert!(manifest.validate().is_err());

        let manifest = PluginManifest {
            name: "ok".to_string(),
            version: "1.0.0".to_string(),
            entry: String::new(),
            capabilities: Vec::new(),
        };
        assert!(manifest.validate().is_err());
    }
}
