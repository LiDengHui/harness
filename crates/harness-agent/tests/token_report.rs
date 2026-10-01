//! The quantified token evidence behind the project spec's context claims.
//!
//! This is a measurement, not a reproduction. It loads the repository's real
//! `skills/` and `agents/` and the real builtin tool registry, prices each of
//! them with [`HeuristicEstimator`], and prints the arithmetic so a reader can
//! check every number. The assertions cover only structural relationships
//! (metadata is cheaper than a body, a narrowed tool set is cheaper than the
//! full one); no percentage from the spec is hard-coded, because a number that
//! a test is told to produce stops being evidence.
//!
//! Three axes are measured:
//!
//! 1. skill progressive disclosure — the always-injected name+description index
//!    versus injecting every skill body;
//! 2. tool schema advertising — the full builtin set versus the subsets the
//!    repository's own agents whitelist, plus a simulated "defer the N most
//!    expensive" curve;
//! 3. system prompt slimming — each `agents/*.agent.md` body against a one-line
//!    rewrite of the same intent written in this file.
//!
//! Everything is local and synchronous: no network, no API key, no MCP server.

use std::path::{Path, PathBuf};

use harness_agent::{AgentRegistry, AgentSpec};
use harness_core::{HeuristicEstimator, TokenEstimator, ToolSpec};
use harness_skills::SkillRegistry;
use harness_tools::ToolRegistry;

/// A one-line statement of each agent's intent, written here so the "after"
/// column of section 3 is a real rewrite rather than a truncated original.
const MINIMAL_PROMPTS: &[(&str, &str)] = &[
    (
        "backend-architect",
        "You design backend services and write down the interface contracts other agents implement against.",
    ),
    (
        "code-reviewer",
        "You review the change you were handed for correctness, regressions and missing tests, and report each finding in the order a reader would hit it.",
    ),
    (
        "default",
        "You are a coding agent in one workspace: read before you write, make the smallest correct change, and answer with the evidence for it.",
    ),
    (
        "test-writer",
        "You write focused tests that fail for exactly one reason, and you run them.",
    ),
];

#[test]
fn token_report() {
    // Hermetic: the global layer is redirected at an empty directory, so a
    // developer's `~/.harness` cannot change the numbers or break the load.
    let home = tempfile::tempdir().expect("a temp dir for HARNESS_HOME");
    std::env::set_var("HARNESS_HOME", home.path());

    let workspace = workspace_root();
    let estimator = HeuristicEstimator::default();
    let skills = SkillRegistry::load(&workspace).expect("the repository's skills/ loads");
    let agents = AgentRegistry::load(&workspace).expect("the repository's agents/ loads");
    let all_tools = ToolRegistry::with_builtins();

    println!();
    println!("=========================== token report ===========================");
    println!("workspace  {}", workspace.display());
    println!(
        "estimator  HeuristicEstimator {{ latin_chars_per_token: {}, message_overhead: {} }}",
        estimator.latin_chars_per_token, estimator.message_overhead
    );
    println!(
        "found      {} skills, {} agents, {} builtin tools",
        skills.len(),
        agents.len(),
        all_tools.len()
    );

    let skill_index = skill_index_section(&skills, &estimator);
    let tool_costs = tool_section(&agents, &all_tools, &estimator);
    let prompt_costs = prompt_section(&agents, &estimator);
    summary_section(
        &agents,
        &all_tools,
        &estimator,
        skill_index,
        &tool_costs,
        &prompt_costs,
    );

    // ---- structural assertions -------------------------------------------
    // These hold because of how the mechanisms are built, not because a
    // document asked for a particular number.
    assert!(!skills.is_empty(), "the repository ships skills");
    assert!(!agents.is_empty(), "the repository ships agents");
    assert_eq!(
        skill_index.activated_all,
        skill_index.index_total + skill_index.bodies_total,
        "activating every skill costs the index plus every body"
    );
    assert!(
        skill_index.index_total < skill_index.bodies_total,
        "the index ({} tokens) must be cheaper than every body ({})",
        skill_index.index_total,
        skill_index.bodies_total
    );
    assert_eq!(
        tool_costs.full_total,
        tool_costs.per_tool.iter().sum::<usize>(),
        "the full advertising cost is the sum of the per-tool costs"
    );
    assert!(
        tool_costs.narrowest_total < tool_costs.full_total,
        "advertising one agent's whitelist must cost less than advertising every builtin"
    );
    assert!(
        tool_costs.narrowest_saved > 0,
        "narrowing the advertised set must save tokens"
    );
    assert!(
        prompt_costs.total_after < prompt_costs.total_before,
        "the one-line rewrites must be cheaper than the bodies they replace"
    );
}

