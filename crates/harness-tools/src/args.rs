//! Typed access to a tool call's raw JSON arguments.
//!
//! The builtins that still hand-parse their arguments go through this so that a
//! missing or mistyped field names itself in the error instead of quietly
//! falling back to a default. The ones built with `#[harness_macros::tool]`
//! derive that from their argument struct instead.

use harness_core::{HarnessError, Result};
use serde_json::Value;

pub(crate) struct Args {
    tool: &'static str,
    value: Value,
}

impl Args {
    pub(crate) fn new(tool: &'static str, value: Value) -> Result<Self> {
        if !value.is_object() {
            return Err(HarnessError::Tool(format!(
                "{tool}: arguments must be a JSON object"
            )));
        }
        Ok(Self { tool, value })
    }

    fn field(&self, name: &str) -> Option<&Value> {
        self.value.get(name).filter(|value| !value.is_null())
    }

    pub(crate) fn required_str(&self, name: &str) -> Result<String> {
        match self.field(name) {
            Some(Value::String(text)) => Ok(text.clone()),
            Some(other) => Err(self.wrong_type(name, "a string", other)),
            None => Err(self.missing(name)),
        }
    }

    pub(crate) fn optional_u64(&self, name: &str) -> Result<Option<u64>> {
        match self.field(name) {
            Some(Value::Number(number)) => match number.as_u64() {
                Some(value) => Ok(Some(value)),
                None => Err(HarnessError::Tool(format!(
                    "{}: argument `{}` must be a non-negative integer, found {number}",
                    self.tool, name
                ))),
            },
            Some(other) => Err(self.wrong_type(name, "an integer", other)),
            None => Ok(None),
        }
    }

    pub(crate) fn optional_usize(&self, name: &str) -> Result<Option<usize>> {
        let Some(value) = self.optional_u64(name)? else {
            return Ok(None);
        };
        usize::try_from(value).map(Some).map_err(|_| {
            HarnessError::Tool(format!(
                "{}: argument `{}` is too large for this platform",
                self.tool, name
            ))
        })
    }

    fn missing(&self, name: &str) -> HarnessError {
        HarnessError::Tool(format!(
            "{}: missing required argument `{}`",
            self.tool, name
        ))
    }

    fn wrong_type(&self, name: &str, expected: &str, found: &Value) -> HarnessError {
        HarnessError::Tool(format!(
            "{}: argument `{}` must be {expected}, found {found}",
            self.tool, name
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_required_arguments_are_named() {
        let args = Args::new("read_file", json!({})).expect("object");
        let err = args.required_str("path").unwrap_err();
        assert_eq!(
            err.to_string(),
            "tool error: read_file: missing required argument `path`"
        );
    }

    #[test]
    fn mistyped_arguments_are_named() {
        let args = Args::new("read_file", json!({ "limit": "ten" })).expect("object");
        let err = args.optional_usize("limit").unwrap_err();
        assert!(err.to_string().contains("argument `limit`"), "{err}");

        let err = Args::new("shell", json!([])).err().expect("not an object");
        assert!(err.to_string().contains("must be a JSON object"), "{err}");
    }

    #[test]
    fn null_optional_arguments_mean_absent() {
        let args = Args::new("read_file", json!({ "offset": null })).expect("object");
        assert_eq!(args.optional_usize("offset").unwrap(), None);
    }
}
