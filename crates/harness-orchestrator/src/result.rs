//! The four-field contract every sub-agent answers with.
//!
//! A sub-agent's reply is the only thing the executor sees from it, so the shape
//! is fixed and parsed strictly: a missing field, an unknown field or an empty
//! value is a failure that names what was wrong. A silent default here would let
//! a node report success while saying nothing, which is exactly the failure the
//! verifier cannot catch.

use harness_core::{HarnessError, Result};
use serde::{Deserialize, Serialize};

/// What one sub-agent hands back. Exactly these four fields, no more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtaskResult {
    pub objective: String,
    pub state: String,
    pub evidence: String,
    pub boundary: String,
}

impl SubtaskResult {
    /// The instruction text that asks a sub-agent for this shape. Kept next to
    /// the parser so the two cannot drift.
    pub const INSTRUCTION: &'static str = "\
Reply with exactly one JSON object and nothing else. It must have these four \
string fields and no others:

  \"objective\" — what you were asked to achieve, restated in one line.
  \"state\"     — what is true now: done, partly done or blocked, and why.
  \"evidence\"  — the commands you ran or the files you read, and what they showed.
  \"boundary\"  — what you did not do, and anything the caller must check.

Every field must be a non-empty string. Any other field is rejected, and a \
missing or empty field is a failure, not a default.";

    /// Extracts the JSON object from a model reply that may wrap it in prose or
    /// a fenced block. Strict about the four fields: a missing one, an unknown
    /// one, or an empty value is an error naming what was wrong — never a
    /// silent default.
    pub fn parse(reply: &str) -> Result<Self> {
        let mut first_object_error: Option<HarnessError> = None;

        for candidate in json_candidates(reply) {
            let candidate = candidate.trim();
            if !candidate.starts_with('{') {
                continue;
            }
            match serde_json::from_str::<Self>(candidate) {
                Ok(result) => {
                    check_non_empty(&result)?;
                    return Ok(result);
                }
                Err(err) => {
                    if first_object_error.is_none() {
                        first_object_error = Some(HarnessError::Other(format!(
                            "the reply is not the required four-field result object: {err}"
                        )));
                    }
                }
            }
        }

        Err(first_object_error
            .unwrap_or_else(|| HarnessError::Other("the reply contains no JSON object".into())))
    }
}

fn check_non_empty(result: &SubtaskResult) -> Result<()> {
    let fields = [
        ("objective", &result.objective),
        ("state", &result.state),
        ("evidence", &result.evidence),
        ("boundary", &result.boundary),
    ];
    for (name, value) in fields {
        if value.trim().is_empty() {
            return Err(HarnessError::Other(format!(
                "field `{name}` must not be empty"
            )));
        }
    }
    Ok(())
}

/// Candidate JSON objects inside `reply`, most likely first.
pub(crate) fn json_candidates(reply: &str) -> Vec<&str> {
    let mut candidates: Vec<&str> = Vec::new();
    candidates.extend(fenced_blocks(reply));
    candidates.extend(balanced_objects(reply));
    candidates.push(reply);
    candidates
}

/// The contents of every ``` fenced block, with the info string stripped.
fn fenced_blocks(reply: &str) -> Vec<&str> {
    const FENCE: &str = "```";
    let mut blocks = Vec::new();
    let mut rest = reply;

    while let Some(start) = rest.find(FENCE) {
        let after_open = &rest[start + FENCE.len()..];
        let body_start = match after_open.find('\n') {
            Some(newline) => &after_open[newline + 1..],
            None => after_open,
        };
        match body_start.find(FENCE) {
            Some(end) => {
                blocks.push(&body_start[..end]);
                rest = &body_start[end + FENCE.len()..];
            }
            None => break,
        }
    }

    blocks
}

