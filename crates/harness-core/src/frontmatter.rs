//! YAML frontmatter extraction for `.agent.md` and `SKILL.md` files.
//!
//! Both file types are Markdown documents whose leading `---` fenced block
//! carries declarative configuration and whose body is prompt content.

use serde::de::DeserializeOwned;

use crate::error::{HarnessError, Result};

/// Splits a document into its raw YAML frontmatter and Markdown body.
///
/// Returns `(None, whole_input)` when the document has no frontmatter. A
/// frontmatter block that opens but never closes is an error rather than being
/// silently treated as body text.
pub fn split(input: &str) -> Result<(Option<&str>, &str)> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);

    let mut lines = input.split_inclusive('\n').peekable();
    match lines.peek() {
        Some(first) if first.trim_end() == "---" => {
            lines.next();
        }
        _ => return Ok((None, input)),
    }

    let yaml_start = input.len() - lines.clone().map(|l| l.len()).sum::<usize>();
    let mut offset = yaml_start;

    for line in lines {
        if line.trim_end() == "---" {
            let yaml = &input[yaml_start..offset];
            let body = &input[offset + line.len()..];
            return Ok((Some(yaml), body));
        }
        offset += line.len();
    }

    Err(HarnessError::Other(
        "frontmatter block opens with `---` but is never closed".into(),
    ))
}

/// Parses a document into typed frontmatter plus its Markdown body.
pub fn parse<T: DeserializeOwned>(input: &str) -> Result<(T, String)> {
    let (yaml, body) = split(input)?;
    let parsed = match yaml {
        Some(yaml) => serde_norway::from_str(yaml)?,
        None => serde_norway::from_str("{}")?,
    };
    Ok((parsed, body.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Spec {
        name: String,
        #[serde(default)]
        tags: Vec<String>,
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let doc = "---\nname: reviewer\ntags:\n  - rust\n  - review\n---\n\nYou are a reviewer.\n";
        let (spec, body) = parse::<Spec>(doc).unwrap();
        assert_eq!(spec.name, "reviewer");
        assert_eq!(spec.tags, vec!["rust", "review"]);
        assert_eq!(body, "You are a reviewer.");
    }

    #[test]
    fn body_without_frontmatter_is_kept_whole() {
        let (yaml, body) = split("# Title\n\ntext\n").unwrap();
        assert!(yaml.is_none());
        assert_eq!(body, "# Title\n\ntext\n");
    }

    #[test]
    fn crlf_documents_are_supported() {
        let doc = "---\r\nname: reviewer\r\n---\r\n\r\nBody line\r\n";
        let (spec, body) = parse::<Spec>(doc).unwrap();
        assert_eq!(spec.name, "reviewer");
        assert_eq!(body, "Body line");
    }

    #[test]
    fn horizontal_rule_inside_body_is_not_frontmatter() {
        let (yaml, _) = split("Some text\n\n---\n\nmore text\n").unwrap();
        assert!(yaml.is_none());
    }

    #[test]
    fn unterminated_frontmatter_is_an_error() {
        let err = split("---\nname: reviewer\n").unwrap_err();
        assert!(err.to_string().contains("never closed"), "{err}");
    }

    #[test]
    fn missing_frontmatter_uses_type_defaults() {
        #[derive(Debug, Deserialize, Default, PartialEq)]
        #[serde(default)]
        struct Empty {
            name: String,
        }
        let (parsed, body) = parse::<Empty>("Just prose.").unwrap();
        assert_eq!(parsed, Empty::default());
        assert_eq!(body, "Just prose.");
    }
}
