//! Embeddings and vector search.
//!
//! Two things live here: the only embedder the harness can use offline, and the
//! index backends that score against it. Nothing in this module talks to a
//! provider — see [`HashingEmbedder`] for what that means for retrieval quality.

use std::collections::HashMap;

use harness_core::{NodeId, Result};

/// Similarity search over node embeddings.
///
/// Implementations must be usable from several tasks, so both `Send` and `Sync`
/// are required even though `upsert` takes `&mut self`.
pub trait VectorIndex: Send + Sync {
    fn upsert(&mut self, node_id: NodeId, vector: &[f32]) -> Result<()>;

    /// Cosine similarity against `query`, best first.
    fn search(&self, query: &[f32], limit: usize) -> Result<Vec<(NodeId, f32)>>;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET ^ seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Feature-hashing bag-of-words embedder.
///
/// **This is a lexical embedder, not a semantic one.** It hashes lowercased
/// word tokens (plus hashed adjacent pairs) into fixed buckets and L2-normalises
/// the result. Retrieval therefore behaves like a fuzzy keyword match: two texts
/// that share vocabulary score alike, while a paraphrase that shares no words
/// scores near zero. It exists because no embedding API is reachable offline and
/// because deterministic, dependency-free vectors are what the tests can assert
/// on. Do not describe its output as semantic.
///
/// FNV-1a is spelled out rather than borrowed from `std` because
/// `RandomState` seeds itself per process, which would make the vectors — and
/// therefore the stored index — differ between runs.
#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    dim: usize,
}

/// Weight of an adjacent-token feature relative to a single-token feature.
const BIGRAM_WEIGHT: f32 = 0.5;
const BIGRAM_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

impl HashingEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// L2-normalised feature hash of `text`. The empty (or wordless) input maps
    /// to the zero vector, which every similarity function treats as "no match".
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let tokens = tokenize(text);
        let mut buckets = vec![0.0f32; self.dim];
        let dim = self.dim as u64;

        for token in &tokens {
            let hash = fnv1a(0, token.as_bytes());
            let index = (hash % dim) as usize;
            buckets[index] += sign(hash);
        }

        for pair in tokens.windows(2) {
            let joined = format!("{} {}", pair[0], pair[1]);
            let hash = fnv1a(BIGRAM_SEED, joined.as_bytes());
            let index = (hash % dim) as usize;
            buckets[index] += sign(hash) * BIGRAM_WEIGHT;
        }

        let norm = buckets.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut buckets {
                *value /= norm;
            }
        }
        buckets
    }
}

/// Maps a hash to ±1 so that unrelated collisions tend to cancel instead of
/// piling positive mass onto the same bucket.
fn sign(hash: u64) -> f32 {
    if hash & (1 << 63) == 0 {
        1.0
    } else {
        -1.0
    }
}

/// Splits on non-word characters. Non-ASCII word characters (CJK, Cyrillic, …)
/// become one token each: a whole Chinese sentence has no spaces, so treating a
/// run as a single token would give every such text a feature count of one.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for character in text.to_lowercase().chars() {
        let is_word = character.is_alphanumeric() || character == '_';
        if is_word && character.is_ascii() {
            current.push(character);
            continue;
        }
        if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        if is_word {
            tokens.push(character.to_string());
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Exact in-memory cosine scan. Also the default backend.
///
/// A linear scan is the right default here: sessions hold thousands of nodes at
/// most, and the index is rebuilt from the `embeddings` table on demand so it
/// cannot drift out of sync with what is actually stored (a stale cache would
/// silently hide nodes that were written by a previous process).
#[derive(Debug, Default, Clone)]
pub struct BruteForceIndex {
    vectors: HashMap<NodeId, Vec<f32>>,
}

impl BruteForceIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds an index from `(node_id, vector)` rows read out of storage.
    pub fn from_rows<I>(rows: I) -> Self
    where
        I: IntoIterator<Item = (NodeId, Vec<f32>)>,
    {
        Self {
            vectors: rows.into_iter().collect(),
        }
    }
}

impl VectorIndex for BruteForceIndex {
    fn upsert(&mut self, node_id: NodeId, vector: &[f32]) -> Result<()> {
        self.vectors.insert(node_id, vector.to_vec());
        Ok(())
    }

