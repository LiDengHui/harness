//! Scenario-adaptive recall: the query a situation asks for, and the budget it
//! respects.

use harness_core::{Memory, Message, Role, SessionId};
use harness_memory::{recall_for, RecallBudget, RecallScenario, SqliteMemory};

async fn fill(memory: &SqliteMemory, session: SessionId, count: usize, text: &str) {
    for _ in 0..count {
        memory
            .append(session, &Message::assistant(text))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_subagent_query_carries_the_objective_and_the_node_id() {
    let query = RecallScenario::SubAgent {
        objective: "refactor the parser".into(),
        node_id: "node-alpha".into(),
    }
    .query(RecallBudget::default());

    assert!(query.text.contains("refactor the parser"), "{}", query.text);
    assert!(query.text.contains("node-alpha"), "{}", query.text);
    // A node's own earlier attempts can live in another session, so the search
    // is not pinned to one.
    assert_eq!(query.session_id, None);
}

#[tokio::test]
async fn a_subagent_recall_finds_the_nodes_own_prior_attempt() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("work")).await.unwrap();
    memory
        .append(
            session,
            &Message::assistant("a prior attempt on node-alpha left a failing test"),
        )
        .await
        .unwrap();
    fill(&memory, session, 12, "unrelated filler about the weather").await;

    let hits = recall_for(
        &memory,
        RecallScenario::SubAgent {
            objective: "make the failing test pass".into(),
            node_id: "node-alpha".into(),
        },
        RecallBudget::default(),
    )
    .await
    .unwrap();

    assert!(
        hits.iter().any(|hit| hit.snippet.contains("node-alpha")),
        "the node's own attempt must be retrievable: {:?}",
        hits.iter().map(|hit| &hit.snippet).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_verifier_recall_returns_the_implementation_claims_not_the_question() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("implementation")).await.unwrap();
    memory
        .append(
            session,
            &Message::user("implement the retry logic with exponential backoff"),
        )
        .await
        .unwrap();
    memory
        .append(
            session,
            &Message::assistant("I added retry logic with exponential backoff and a cap"),
        )
        .await
        .unwrap();
    fill(&memory, session, 8, "chatty filler about other subjects").await;

    let hits = recall_for(
        &memory,
        RecallScenario::Verifier {
            claim: "I added retry logic with exponential backoff and a cap".into(),
        },
        RecallBudget::default(),
    )
    .await
    .unwrap();

    assert!(!hits.is_empty(), "the claim must be retrievable");
    assert!(
        hits.iter().all(|hit| hit.role == Some(Role::Assistant)),
        "a verifier must not be fed the question as evidence: {:?}",
        hits.iter().map(|hit| hit.role).collect::<Vec<_>>()
    );
    assert!(
        hits.iter()
            .any(|hit| hit.snippet.contains("exponential backoff")),
        "the implementation's claim must be the top material: {:?}",
        hits.iter().map(|hit| &hit.snippet).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_follow_up_weights_the_current_session_over_others() {
    let memory = SqliteMemory::in_memory().await.unwrap();

    let current = memory.create_session(Some("current")).await.unwrap();
    memory
        .append(
            current,
            &Message::user("the deployment target for this project is staging"),
        )
        .await
        .unwrap();

    let other = memory.create_session(Some("other")).await.unwrap();
    memory
        .append(
            other,
            &Message::user("an unrelated project's deployment target is production"),
        )
        .await
        .unwrap();
    fill(
        &memory,
        other,
        6,
        "deployment target deployment target deployment target",
    )
    .await;

    let hits = recall_for(
        &memory,
        RecallScenario::FollowUp {
            session_id: current,
            question: "what is the deployment target".into(),
        },
        RecallBudget::default(),
    )
    .await
    .unwrap();

    assert!(!hits.is_empty());
    assert_eq!(
        hits[0].session_id,
        current,
        "the session being continued must come first: {:?}",
        hits.iter()
            .map(|hit| (hit.session_id, &hit.snippet))
            .collect::<Vec<_>>()
    );
    assert!(
        hits.iter().any(|hit| hit.session_id == current),
        "the current session must be represented"
    );
}

#[tokio::test]
async fn every_scenario_respects_the_token_budget() {
    let memory = SqliteMemory::in_memory().await.unwrap();
    let session = memory.create_session(Some("budget")).await.unwrap();
    let body = "the shared vocabulary appears repeatedly here. ".repeat(20);
    for index in 0..20 {
        memory
            .append(
                session,
                &Message::assistant(format!("point {index}: {body}")),
            )
            .await
            .unwrap();
    }

    let budget = RecallBudget {
        limit: 20,
        max_tokens: 60,
    };
    for scenario in [
        RecallScenario::SubAgent {
            objective: "shared vocabulary".into(),
            node_id: "node-budget".into(),
        },
        RecallScenario::Verifier {
            claim: "shared vocabulary appears repeatedly".into(),
        },
        RecallScenario::FollowUp {
            session_id: session,
            question: "shared vocabulary appears repeatedly".into(),
        },
    ] {
        let hits = recall_for(&memory, scenario.clone(), budget).await.unwrap();
        let used: usize = hits.iter().map(|hit| hit.tokens).sum();
        assert!(
            used <= budget.max_tokens,
            "{scenario:?} spent {used} of {} tokens",
            budget.max_tokens
        );
        assert!(!hits.is_empty(), "{scenario:?} returned nothing");
    }
}
