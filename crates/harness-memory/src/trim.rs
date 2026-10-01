//! Content rewriting used by the three trim phases.
//!
//! The free functions here are the *mechanical* half of trim: they take text and
//! return shorter text. Everything that touches the database lives in
//! [`crate::store`], which is what makes these directly testable.
//!
//! The design rule behind all of it: a rewrite must never destroy information,
//! only move it somewhere retrievable. Tool bodies go to the `blobs` table
//! before their content is replaced (that is why [`crate::store`] writes a blob
//! row for every large append), and inline base64 is replaced by a placeholder
//! that records what was removed. Without that, "trim" would just be deletion
//! with a friendlier name.

use harness_core::NodeId;

/// Shortest base64 run worth replacing. Below this a `data:` URI is plausibly a
/// real (tiny) value the model should still see.
pub const MIN_BASE64_RUN: usize = 128;

/// Shortest run of `\uXXXX`-style escapes treated as escaped binary.
pub const MIN_ESCAPE_RUN: usize = 64;

/// Renders a byte count the way the elision placeholders spell it.
pub fn size_label(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

/// The reference that replaces an elided tool body. It names the byte count and
/// the node so an operator (or the model) can ask for it back through
/// `raw_output`.
pub fn elide_tool_output(node_id: NodeId, bytes: usize) -> String {
    format!(
        "[tool output elided: {} — retrieve node {node_id} via raw_output]",
        size_label(bytes)
    )
}

/// Whether `content` already *is* an elision reference.
///
/// Used to keep trim idempotent: re-running it must not overwrite a stored blob
/// with the placeholder that replaced it.
pub fn is_elision_reference(content: &str) -> bool {
    content.starts_with("[tool output elided:")
        || content.starts_with("[base64 data URI elided:")
        || content.starts_with("[escaped binary elided:")
}

/// Strips embedded binary from `content`, returning `None` when nothing matched.
///
/// Only the payload span is replaced; every byte around it survives verbatim,
/// which is what lets phase 2 coexist with the losslessness invariant.
pub fn strip_inline_binary(content: &str) -> Option<String> {
    let without_data_uris = strip_data_uris(content);
    let stripped = strip_escape_runs(&without_data_uris);
    (stripped != content).then_some(stripped)
}

fn is_base64_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'='
}

/// `(end of the media type, end of the payload)` for the data URI at `start`.
fn data_uri_span(content: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = content.as_bytes();
    // The media type is short; a `;base64,` further away than this is not part
    // of the same URI, and a whitespace byte means we are not inside a URI at
    // all (the phrase "data:" also occurs in prose).
    let search_end = (start + 128).min(bytes.len());
    let mut cursor = start + "data:".len();
    let mut separator = None;
    while cursor < search_end {
        if bytes[cursor..].starts_with(b";base64,") {
            separator = Some(cursor);
            break;
        }
        if bytes[cursor].is_ascii_whitespace() {
            return None;
        }
        cursor += 1;
    }
    let separator = separator?;

    let payload_start = separator + ";base64,".len();
    let mut end = payload_start;
    while end < bytes.len() && is_base64_byte(bytes[end]) {
        end += 1;
    }
    if end - payload_start < MIN_BASE64_RUN {
        return None;
    }
    Some((separator, end))
}

fn strip_data_uris(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut copied = 0usize;
    let mut cursor = 0usize;

    while cursor + 5 <= bytes.len() {
        if bytes[cursor..].starts_with(b"data:") {
            if let Some((media_type_end, payload_end)) = data_uri_span(content, cursor) {
                let payload_len = payload_end - (media_type_end + ";base64,".len());
                let media_type = content
                    .get(cursor + "data:".len()..media_type_end)
                    .filter(|mime| !mime.is_empty())
                    .unwrap_or("application/octet-stream");

                out.push_str(&content[copied..cursor]);
                out.push_str(&format!(
                    "[base64 data URI elided: {} ({media_type})]",
                    size_label(payload_len)
                ));
                copied = payload_end;
                cursor = payload_end;
                continue;
            }
        }
        cursor += 1;
    }

    out.push_str(&content[copied..]);
    out
}

