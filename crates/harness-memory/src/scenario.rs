//! Scenario-adaptive recall: what to ask memory for depends on who is asking.
//!
//! A single query built from the raw task text is the wrong question in three
//! recurring situations, and the difference is not cosmetic — it decides which
//! nodes come back at all:
//!
//! * A **sub-agent starting a node** needs its own objective *and* its node id.
//!   The objective finds related work; the node id finds this node's own earlier
//!   attempts, whose sessions and snapshots are labelled with it. Searching the
//!   objective alone can surface a sibling node's material as if it were this
//!   one's.
//! * A **verifier** needs what the implementation *claimed*, not what was asked.
//!   The user turn states the requirement; the assistant turns state what was
//!   done. Restricting the query to assistant roles and searching the claim text
//!   keeps the verifier from re-reading the question as if it were evidence.
//! * A **main agent answering a follow-up** should weight the current session
//!   over every other one. The follow-up is usually about this conversation, and
//!   an unweighted cross-session search can spend the whole budget on unrelated
//!   sessions before it reaches the turn being asked about.
//!
//! [`recall_for`] is the entry point. The per-scenario query is also exposed as
//! [`RecallScenario::query`], so a caller that owns its own retrieval loop can
//! reuse the rule without the two-pass budget split.

use harness_core::{Memory, RecallHit, RecallQuery, Result, Role, SessionId};

/// How much recall a caller is willing to inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallBudget {
    /// Maximum number of hits.
    pub limit: usize,
    /// Hard ceiling on the tokens the hits may occupy.
    pub max_tokens: usize,
}

impl Default for RecallBudget {
    fn default() -> Self {
        Self {
            limit: 8,
            max_tokens: 2_000,
        }
    }
}

/// The situation a recall is serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecallScenario {
    /// A sub-agent is about to start `node_id` with `objective`.
    SubAgent { objective: String, node_id: String },
    /// A verifier is checking a result that `claim` describes.
    Verifier { claim: String },
    /// The main agent is answering `question` inside `session_id`.
    FollowUp {
        session_id: SessionId,
        question: String,
    },
}

impl RecallScenario {
    /// The query this scenario asks memory for.
    ///
    /// For [`RecallScenario::FollowUp`] this is the *local* query — restricted to
    /// the current session. [`recall_for`] adds the cross-session pass with
    /// whatever budget is left, which is what weights the current session over
    /// the others.
    pub fn query(&self, budget: RecallBudget) -> RecallQuery {
        match self {
            RecallScenario::SubAgent { objective, node_id } => RecallQuery {
                session_id: None,
                // The node id is appended rather than replacing the objective:
                // prior attempts at this node are labelled with the id, while the
                // objective reaches the surrounding work.
                text: format!("{objective} {node_id}"),
                limit: budget.limit,
                max_tokens: budget.max_tokens,
                roles: Vec::new(),
            },
            RecallScenario::Verifier { claim } => RecallQuery {
                session_id: None,
                text: claim.clone(),
                limit: budget.limit,
                max_tokens: budget.max_tokens,
                // A claim is an assistant turn; a user turn is the requirement,
                // which is exactly what the verifier must not treat as evidence.
                roles: vec![Role::Assistant],
            },
            RecallScenario::FollowUp {
                session_id,
                question,
            } => RecallQuery {
                session_id: Some(*session_id),
                text: question.clone(),
                limit: budget.limit,
                max_tokens: budget.max_tokens,
                roles: Vec::new(),
            },
        }
    }
}

/// Recalls for `scenario`, shaping the query to the situation.
///
/// Every scenario gets the budget it asks for. [`RecallScenario::FollowUp`] is
/// the one that needs two passes: the current session is searched first and
/// claims the budget, then a cross-session search spends only what is left. That
/// is how the current session is weighted over the others without a scoring knob
/// in the store — a hit from another session can never displace one from the
/// conversation being continued.
pub async fn recall_for(
    memory: &dyn Memory,
    scenario: RecallScenario,
    budget: RecallBudget,
) -> Result<Vec<RecallHit>> {
    let mut hits = memory.recall(scenario.query(budget)).await?;

    let RecallScenario::FollowUp { .. } = scenario else {
        return Ok(hits);
    };

    let used: usize = hits.iter().map(|hit| hit.tokens).sum();
    if used >= budget.max_tokens {
        return Ok(hits);
    }

    // The second pass is unrestricted, so it may return the same local nodes
    // again; they are dropped by id rather than re-injected.
    let mut elsewhere = scenario.query(RecallBudget {
        max_tokens: budget.max_tokens - used,
        ..budget
    });
    elsewhere.session_id = None;
    for hit in memory.recall(elsewhere).await? {
        if !hits.iter().any(|seen| seen.node_id == hit.node_id) {
            hits.push(hit);
        }
    }
    Ok(hits)
}
