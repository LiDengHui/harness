//! `harness skill`: inspect the skills declared by `SKILL.md` files.
//!
//! The listing prints both costs of every skill because that is the whole
//! progressive-disclosure argument: the index is what the model pays up front,
//! the body is what it pays only once the skill is activated.

use harness_core::HeuristicEstimator;
use harness_skills::{search_dirs, SkillRegistry};

use crate::commands::SkillCommand;
use crate::context::AppContext;

pub(crate) async fn execute(command: SkillCommand, ctx: &AppContext) -> anyhow::Result<()> {
    let registry =
        SkillRegistry::load(&ctx.workspace_root).map_err(|err| anyhow::anyhow!("{err}"))?;

    match command {
        SkillCommand::List => list(&registry, ctx),
        SkillCommand::Show { name } => show(&registry, ctx, &name),
    }
}

const COLUMNS: [&str; 5] = ["skill", "meta", "body", "saved", "source"];

fn list(registry: &SkillRegistry, ctx: &AppContext) -> anyhow::Result<()> {
    let specs = registry.list();
    if specs.is_empty() {
        eprintln!("no skills found; searched:");
        for dir in search_dirs(&ctx.workspace_root) {
            eprintln!("  {}", dir.display());
        }
        return Ok(());
    }

    let estimator = HeuristicEstimator::default();
    let mut rows: Vec<Vec<String>> =
        vec![COLUMNS.iter().map(|column| column.to_string()).collect()];
    rows.extend(specs.iter().map(|spec| {
        let metadata = spec.metadata_tokens(&estimator);
        let body = spec.body_tokens(&estimator);
        vec![
            spec.name.clone(),
            metadata.to_string(),
            body.to_string(),
            saving(metadata, body),
            spec.source_path.display().to_string(),
        ]
    }));

    print!("{}", table(&rows));
    Ok(())
}

/// How many times cheaper the always-injected index is than the body. `-` when
/// the metadata is free, because the ratio would be meaningless.
fn saving(metadata: usize, body: usize) -> String {
    if metadata == 0 {
        return "-".to_string();
    }
    format!("{:.1}x", body as f64 / metadata as f64)
}

fn show(registry: &SkillRegistry, ctx: &AppContext, name: &str) -> anyhow::Result<()> {
    let Some(spec) = registry.get(name) else {
        return Err(unknown_skill(registry, ctx, name));
    };

    let estimator = HeuristicEstimator::default();
    let metadata = spec.metadata_tokens(&estimator);
    let body = spec.body_tokens(&estimator);

    field("name", &spec.name);
    field("description", &spec.description);
    field("model", spec.model.as_deref().unwrap_or("(agent default)"));
    field(
        "allowed-tools",
        &join_or(&spec.allowed_tools, "no narrowing"),
    );
    field("source", &spec.source_path.display().to_string());
    field(
        "cost",
        &format!(
            "{metadata} indexed + {body} on activation = {}",
            metadata + body
        ),
    );

    println!();
    println!("--- instructions ---");
    println!("{}", spec.instructions);
    Ok(())
}

fn field(label: &str, value: &str) {
    println!("{label:<14}{value}");
}

fn join_or(values: &[String], empty: &str) -> String {
    if values.is_empty() {
        empty.to_string()
    } else {
        values.join(", ")
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

/// Reports an unknown name with the available ones, or with the directories
/// that were searched when there are none — a typo and an empty search path
/// need different fixes.
fn unknown_skill(registry: &SkillRegistry, ctx: &AppContext, name: &str) -> anyhow::Error {
    let mut message = format!("skill `{name}` not found");
    if registry.is_empty() {
        message.push_str("; no skills found, searched:");
        for dir in search_dirs(&ctx.workspace_root) {
            message.push_str(&format!("\n  {}", dir.display()));
        }
    } else {
        message.push_str(&format!(
            "; available skills: {}",
            registry.names().join(", ")
        ));
    }
    anyhow::anyhow!("{message}")
}
