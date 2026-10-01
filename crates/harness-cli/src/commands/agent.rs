//! `harness agent`: inspect the declarative agent definitions on disk.

use harness_agent::{search_dirs, AgentRegistry, AgentSpec, ModelRouter};

use crate::commands::AgentCommand;
use crate::context::AppContext;

pub(crate) async fn execute(command: AgentCommand, ctx: &AppContext) -> anyhow::Result<()> {
    let registry =
        AgentRegistry::load(&ctx.workspace_root).map_err(|err| anyhow::anyhow!("{err}"))?;

    match command {
        AgentCommand::List => list(&registry, ctx),
        AgentCommand::Show { id } => show(&registry, ctx, &id),
    }
}

const COLUMNS: [&str; 6] = ["id", "model", "tools", "skills", "budget", "source"];

fn list(registry: &AgentRegistry, ctx: &AppContext) -> anyhow::Result<()> {
    let specs = registry.list();
    if specs.is_empty() {
        eprintln!("no agents found; searched:");
        for dir in search_dirs(&ctx.workspace_root) {
            eprintln!("  {}", dir.display());
        }
        return Ok(());
    }

    let mut rows: Vec<Vec<String>> = vec![COLUMNS.iter().map(|c| c.to_string()).collect()];
    rows.extend(specs.iter().map(|spec| {
        vec![
            spec.id.clone(),
            spec.model.clone().unwrap_or_else(|| "(inherit)".into()),
            tool_count(spec),
            spec.skills.len().to_string(),
            spec.max_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "unbounded".into()),
            spec.source_path.display().to_string(),
        ]
    }));

    print!("{}", table(&rows));
    Ok(())
}

/// An empty whitelist is every registered tool, which a count would misreport.
fn tool_count(spec: &AgentSpec) -> String {
    if spec.tools.is_empty() {
        "all".to_string()
    } else {
        spec.tools.len().to_string()
    }
}

fn table(rows: &[Vec<String>]) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    for row in rows {
        let line = row
            .iter()
            .enumerate()
            .map(|(index, cell)| format!("{cell:<width$}", width = widths[index]))
            .collect::<Vec<_>>()
            .join("  ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn show(registry: &AgentRegistry, ctx: &AppContext, id: &str) -> anyhow::Result<()> {
    let Some(spec) = registry.get(id) else {
        return Err(unknown_agent(registry, ctx, id));
    };

    let model = ModelRouter::new(ctx.config.clone()).resolve(Some(spec), None, &[]);
    let model = match model {
        Ok(resolved) => format!(
            "{}/{} (source: {})",
            resolved.provider_id,
            resolved.model,
            resolved.source.as_str()
        ),
        // The resolution error is the useful part — typically a provider id that
        // is not defined in the configuration.
        Err(err) => format!("unresolved ({err})"),
    };

    field("id", &spec.id);
    field("name", &spec.name);
    field("description", &spec.description);
    field("source", &spec.source_path.display().to_string());
    field("model", &model);
    field(
        "temperature",
        &spec
            .temperature
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(provider default)".into()),
    );
    field(
        "max_tokens",
        &match (spec.max_tokens, spec.fallback_model.as_deref()) {
            (Some(tokens), Some(fallback)) => format!("{tokens} (fallback: {fallback})"),
            (Some(tokens), None) => tokens.to_string(),
            (None, Some(fallback)) => format!("unbounded (fallback: {fallback})"),
            (None, None) => "unbounded".to_string(),
        },
    );
    field("tools", &join_or(&spec.tools, "all registered tools"));
    field("skills", &join_or(&spec.skills, "none"));
    field("subagents", &join_or(&spec.subagents, "none"));

    println!();
    println!("--- system prompt ---");
    println!("{}", spec.system_prompt);
    Ok(())
}

fn field(label: &str, value: &str) {
    println!("{label:<12}{value}");
}

fn join_or(values: &[String], empty: &str) -> String {
    if values.is_empty() {
        empty.to_string()
    } else {
        values.join(", ")
    }
}

/// Reports an unknown id with both the available ids and the directories that
/// were searched, so a typo is distinguishable from an empty search path.
fn unknown_agent(registry: &AgentRegistry, ctx: &AppContext, id: &str) -> anyhow::Error {
    let mut message = format!("agent `{id}` not found");
    if registry.is_empty() {
        message.push_str("; no agents found, searched:");
        for dir in search_dirs(&ctx.workspace_root) {
            message.push_str(&format!("\n  {}", dir.display()));
        }
    } else {
        let available: Vec<&str> = registry
            .list()
            .iter()
            .map(|spec| spec.id.as_str())
            .collect();
        message.push_str(&format!("; available agents: {}", available.join(", ")));
    }
    anyhow::anyhow!("{message}")
}