// ---------------------------------------------------------------------------
// section 1: skill progressive disclosure
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct SkillIndex {
    index_total: usize,
    bodies_total: usize,
    activated_all: usize,
}

fn skill_index_section(skills: &SkillRegistry, estimator: &HeuristicEstimator) -> SkillIndex {
    println!();
    println!("--- 1. skill progressive disclosure --------------------------------");

    let mut rows = vec![vec![
        "skill".to_string(),
        "meta".to_string(),
        "body".to_string(),
        "meta+body".to_string(),
        "body/meta".to_string(),
        "meta_chars".to_string(),
        "body_chars".to_string(),
    ]];
    let mut metas = Vec::new();
    let mut bodies = Vec::new();

    for spec in skills.list() {
        let meta = spec.metadata_tokens(estimator);
        let body = spec.body_tokens(estimator);
        let meta_chars = spec.name.chars().count() + spec.description.chars().count();
        let body_chars = spec.instructions.chars().count();
        rows.push(vec![
            spec.name.clone(),
            meta.to_string(),
            body.to_string(),
            (meta + body).to_string(),
            format!("{:.1}x", body as f64 / meta.max(1) as f64),
            meta_chars.to_string(),
            body_chars.to_string(),
        ]);
        assert!(
            meta < body,
            "skill `{}`: metadata ({meta}) must cost less than its body ({body})",
            spec.name
        );
        metas.push(meta);
        bodies.push(body);
    }
    print_table(&rows);

    let index_total = sum(&metas);
    let bodies_total = sum(&bodies);
    let saved = bodies_total.saturating_sub(index_total);

    println!(
        "index   (name + description, injected every turn) = {} = {index_total} tokens",
        addends(&metas)
    );
    println!(
        "bodies  (every instruction, injected on activation) = {} = {bodies_total} tokens",
        addends(&bodies)
    );
    println!(
        "saving  = {bodies_total} - {index_total} = {saved} tokens = {:.1}% of the naive \
         \"inject every body\" baseline",
        percent(saved, bodies_total)
    );
    println!(
        "note    the index is paid every turn; a body is paid only for the skills actually \
         activated, so the cost of using k skills is {index_total} + the k activated bodies"
    );

    SkillIndex {
        index_total,
        bodies_total,
        activated_all: index_total + bodies_total,
    }
}

// ---------------------------------------------------------------------------
// section 2: tool schema advertising
// ---------------------------------------------------------------------------

struct ToolCosts {
    per_tool: Vec<usize>,
    full_total: usize,
    narrowest_total: usize,
    narrowest_saved: usize,
}

