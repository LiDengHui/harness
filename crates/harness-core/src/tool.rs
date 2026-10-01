//! The provider-facing description of a callable tool.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tokens::TokenEstimator;

/// Everything a model needs in order to decide to call a tool.
///
/// This lives in `harness-core` rather than in the tools crate because it is
/// also part of the provider wire protocol, and because the token report needs
/// to measure the cost of advertising a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

impl ToolSpec {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// What it costs to advertise this tool to the model.
    ///
    /// The deferred-schema optimisation depends on being able to measure this:
    /// tools that are not advertised cost nothing.
    pub fn advertised_tokens(&self, estimator: &dyn TokenEstimator) -> usize {
        const ENVELOPE: usize = 8;
        estimator.estimate(&self.name)
            + estimator.estimate(&self.description)
            + estimator.estimate(&self.parameters.to_string())
            + ENVELOPE
    }
}

/// Builds an object schema that rejects unknown properties.
///
/// Every builtin tool uses this so that a malformed argument object fails loudly
/// instead of being silently ignored.
pub fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::HeuristicEstimator;

    #[test]
    fn object_schema_marks_required_fields() {
        let schema = object_schema(json!({ "path": { "type": "string" } }), &["path"]);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "path");
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn advertised_cost_grows_with_the_description() {
        let estimator = HeuristicEstimator::default();
        let small = ToolSpec::new("a", "short", json!({}));
        let large = ToolSpec::new("a", "a much longer description of the tool", json!({}));
        assert!(large.advertised_tokens(&estimator) > small.advertised_tokens(&estimator));
    }
}