/// Length in bytes of a `\uXXXX` / `\xXX` escape at the front of `bytes`.
fn escape_unit(bytes: &[u8]) -> Option<usize> {
    if bytes.len() >= 6
        && bytes[0] == b'\\'
        && bytes[1] == b'u'
        && bytes[2..6].iter().all(|byte| byte.is_ascii_hexdigit())
    {
        return Some(6);
    }
    if bytes.len() >= 4
        && bytes[0] == b'\\'
        && bytes[1] == b'x'
        && bytes[2..4].iter().all(|byte| byte.is_ascii_hexdigit())
    {
        return Some(4);
    }
    None
}

fn strip_escape_runs(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut copied = 0usize;
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        if escape_unit(&bytes[cursor..]).is_none() {
            cursor += 1;
            continue;
        }

        let mut end = cursor;
        let mut units = 0usize;
        while let Some(unit) = escape_unit(&bytes[end..]) {
            end += unit;
            units += 1;
        }

        if units >= MIN_ESCAPE_RUN {
            out.push_str(&content[copied..cursor]);
            out.push_str(&format!(
                "[escaped binary elided: {}]",
                size_label(end - cursor)
            ));
            copied = end;
        }
        cursor = end.max(cursor + 1);
    }

    out.push_str(&content[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_labels_read_the_way_the_placeholder_claims() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(12_600), "12.3 KB");
    }

    #[test]
    fn elision_reference_names_the_size_and_the_node() {
        let node = NodeId::new();
        let reference = elide_tool_output(node, 12_600);
        assert!(reference.contains("12.3 KB"), "{reference}");
        assert!(reference.contains(&node.to_string()), "{reference}");
        assert!(is_elision_reference(&reference));
    }

    #[test]
    fn data_uris_are_replaced_but_surrounding_prose_survives() {
        let payload = "iVBORw0KGgo".repeat(64);
        let content = format!(
            "Here is the screenshot you asked for:\n![shot](data:image/png;base64,{payload})\nThat is all."
        );

        let stripped = strip_inline_binary(&content).expect("the payload should be elided");
        assert!(!stripped.contains(&payload));
        assert!(stripped.contains("[base64 data URI elided:"));
        assert!(stripped.contains("(image/png)"));
        assert!(stripped.starts_with("Here is the screenshot you asked for:\n![shot]("));
        assert!(stripped.ends_with(")\nThat is all."));
        assert!(stripped.len() < content.len() / 4);
    }

    #[test]
    fn short_data_uris_and_prose_about_data_are_left_alone() {
        let tiny = "data:text/plain;base64,aGVsbG8=";
        assert!(strip_inline_binary(tiny).is_none());

        let prose = "the data: field is documented as;base64, but we do not use it here";
        assert!(strip_inline_binary(prose).is_none());

        assert!(strip_inline_binary("nothing to strip at all").is_none());
    }

    #[test]
    fn long_escape_runs_are_elided_and_short_ones_are_kept() {
        let long = format!("blob follows: {}", "\\u0000".repeat(MIN_ESCAPE_RUN + 10));
        let stripped = strip_inline_binary(&long).expect("a long escape run should be elided");
        assert!(stripped.contains("[escaped binary elided:"));
        assert!(stripped.starts_with("blob follows: "));

        let short = "a\\u0000b\\u0001c";
        assert!(strip_inline_binary(short).is_none());
    }

    #[test]
    fn stripping_is_idempotent() {
        let payload = "QUJD".repeat(80);
        let content = format!("before data:application/octet-stream;base64,{payload} after");
        let once = strip_inline_binary(&content).unwrap();
        assert!(strip_inline_binary(&once).is_none());
    }
}
