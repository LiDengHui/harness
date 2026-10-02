//! The permission tier a run operates under, and the risk a tool call carries.
//!
//! The tier is deliberately coarse — three settings a person can hold in mind —
//! and the risk classification is a fixed table rather than a per-tool property
//! so that a tool the harness has never heard of still lands somewhere safe.

use serde::{Deserialize, Serialize};

/// How much the harness asks a human before running a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Every tool call is put to the user, including a read.
    AlwaysAsk,
    /// Only calls that could change something are put to the user.
    AskWhenNeeded,
    /// Nothing is put to the user; the policy's hard blocks still apply.
    ///
    /// This is the default because it is the behaviour every run had before the
    /// tier existed: a run with no human attached must not stall on a question
    /// nobody can answer.
    #[default]
    FullAuto,
}

impl PermissionMode {
    /// Parses a mode from configuration or wire text, ignoring case and
    /// surrounding whitespace. Anything else is `None`, so a caller can fall
    /// back deliberately instead of acting on a value it did not understand.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("always_ask") {
            Some(Self::AlwaysAsk)
        } else if text.eq_ignore_ascii_case("ask_when_needed") {
            Some(Self::AskWhenNeeded)
        } else if text.eq_ignore_ascii_case("full_auto") {
            Some(Self::FullAuto)
        } else {
            None
        }
    }

    /// The canonical spelling, which is also what serde puts on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlwaysAsk => "always_ask",
            Self::AskWhenNeeded => "ask_when_needed",
            Self::FullAuto => "full_auto",
        }
    }
}

/// What a tool call could do, as far as the permission tier is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRisk {
    Read,
    Write,
    Execute,
}

/// The risk of a tool by name.
///
/// The table is built in rather than declared on each tool because the decision
/// has to hold for tools this crate has never seen: an MCP tool is treated as
/// arbitrary code, and a name that matches nothing is treated as consequential.
/// Guessing "safe" for an unknown tool would silently widen what a cautious mode
/// lets through.
pub fn classify(tool: &str) -> ToolRisk {
    match tool {
        "read_file" | "list_dir" | "grep" | "web_fetch" => ToolRisk::Read,
        "write_file" | "edit_file" => ToolRisk::Write,
        "shell" => ToolRisk::Execute,
        name if name.starts_with("mcp__") => ToolRisk::Execute,
        _ => ToolRisk::Write,
    }
}

/// Whether `mode` puts a call of this risk to the user.
///
/// `FullAuto` never asks; `AlwaysAsk` always does; `AskWhenNeeded` asks for
/// anything that is not a read, because a read cannot change the workspace.
pub fn mode_requires_approval(mode: PermissionMode, risk: ToolRisk) -> bool {
    match mode {
        PermissionMode::FullAuto => false,
        PermissionMode::AlwaysAsk => true,
        PermissionMode::AskWhenNeeded => risk != ToolRisk::Read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_from_their_canonical_spellings_and_round_trip() {
        for mode in [
            PermissionMode::AlwaysAsk,
            PermissionMode::AskWhenNeeded,
            PermissionMode::FullAuto,
        ] {
            assert_eq!(PermissionMode::parse(mode.as_str()), Some(mode));
        }

        // Case and surrounding whitespace are not part of the value.
        assert_eq!(
            PermissionMode::parse("  ALWAYS_ASK "),
            Some(PermissionMode::AlwaysAsk)
        );
        assert_eq!(
            PermissionMode::parse("Ask_When_Needed"),
            Some(PermissionMode::AskWhenNeeded)
        );

        // Anything else is refused rather than guessed at.
        assert_eq!(PermissionMode::parse("sometimes"), None);
        assert_eq!(PermissionMode::parse(""), None);
        assert_eq!(PermissionMode::parse("   "), None);
        assert_eq!(PermissionMode::parse("ask"), None);
    }

    #[test]
    fn a_mode_serializes_to_the_spelling_it_parses_from() {
        let json = serde_json::to_value(PermissionMode::AskWhenNeeded).unwrap();
        assert_eq!(json, serde_json::json!("ask_when_needed"));
        let back: PermissionMode = serde_json::from_value(json).unwrap();
        assert_eq!(back, PermissionMode::AskWhenNeeded);
    }

    #[test]
    fn known_tools_are_classified_by_what_they_can_do() {
        for tool in ["read_file", "list_dir", "grep", "web_fetch"] {
            assert_eq!(classify(tool), ToolRisk::Read, "{tool}");
        }
        for tool in ["write_file", "edit_file"] {
            assert_eq!(classify(tool), ToolRisk::Write, "{tool}");
        }
        assert_eq!(classify("shell"), ToolRisk::Execute);
        assert_eq!(classify("mcp__files__move"), ToolRisk::Execute);
    }

    #[test]
    fn an_unknown_tool_is_treated_as_consequential() {
        // Neither a read nor a recognised write: the safe answer is to make it
        // ask rather than let it through unseen.
        assert_eq!(classify("some_future_tool"), ToolRisk::Write);
        assert!(mode_requires_approval(
            PermissionMode::AskWhenNeeded,
            classify("some_future_tool")
        ));
    }

    #[test]
    fn each_mode_asks_about_the_risks_it_says_it_does() {
        use PermissionMode::{AlwaysAsk, AskWhenNeeded, FullAuto};
        use ToolRisk::{Execute, Read, Write};

        for risk in [Read, Write, Execute] {
            assert!(mode_requires_approval(AlwaysAsk, risk), "{risk:?}");
            assert!(!mode_requires_approval(FullAuto, risk), "{risk:?}");
        }

        assert!(!mode_requires_approval(AskWhenNeeded, Read));
        assert!(mode_requires_approval(AskWhenNeeded, Write));
        assert!(mode_requires_approval(AskWhenNeeded, Execute));
    }
}
