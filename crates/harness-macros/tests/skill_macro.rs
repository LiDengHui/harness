//! `#[harness::skill]` compiles a skill's name and description into the binary.

use harness_macros::skill;

#[skill(
    name = "system-design",
    description = "Design backend systems before writing them."
)]
pub struct SystemDesign;

#[skill(
    name = "code-review",
    description = "Review a diff against its fixed point."
)]
mod code_review {}

#[test]
fn the_marker_publishes_name_and_description() {
    assert_eq!(
        SYSTEM_DESIGN_SKILL,
        (
            "system-design",
            "Design backend systems before writing them."
        )
    );
    assert_eq!(
        CODE_REVIEW_SKILL,
        ("code-review", "Review a diff against its fixed point.")
    );
}

#[test]
fn the_marker_item_survives() {
    // The attribute keeps the item it is attached to, so it can sit on whatever
    // already names the skill.
    let _ = SystemDesign;
}
