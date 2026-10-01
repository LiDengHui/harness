//! The pieces of hybrid recall that are not storage: query building, snippet
//! shaping and token budgeting.

use harness_core::{HarnessError, HeuristicEstimator, NodeId, Result, TokenEstimator};
use rusqlite::Connection;

/// Snippets are clipped to roughly this many characters. Recall injects hits
/// into a prompt, so a hit costs prompt space proportional to its snippet, not
/// to the node it came from.
pub const SNIPPET_CHARS: usize = 400;

/// How many candidates each retriever contributes per requested hit.
pub const CANDIDATE_FANOUT: usize = 4;

/// Turns free text into an FTS5 query, or `None` when there is nothing to search
/// for.
///
/// Every term is quoted: raw user text contains `-`, `*`, `"` and `NEAR`, all of
/// which are FTS5 syntax and would otherwise turn a query into a syntax error.
/// Terms are OR-ed because BM25 already ranks documents matching more of them
/// higher, while AND would return nothing as soon as one word is absent.
pub fn fts_query(text: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if raw.is_empty() || !raw.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        let term = format!("\"{}\"", raw.replace('"', ""));
        if !terms.contains(&term) {
            terms.push(term);
        }
        if terms.len() >= 32 {
            break;
        }
    }
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// Keyword ranking from the FTS5 index, best first.
///
/// The FTS side is what keeps nodes without an embedding reachable.
pub fn keyword_ranking(conn: &Connection, query: &str, limit: usize) -> Result<Vec<NodeId>> {
    let Some(fts) = fts_query(query) else {
        return Ok(Vec::new());
    };
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut statement = conn
        .prepare("SELECT rowid FROM nodes_fts WHERE nodes_fts MATCH ?1 ORDER BY rank LIMIT ?2")
        .map_err(|err| HarnessError::Memory(err.to_string()))?;

    let rowids = statement
        .query_map(rusqlite::params![fts, limit as i64], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|err| HarnessError::Memory(err.to_string()))?;

    let mut hits = Vec::new();
    for rowid in rowids {
        let rowid = rowid.map_err(|err| HarnessError::Memory(err.to_string()))?;
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM nodes WHERE rowid = ?1",
                rusqlite::params![rowid],
                |row| row.get(0),
            )
            .map_err(|err| HarnessError::Memory(err.to_string()))?;
        if let Some(id) = id {
            hits.push(id.parse::<NodeId>()?);
        }
    }
    Ok(hits)
}

/// Clips `content` to [`SNIPPET_CHARS`], reporting whether anything was cut.
pub fn snippet(content: &str) -> (String, bool) {
    if content.chars().count() <= SNIPPET_CHARS {
        return (content.to_string(), false);
    }
    let mut clipped: String = content.chars().take(SNIPPET_CHARS).collect();
    clipped.push('…');
    (clipped, true)
}

/// Shrinks `text` until it fits `budget` tokens, or gives up on it.
///
/// Returns `None` when even a single character would overflow, so a caller never
/// has to decide whether an empty hit is worth returning.
pub fn fit_snippet(text: &str, budget: usize, estimator: &HeuristicEstimator) -> Option<String> {
    if budget == 0 {
        return None;
    }
    if estimator.estimate(text) <= budget {
        return Some(text.to_string());
    }

    let mut candidate = text;
    loop {
        let characters = candidate.chars().count();
        if characters <= 1 {
            return None;
        }
        // Geometric shrink: converging in a handful of steps keeps this off the
        // hot path even for absurd budgets.
        candidate = &candidate[..candidate
            .char_indices()
            .nth(characters * 3 / 4)
            .map(|(index, _)| index)
            .unwrap_or(candidate.len())];
        if estimator.estimate(candidate) <= budget {
            return Some(candidate.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_queries_quote_terms_and_survive_syntax_characters() {
        assert_eq!(
            fts_query("quantum flux capacitor").as_deref(),
            Some("\"quantum\" OR \"flux\" OR \"capacitor\"")
        );
        let hostile = fts_query("NEAR(a b) -c *d \"e\"");
        let hostile = hostile.expect("alphanumeric terms should survive");
        assert!(hostile.contains("\"NEAR\""), "{hostile}");
        assert!(!hostile.contains("NEAR("));

        assert_eq!(fts_query("   ...  "), None);
        assert_eq!(fts_query(""), None);
    }

    #[test]
    fn snippets_are_clipped_on_character_boundaries() {
        let short = "a short node body";
        assert_eq!(snippet(short), (short.to_string(), false));

        let long = "设".repeat(SNIPPET_CHARS + 50);
        let (clipped, was_clipped) = snippet(&long);
        assert!(was_clipped);
        assert_eq!(clipped.chars().count(), SNIPPET_CHARS + 1);
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn budget_shrinking_always_lands_within_budget() {
        let estimator = HeuristicEstimator::default();
        let text = "the quick brown fox jumps over the lazy dog ".repeat(20);

        for budget in [1usize, 2, 7, 40, 900] {
            let fitted = fit_snippet(&text, budget, &estimator).expect("budget >= 1 fits");
            assert!(
                estimator.estimate(&fitted) <= budget,
                "budget {budget} produced {} tokens",
                estimator.estimate(&fitted)
            );
            assert!(!fitted.is_empty());
        }

        assert!(fit_snippet(&text, 0, &estimator).is_none());
        assert_eq!(fit_snippet("tiny", 4, &estimator).as_deref(), Some("tiny"));
    }
}