fn tool_section(
    agents: &AgentRegistry,
    all_tools: &ToolRegistry,
    estimator: &HeuristicEstimator,
) -> ToolCosts {
    println!();
    println!("--- 2. tool schema advertising -------------------------------------");

    let specs = all_tools.specs();
    let mut rows = vec![vec![
        "tool".to_string(),
        "advertised".to_string(),
        "name+desc".to_string(),
        "schema_chars".to_string(),
    ]];
    let mut per_tool = Vec::new();

    for spec in &specs {
        let cost = spec.advertised_tokens(estimator);
        let prose = estimator.estimate(&spec.name) + estimator.estimate(&spec.description);
        rows.push(vec![
            spec.name.clone(),
            cost.to_string(),
            prose.to_string(),
            spec.parameters.to_string().chars().count().to_string(),
        ]);
        per_tool.push(cost);
    }
    print_table(&rows);

    let full_total = sum(&per_tool);
    println!(
        "all {} builtins advertised = {} = {full_total} tokens per turn",
        specs.len(),
        addends(&per_tool)
    );

    // (a) the deferral the repository already performs: an agent's `tools:`
    // whitelist is applied by `AgentRuntime`, so its advertised set is real.
    println!();
    println!(
        "a. deferral the repository already does — an agent advertises only its `tools:` whitelist:"
    );
    let mut rows = vec![vec![
        "agent".to_string(),
        "kept".to_string(),
        "advertised".to_string(),
        "deferred".to_string(),
        "saved".to_string(),
        "saved%".to_string(),
    ]];
    let mut narrowest: Option<(String, usize, usize, Vec<String>)> = None;

    for spec in agents.list() {
        // An empty whitelist means "every registered tool", so there is nothing
        // to narrow and it cannot be the narrowest policy.
        if spec.tools.is_empty() {
            continue;
        }
        let kept = all_tools.filter(&spec.tools);
        let kept_total = advertised_total(&kept.specs(), estimator);
        let saved = full_total.saturating_sub(kept_total);
        assert!(
            kept_total <= full_total,
            "agent `{}`: a whitelist cannot advertise more than every builtin",
            spec.id
        );
        rows.push(vec![
            spec.id.clone(),
            kept.len().to_string(),
            kept_total.to_string(),
            (specs.len() - kept.len()).to_string(),
            saved.to_string(),
            format!("{:.1}%", percent(saved, full_total)),
        ]);
        if narrowest
            .as_ref()
            .map_or(true, |(_, total, _, _)| kept_total < *total)
        {
            narrowest = Some((spec.id.clone(), kept_total, kept.len(), kept.names()));
        }
    }
    print_table(&rows);

    let (narrowest_agent, narrowest_total, narrowest_kept, narrowest_names) =
        narrowest.expect("at least one agent whitelists its tools");
    let narrowest_saved = full_total.saturating_sub(narrowest_total);
    println!(
        "best real policy: agent `{narrowest_agent}` advertises {narrowest_kept} of {} tools = \
         {narrowest_total} tokens, saving {narrowest_saved} = {:.1}%",
        specs.len(),
        percent(narrowest_saved, full_total)
    );

    // Cross-check the subtraction the next table relies on: "specs minus a
    // subset" is the same thing as "the registry of the names left over".
    assert_eq!(
        advertised_total(&all_tools.filter(&narrowest_names).specs(), estimator),
        narrowest_total,
        "filtering by the kept names must equal the whitelisted registry"
    );

    // (b) the generic mechanism: defer the N most expensive schemas.
    println!();
    println!("b. simulated deferral — drop the N most expensive schemas from the advertised set.");
    println!(
        "   assumption: a deferred tool is not advertised at all, so it costs nothing per turn;"
    );
    println!(
        "   the model must name it explicitly, and activating it costs its schema once, not every turn."
    );
    let mut ranked: Vec<&ToolSpec> = specs.iter().collect();
    ranked.sort_by(|a, b| {
        b.advertised_tokens(estimator)
            .cmp(&a.advertised_tokens(estimator))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut rows = vec![vec![
        "deferred".to_string(),
        "which".to_string(),
        "advertised".to_string(),
        "saved".to_string(),
        "saved%".to_string(),
    ]];
    for n in 1..specs.len() {
        let dropped: Vec<&str> = ranked[..n].iter().map(|spec| spec.name.as_str()).collect();
        let kept: Vec<ToolSpec> = specs
            .iter()
            .filter(|spec| !dropped.contains(&spec.name.as_str()))
            .cloned()
            .collect();
        let kept_total = advertised_total(&kept, estimator);
        let saved = full_total - kept_total;
        assert_eq!(
            kept_total + sum(&per_tool_of(&ranked[..n], estimator)),
            full_total,
            "kept + deferred must account for every advertised token"
        );
        rows.push(vec![
            n.to_string(),
            dropped.join(", "),
            kept_total.to_string(),
            saved.to_string(),
            format!("{:.1}%", percent(saved, full_total)),
        ]);
    }
    print_table(&rows);

    ToolCosts {
        per_tool,
        full_total,
        narrowest_total,
        narrowest_saved,
    }
}

fn per_tool_of(specs: &[&ToolSpec], estimator: &HeuristicEstimator) -> Vec<usize> {
    specs
        .iter()
        .map(|spec| spec.advertised_tokens(estimator))
        .collect()
}

fn advertised_total(specs: &[ToolSpec], estimator: &HeuristicEstimator) -> usize {
    specs
        .iter()
        .map(|spec| spec.advertised_tokens(estimator))
        .sum()
}

// ---------------------------------------------------------------------------
// section 3: system prompt slimming
// ---------------------------------------------------------------------------

struct PromptCosts {
    total_before: usize,
    total_after: usize,
}

fn prompt_section(agents: &AgentRegistry, estimator: &HeuristicEstimator) -> PromptCosts {
    println!();
    println!("--- 3. system prompt slimming --------------------------------------");
    println!(
        "before = the agent body as checked in; after = the one-line rewrite of the same intent"
    );
    println!("written in this file (`MINIMAL_PROMPTS`), so this is a real comparison.");

    let mut rows = vec![vec![
        "agent".to_string(),
        "before".to_string(),
        "after".to_string(),
        "saved".to_string(),
        "saved%".to_string(),
        "before_chars".to_string(),
        "after_chars".to_string(),
    ]];
    let mut befores = Vec::new();
    let mut afters = Vec::new();

    for spec in agents.list() {
        let before = estimator.estimate(&spec.system_prompt);
        let (minimal, _source) = minimal_prompt(spec);
        let after = estimator.estimate(minimal);
        assert!(
            after < before,
            "agent `{}`: the one-line rewrite ({after}) must be cheaper than the body ({before})",
            spec.id
        );
        rows.push(vec![
            spec.id.clone(),
            before.to_string(),
            after.to_string(),
            before.saturating_sub(after).to_string(),
            format!("{:.1}%", percent(before.saturating_sub(after), before)),
            spec.system_prompt.chars().count().to_string(),
            minimal.chars().count().to_string(),
        ]);
        befores.push(before);
        afters.push(after);
    }
    print_table(&rows);

    let total_before = sum(&befores);
    let total_after = sum(&afters);
    println!(
        "all {} agent prompts = {} = {total_before} tokens before, {} = {total_after} tokens after",
        agents.len(),
        addends(&befores),
        addends(&afters)
    );
    println!(
        "saving = {total_before} - {total_after} = {} tokens = {:.1}%",
        total_before.saturating_sub(total_after),
        percent(total_before.saturating_sub(total_after), total_before)
    );

    PromptCosts {
        total_before,
        total_after,
    }
}

/// The rewrite for an agent, plus where it came from. A future agent file with
/// no entry here falls back to its own one-line `description` rather than
/// silently dropping out of the comparison.
fn minimal_prompt(spec: &AgentSpec) -> (&str, &'static str) {
    for (id, prompt) in MINIMAL_PROMPTS {
        if *id == spec.id {
            return (prompt, "MINIMAL_PROMPTS in this test");
        }
    }
    (
        &spec.description,
        "frontmatter `description` (no rewrite written)",
    )
}

// ---------------------------------------------------------------------------
// section 4: summary
// ---------------------------------------------------------------------------

fn summary_section(
    agents: &AgentRegistry,
    all_tools: &ToolRegistry,
    estimator: &HeuristicEstimator,
    skills: SkillIndex,
    tools: &ToolCosts,
    prompts: &PromptCosts,
) {
    println!();
    println!("--- 4. summary -----------------------------------------------------");

    let mut rows = vec![vec![
        "axis".to_string(),
        "baseline".to_string(),
        "optimised".to_string(),
        "saved".to_string(),
        "saved%".to_string(),
    ]];
    let mut push = |axis: &str, baseline: usize, optimised: usize| {
        rows.push(vec![
            axis.to_string(),
            baseline.to_string(),
            optimised.to_string(),
            baseline.saturating_sub(optimised).to_string(),
            format!(
                "{:.1}%",
                percent(baseline.saturating_sub(optimised), baseline)
            ),
        ]);
    };

    push(
        "skills: all bodies -> name+description index",
        skills.bodies_total,
        skills.index_total,
    );
    push(
        "tools: all builtins -> narrowest agent whitelist",
        tools.full_total,
        tools.narrowest_total,
    );
    push(
        "prompts: checked-in bodies -> one-line rewrites",
        prompts.total_before,
        prompts.total_after,
    );

    // The three axes compose into the fixed prefix a session pays before any
    // conversation happens. This is what the spec's "session tokens" numbers
    // are about; the conversation itself is variable and is not measured here.
    let default_agent = agents
        .default_spec("default")
        .expect("the repository ships a default agent");
    let (minimal_default, _) = minimal_prompt(default_agent);
    let default_kept = all_tools.filter(&default_agent.tools);
    let prefix_before =
        estimator.estimate(&default_agent.system_prompt) + skills.bodies_total + tools.full_total;
    let prefix_after = estimator.estimate(minimal_default)
        + skills.index_total
        + advertised_total(&default_kept.specs(), estimator);
    push(
        "fixed prefix (default agent, all three)",
        prefix_before,
        prefix_after,
    );
    print_table(&rows);

    println!();
    println!("fixed prefix for the `default` agent: system prompt + skill cost + tool schemas");
    println!(
        "  before = {} + {} + {} = {prefix_before} tokens",
        estimator.estimate(&default_agent.system_prompt),
        skills.bodies_total,
        tools.full_total
    );
    println!(
        "  after  = {} + {} + {} = {prefix_after} tokens ({} tools whitelisted)",
        estimator.estimate(minimal_default),
        skills.index_total,
        advertised_total(&default_kept.specs(), estimator),
        default_kept.len()
    );

    println!();
    println!("--- caveats --------------------------------------------------------");
    println!(
        "* costs come from HeuristicEstimator (about 4 Latin chars/token, 1 per wide char), not a"
    );
    println!(
        "  real tokenizer; the ratios are the meaningful part, the absolute counts are estimates."
    );
    println!(
        "* this measures the *fixed prefix* a turn starts from (system prompt + skill index + tool"
    );
    println!(
        "  schemas). A session's total tokens also include the conversation, which is not measured."
    );
    println!(
        "* no MCP server is configured in this workspace, so there are no MCP tool schemas to move"
    );
    println!("  out of the system prompt; section 2 measures the same deferral mechanism on the");
    println!("  builtins, which is all that is locally observable.");
    println!(
        "* section 3's `after` numbers are the rewrites written in this file, not a repository"
    );
    println!(
        "  artefact, so they bound what slimming can buy rather than reproduce an experiment."
    );
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/<crate>/ sits two levels below the workspace root")
        .to_path_buf()
}

fn sum(values: &[usize]) -> usize {
    values.iter().sum()
}

fn addends(values: &[usize]) -> String {
    values
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(" + ")
}

fn percent(saved: usize, baseline: usize) -> f64 {
    if baseline == 0 {
        0.0
    } else {
        saved as f64 / baseline as f64 * 100.0
    }
}

/// Left-aligns text columns, right-aligns columns whose data cells are all
/// numbers, counts or dashes.
fn print_table(rows: &[Vec<String>]) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    let mut numeric = vec![true; columns];

    for (index, row) in rows.iter().enumerate() {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column].max(cell.chars().count());
            if index > 0 && column > 0 && !is_numeric(cell) {
                numeric[column] = false;
            }
        }
    }

    for row in rows {
        let line = row
            .iter()
            .enumerate()
            .map(|(column, cell)| {
                let pad = widths[column].saturating_sub(cell.chars().count());
                if numeric[column] && column > 0 {
                    format!("{}{cell}", " ".repeat(pad))
                } else {
                    format!("{cell}{}", " ".repeat(pad))
                }
            })
            .collect::<Vec<_>>()
            .join("  ");
        println!("{}", line.trim_end());
    }
}

fn is_numeric(cell: &str) -> bool {
    cell.ends_with('%') || cell.parse::<usize>().is_ok() || cell == "-"
}