/// Every balanced `{...}` run, in the order they appear, ignoring braces inside
/// string literals.
///
/// Prose can contain balanced braces that are not JSON (`the set {a, b}`), so a
/// caller must be able to look past one run to the next rather than committing
/// to the first.
pub(crate) fn balanced_objects(text: &str) -> Vec<&str> {
    let mut objects = Vec::new();
    let mut search_from = 0usize;

    while let Some(offset) = text[search_from..].find('{') {
        let start = search_from + offset;
        match matching_brace(text, start) {
            Some(end) => {
                objects.push(&text[start..end]);
                search_from = end;
            }
            None => search_from = start + 1,
        }
    }

    objects
}

/// The index one past the `}` matching the `{` at `start`.
fn matching_brace(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, byte) in bytes.iter().enumerate().skip(start) {
        let byte = *byte;
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_json() -> String {
        r#"{"objective":"add a parser","state":"done","evidence":"cargo test passed","boundary":"no docs written"}"#
            .to_string()
    }

    #[test]
    fn a_bare_object_parses() {
        let result = SubtaskResult::parse(&valid_json()).unwrap();
        assert_eq!(result.objective, "add a parser");
        assert_eq!(result.state, "done");
        assert_eq!(result.evidence, "cargo test passed");
        assert_eq!(result.boundary, "no docs written");
    }

    #[test]
    fn an_object_in_a_json_fence_parses() {
        let reply = format!("```json\n{}\n```", valid_json());
        let result = SubtaskResult::parse(&reply).unwrap();
        assert_eq!(result.objective, "add a parser");
    }

    #[test]
    fn an_object_in_a_bare_fence_parses() {
        let reply = format!("```\n{}\n```", valid_json());
        assert!(SubtaskResult::parse(&reply).is_ok());
    }

    #[test]
    fn an_object_surrounded_by_prose_parses() {
        let reply = format!(
            "I finished the work. Here is the summary:\n\n{}\n\nLet me know if you need more.",
            valid_json()
        );
        let result = SubtaskResult::parse(&reply).unwrap();
        assert_eq!(result.state, "done");
    }

    #[test]
    fn prose_containing_braces_does_not_confuse_extraction() {
        let reply = format!("The set {{a, b}} was computed.\n{}\nDone.", valid_json());
        let result = SubtaskResult::parse(&reply).unwrap();
        assert_eq!(result.objective, "add a parser");
    }

    #[test]
    fn a_missing_field_is_rejected_and_named() {
        let reply = r#"{"objective":"x","state":"done","evidence":"tests pass"}"#;
        let err = SubtaskResult::parse(reply).unwrap_err();
        assert!(err.to_string().contains("boundary"), "{err}");
    }

    #[test]
    fn an_extra_field_is_rejected_and_named() {
        let reply =
            r#"{"objective":"x","state":"done","evidence":"e","boundary":"b","confidence":0.9}"#;
        let err = SubtaskResult::parse(reply).unwrap_err();
        assert!(err.to_string().contains("confidence"), "{err}");
    }

    #[test]
    fn an_empty_value_is_rejected_and_named() {
        let reply = r#"{"objective":"x","state":"done","evidence":"   ","boundary":"b"}"#;
        let err = SubtaskResult::parse(reply).unwrap_err();
        assert!(err.to_string().contains("evidence"), "{err}");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn no_json_at_all_is_an_error() {
        let err = SubtaskResult::parse("I did the work, trust me.").unwrap_err();
        assert!(err.to_string().contains("no JSON object"), "{err}");
    }

    #[test]
    fn malformed_json_is_an_error() {
        let err = SubtaskResult::parse(r#"{"objective": "x", "state": }"#).unwrap_err();
        assert!(err.to_string().contains("four-field"), "{err}");
    }

    #[test]
    fn the_instruction_names_all_four_fields() {
        for field in ["objective", "state", "evidence", "boundary"] {
            assert!(
                SubtaskResult::INSTRUCTION.contains(field),
                "instruction omits `{field}`"
            );
        }
        assert!(SubtaskResult::INSTRUCTION.contains("no others"));
    }

    #[test]
    fn a_result_round_trips_through_json() {
        let result = SubtaskResult::parse(&valid_json()).unwrap();
        let encoded = serde_json::to_string(&result).unwrap();
        let decoded: SubtaskResult = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, result);
    }
}
