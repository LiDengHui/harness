use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{HarnessError, Result};

macro_rules! ulid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::generate())
            }

            pub fn as_ulid(&self) -> Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = HarnessError;

            fn from_str(s: &str) -> Result<Self> {
                Ulid::from_string(s)
                    .map(Self)
                    .map_err(|e| HarnessError::Other(format!("invalid {}: {e}", stringify!($name))))
            }
        }
    };
}

ulid_id!(
    /// Identifies one conversation timeline. Branching creates a new session id.
    SessionId
);
ulid_id!(
    /// Identifies one immutable node in the memory DAG.
    NodeId
);
ulid_id!(
    /// Identifies one message inside a session.
    MessageId
);
ulid_id!(
    /// Identifies one unit of work handed to a sub-agent.
    SubtaskId
);

/// Identifies an agent declared by an `.agent.md` file.
///
/// Agent ids are human-readable slugs (`backend-architect`) rather than ULIDs,
/// because they are what users type on the command line and reference in YAML.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(HarnessError::Other("agent id must not be empty".into()));
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AgentId {
    type Err = HarnessError;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_ids_are_unique_and_round_trip() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);

        let parsed: SessionId = a.to_string().parse().unwrap();
        assert_eq!(a, parsed);
    }

    #[test]
    fn agent_id_rejects_empty() {
        assert!(AgentId::new("").is_err());
        assert_eq!(
            AgentId::new("code-reviewer").unwrap().as_str(),
            "code-reviewer"
        );
    }
}
