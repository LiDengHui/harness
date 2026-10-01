//! `dhs workflow`: inspect the workflow procedures this workspace declares.
//!
//! Read-only: authoring a workflow is `POST /api/workflows`, where a model
//! writes the file. This command exists so the same registry the selector and
//! the server read can be seen, and checked, from the terminal.

use std::collections::BTreeSet;

use harness_agent::AgentRegistry;
use harness_workflows::{WorkflowRegistry, WorkflowSpec};

use crate::commands::WorkflowCommand;
use crate::context::AppContext;
use crate::workflows::load_registry;

pub(crate) async fn execute(command: WorkflowCommand, ctx: &AppContext) -> anyhow::Result<()> {
    match command {
        WorkflowCommand::List => list(ctx),
        WorkflowCommand::Show { id } => show(ctx, &id),
        WorkflowCommand::Validate { id } => validate(ctx, id.as_deref()),
    }
}

fn list(ctx: &AppContext) -> anyhow::Result<()> {
    let registry = load_registry(&ctx.workspace_root).map_err(|err| anyhow::anyhow!("{err}"))?;
    if registry.is_empty() {
        println!(
            "no workflows found under {}",
            ctx.workspace_root.join("workflows").display()
        );
        return Ok(());
    }

    for spec in registry.list() {
        println!("{}  {}", spec.id, spec.name);
        println!("  when:   {}", spec.when);
        println!("  what:   {}", spec.description);
        println!("  stages: {}", spec.stages.len());
        println!("  file:   {}", spec.source_path.display());
    }
    Ok(())
}

/// Prints one workflow in full: the picker's fields, then every stage.
///
/// The stages are the part `list` deliberately omits — they are the run's
/// business — so `show` is the command that answers "what does this actually
/// do".
fn show(ctx: &AppContext, id: &str) -> anyhow::Result<()> {
    // The structural load is used rather than `load_registry` so a workspace
    // whose *other* workflows are broken can still show this one; the agent
    // check is reported below instead of refusing the whole command.
    let registry =
        WorkflowRegistry::load(&ctx.workspace_root).map_err(|err| anyhow::anyhow!("{err}"))?;
    let spec = registry
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("no workflow `{id}`; known: {}", known_ids(&registry)))?;

    println!("{}  {}", spec.id, spec.name);
    println!("  when:        {}", spec.when);
    println!("  description: {}", spec.description);
    println!("  file:        {}", spec.source_path.display());

    for (index, stage) in spec.stages.iter().enumerate() {
        println!();
        println!("  stage {}: {}", index + 1, stage.id);
        println!("    objective: {}", stage.objective);
        println!(
            "    agent:     {}",
            stage.agent.as_deref().unwrap_or("(default)")
        );
        if !stage.depends_on.is_empty() {
            println!("    after:     {}", stage.depends_on.join(", "));
        }
        for command in &stage.verify {
            println!("    verify:    {}", command.join(" "));
        }
    }

    println!();
    println!("  guidance:");
    for line in spec.guidance.lines() {
        println!("    {line}");
    }
    Ok(())
}

/// Checks every workflow (or one) and reports the first thing wrong with each.
///
/// This reports all the failures rather than stopping at the first, which is
/// the opposite of the loader's behaviour and deliberate: the loader refuses a
/// workspace at startup and must name one file, while `validate` exists to tell
/// a user everything that needs fixing in one pass.
fn validate(ctx: &AppContext, only: Option<&str>) -> anyhow::Result<()> {
    let registry =
        WorkflowRegistry::load(&ctx.workspace_root).map_err(|err| anyhow::anyhow!("{err}"))?;
    let agents =
        AgentRegistry::load(&ctx.workspace_root).map_err(|err| anyhow::anyhow!("{err}"))?;
    let known: BTreeSet<String> = agents.list().iter().map(|spec| spec.id.clone()).collect();

    let specs: Vec<&WorkflowSpec> = match only {
        Some(id) => vec![registry.get(id).ok_or_else(|| {
            anyhow::anyhow!("no workflow `{id}`; known: {}", known_ids(&registry))
        })?],
        None => registry.list(),
    };

    if specs.is_empty() {
        println!(
            "no workflows found under {}",
            ctx.workspace_root.join("workflows").display()
        );
        return Ok(());
    }

    let mut failures = 0usize;
    for spec in &specs {
        match check(spec, &known) {
            Ok(()) => println!("ok    {}", spec.id),
            Err(err) => {
                failures += 1;
                println!("FAIL  {}", spec.id);
                println!("      {err}");
            }
        }
    }

    if failures > 0 {
        anyhow::bail!("{failures} of {} workflow(s) are invalid", specs.len());
    }
    println!(
        "\n{} workflow(s) valid; {} agent(s) available",
        specs.len(),
        known.len()
    );
    Ok(())
}

/// The two checks a workflow must pass to be runnable: a valid stage graph and
/// agents that exist.
fn check(spec: &WorkflowSpec, known: &BTreeSet<String>) -> harness_core::Result<()> {
    spec.to_graph()?;
    spec.validate_agents(known)
}

fn known_ids(registry: &WorkflowRegistry) -> String {
    let ids = registry.names();
    if ids.is_empty() {
        "(none)".to_string()
    } else {
        ids.join(", ")
    }
}
