//! Rank fusion for hybrid retrieval.
//!
//! Recall runs a vector search and a keyword search, then merges the two ranked
//! lists with Reciprocal Rank Fusion. RRF is used instead of adding the raw
//! scores because cosine similarity and BM25 are not comparable scales — fusing
//! only the *ranks* keeps one side from dominating for reasons that have nothing
//! to do with relevance.

use std::collections::BTreeMap;

use harness_core::{NodeId, Result};

use crate::vector::{HashingEmbedder, VectorIndex};

/// The `k` of `score = Σ 1/(k + rank)`. The conventional value: large enough
/// that being ranked first by one retriever is worth about as much as being
/// ranked in the top handful by both.
pub const RRF_K: f32 = 60.0;

/// Fuses ranked lists into one descending list of `(node_id, score)`.
///
/// Ties are broken by node id so the output order is deterministic.
pub fn reciprocal_rank_fusion(rankings: &[Vec<NodeId>], k: f32) -> Vec<(NodeId, f32)> {
    let mut scores: BTreeMap<NodeId, f32> = BTreeMap::new();
    for ranking in rankings {
        for (position, node_id) in ranking.iter().enumerate() {
            let rank = (position + 1) as f32;
            *scores.entry(*node_id).or_insert(0.0) += 1.0 / (k + rank);
        }
    }

    let mut fused: Vec<(NodeId, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fused
}

/// Couples an embedder with a vector index and fuses its ranking with a keyword
/// ranking supplied by the caller.
///
/// The keyword side is passed in rather than owned because it lives in SQLite's
/// FTS5 tables; this type deliberately knows nothing about storage.
pub struct HybridIndex {
    vectors: Box<dyn VectorIndex>,
    embedder: HashingEmbedder,
}

impl HybridIndex {
    pub fn new(vectors: Box<dyn VectorIndex>, embedder: HashingEmbedder) -> Self {
        Self { vectors, embedder }
    }

    pub fn embedder(&self) -> &HashingEmbedder {
        &self.embedder
    }

    pub fn vector_index(&self) -> &dyn VectorIndex {
        self.vectors.as_ref()
    }

    /// Embeds `text` and stores the vector under `node_id`.
    pub fn index_text(&mut self, node_id: NodeId, text: &str) -> Result<()> {
        self.vectors.upsert(node_id, &self.embedder.embed(text))
    }

    pub fn vector_ranking(&self, query: &str, limit: usize) -> Result<Vec<NodeId>> {
        if limit == 0 || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let query_vector = self.embedder.embed(query);
        Ok(self
            .vectors
            .search(&query_vector, limit)?
            .into_iter()
            .map(|(node_id, _)| node_id)
            .collect())
    }

    /// Fused, descending candidates from both retrievers.
    pub fn search(
        &self,
        query: &str,
        keyword_ranking: &[NodeId],
        candidate_limit: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let vector_ranking = self.vector_ranking(query, candidate_limit)?;
        Ok(reciprocal_rank_fusion(
            &[vector_ranking, keyword_ranking.to_vec()],
            RRF_K,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::BruteForceIndex;

    #[test]
    fn rrf_ranks_agreement_above_single_retriever_hits() {
        let agreed = NodeId::new();
        let first_only = NodeId::new();
        let second_only = NodeId::new();
        let tail = NodeId::new();

        let fused = reciprocal_rank_fusion(
            &[vec![first_only, agreed], vec![second_only, agreed, tail]],
            RRF_K,
        );

        assert_eq!(fused[0].0, agreed, "both retrievers agree on it");
        let expected = 1.0 / (RRF_K + 2.0) + 1.0 / (RRF_K + 2.0);
        assert!((fused[0].1 - expected).abs() < 1e-6);
        assert_eq!(fused.len(), 4);
        assert!(fused[3].1 < fused[2].1);
    }

    #[test]
    fn rrf_scores_decay_with_rank_and_k_is_a_knob() {
        let a = NodeId::new();
        let b = NodeId::new();
        let ranking = vec![a, b];

        let tight = reciprocal_rank_fusion(std::slice::from_ref(&ranking), 0.0);
        assert!((tight[0].1 - 1.0).abs() < 1e-6);
        assert!((tight[1].1 - 0.5).abs() < 1e-6);

        let loose = reciprocal_rank_fusion(&[ranking], 1_000.0);
        assert!(loose[0].1 > loose[1].1);
        assert!(loose[0].1 < 0.01);
    }

    #[test]
    fn rrf_ignores_empty_rankings() {
        let id = NodeId::new();
        let fused = reciprocal_rank_fusion(&[Vec::new(), vec![id], Vec::new()], RRF_K);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].0, id);
    }

    #[test]
    fn hybrid_search_merges_both_sides() {
        let embedder = HashingEmbedder::new(128);
        let mut hybrid = HybridIndex::new(Box::new(BruteForceIndex::new()), embedder);

        let vector_winner = NodeId::new();
        let keyword_winner = NodeId::new();
        hybrid
            .index_text(vector_winner, "sqlite wal checkpoint vacuum")
            .unwrap();
        hybrid
            .index_text(keyword_winner, "vacuum the workshop floor before friday")
            .unwrap();

        let fused = hybrid
            .search("sqlite wal checkpoint vacuum", &[keyword_winner], 8)
            .unwrap();

        assert_eq!(fused.len(), 2);
        assert!(
            fused.iter().any(|(id, _)| *id == keyword_winner),
            "a node only the keyword side found must survive fusion"
        );
        assert_eq!(hybrid.vector_index().len(), 2);
    }
}