    fn search(&self, query: &[f32], limit: usize) -> Result<Vec<(NodeId, f32)>> {
        if limit == 0 || query.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(NodeId, f32)> = self
            .vectors
            .iter()
            .filter_map(|(node_id, vector)| {
                let score = cosine_similarity(query, vector);
                (score > 0.0).then_some((*node_id, score))
            })
            .collect();

        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(limit);
        Ok(scored)
    }

    fn len(&self) -> usize {
        self.vectors.len()
    }
}

/// Cosine similarity, `0.0` for mismatched dimensions or a zero-norm side.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(vector: &[f32]) -> f32 {
        vector.iter().map(|v| v * v).sum::<f32>().sqrt()
    }

    #[test]
    fn hashing_embedder_is_deterministic_and_normalised() {
        let embedder = HashingEmbedder::new(64);
        let a = embedder.embed("the quick brown fox jumps over the lazy dog");
        let b = embedder.embed("the quick brown fox jumps over the lazy dog");

        assert_eq!(embedder.embed("same text"), embedder.embed("same text"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!((norm(&a) - 1.0).abs() < 1e-5, "norm was {}", norm(&a));
    }

    #[test]
    fn different_texts_embed_differently() {
        let embedder = HashingEmbedder::new(64);
        let a = embedder.embed("incremental compilation of the parser crate");
        let b = embedder.embed("the deployment pipeline rolled back at midnight");
        assert_ne!(a, b);
        assert!(cosine_similarity(&a, &b) < 0.9);
    }

    #[test]
    fn feature_hashing_is_case_insensitive_and_rewards_overlap() {
        let embedder = HashingEmbedder::new(128);
        let plain = embedder.embed("QUANTUM FLUX CAPACITOR");
        let lower = embedder.embed("quantum flux capacitor");
        assert_eq!(plain, lower);

        let overlapping = embedder.embed("quantum flux capacitor recalibration");
        let unrelated = embedder.embed("grocery list for the weekend");
        assert!(
            cosine_similarity(&plain, &overlapping) > cosine_similarity(&plain, &unrelated),
            "shared vocabulary must score higher than none"
        );
    }

    #[test]
    fn wordless_input_yields_the_zero_vector() {
        let embedder = HashingEmbedder::new(16);
        assert_eq!(embedder.embed(""), vec![0.0; 16]);
        assert_eq!(embedder.embed("!!!"), vec![0.0; 16]);
        assert_eq!(
            cosine_similarity(&embedder.embed(""), &embedder.embed("a")),
            0.0
        );
    }

    #[test]
    fn non_ascii_scripts_get_one_token_per_character() {
        let tokens = tokenize("设计一个后端架构");
        assert_eq!(tokens.len(), 8);

        let embedder = HashingEmbedder::new(32);
        let shared = embedder.embed("设计一个后端架构");
        let different = embedder.embed("今天的天气很不错啊");
        assert!(cosine_similarity(&shared, &different) < 0.9);
        assert!((norm(&shared) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn brute_force_search_ranks_by_cosine_and_respects_the_limit() {
        let embedder = HashingEmbedder::new(128);
        let mut index = BruteForceIndex::new();

        let near = NodeId::new();
        let far = NodeId::new();
        let unrelated = NodeId::new();
        index
            .upsert(near, &embedder.embed("sqlite wal checkpoint vacuum"))
            .unwrap();
        index
            .upsert(
                far,
                &embedder.embed("sqlite wal checkpoint vacuum of the database"),
            )
            .unwrap();
        index
            .upsert(unrelated, &embedder.embed("banana bread recipe"))
            .unwrap();

        assert_eq!(index.len(), 3);
        assert!(!index.is_empty());

        let hits = index
            .search(&embedder.embed("sqlite wal checkpoint vacuum"), 2)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, near);
        assert!(hits[0].1 > hits[1].1);
        assert!(hits.iter().all(|(id, _)| *id != unrelated));

        assert!(index
            .search(&embedder.embed("sqlite"), 0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn brute_force_index_ignores_dimension_mismatches() {
        let index = BruteForceIndex::from_rows(vec![(NodeId::new(), vec![1.0, 0.0, 0.0])]);
        assert!(index.search(&[1.0, 0.0], 5).unwrap().is_empty());
    }
}
