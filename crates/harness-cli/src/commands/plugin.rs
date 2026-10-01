//! `harness plugin`: inspect and run WASM plugins from `plugins/`.
//!
//! A plugin is a `<name>.wat` (or `.wasm`) module beside a `<name>.plugin.toml`
//! manifest declaring the capabilities it needs. The host grants nothing else,
//! so a plugin's blast radius is visible in one file.

use std::path::{Path, PathBuf};

use harness_sandbox::{PluginHost, PluginManifest, SandboxLimits};

use crate::commands::PluginCommand;
use crate::context::AppContext;

const PLUGIN_DIR: &str = "plugins";

struct Discovered {
    manifest: PluginManifest,
    module: PathBuf,
}

pub(crate) async fn execute(command: PluginCommand, ctx: &AppContext) -> anyhow::Result<()> {
    match command {
        PluginCommand::List => list(ctx),
        PluginCommand::Run { name, json } => run(ctx, &name, &json).await,
    }
}

/// Finds `<dir>/<name>.plugin.toml` pairs. A missing directory is not an error.
fn discover(root: &Path) -> anyhow::Result<Vec<Discovered>> {
    let dir = root.join(PLUGIN_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(anyhow::anyhow!("failed to read {}: {err}", dir.display())),
    };

    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(name) = stem.strip_suffix(".plugin") else {
            continue;
        };

        let text = std::fs::read_to_string(&path)?;
        let manifest = PluginManifest::from_toml(&text)
            .map_err(|err| anyhow::anyhow!("{}: {err}", path.display()))?;

        let module = ["wat", "wasm"]
            .iter()
            .map(|ext| dir.join(format!("{name}.{ext}")))
            .find(|candidate| candidate.exists())
            .ok_or_else(|| {
                anyhow::anyhow!("plugin `{name}` has a manifest but no .wat or .wasm module")
            })?;

        found.push(Discovered { manifest, module });
    }

    found.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    Ok(found)
}

fn list(ctx: &AppContext) -> anyhow::Result<()> {
    let found = discover(&ctx.workspace_root)?;
    if found.is_empty() {
        println!(
            "no plugins found in {}",
            ctx.workspace_root.join(PLUGIN_DIR).display()
        );
        return Ok(());
    }

    println!(
        "{:<14} {:<9} {:<8} capabilities",
        "plugin", "version", "entry"
    );
    for item in found {
        let capabilities = if item.manifest.capabilities.is_empty() {
            "none".to_string()
        } else {
            item.manifest
                .capabilities
                .iter()
                .map(|capability| format!("{capability:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "{:<14} {:<9} {:<8} {capabilities}",
            item.manifest.name, item.manifest.version, item.manifest.entry
        );
    }
    Ok(())
}

async fn run(ctx: &AppContext, name: &str, json: &str) -> anyhow::Result<()> {
    let found = discover(&ctx.workspace_root)?;
    let item = found
        .into_iter()
        .find(|item| item.manifest.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown plugin `{name}`"))?;

    let mut host = PluginHost::new(ctx.workspace_root.clone(), SandboxLimits::default())
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    host.load(item.manifest, &item.module)
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    let output = host
        .call(name, json)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    for line in host.logs() {
        eprintln!("[log] {line}");
    }
    println!("{output}");
    Ok(())
}
