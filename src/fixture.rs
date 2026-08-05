//! The hardcoded rows and notifications the prototype draws.
//!
//! They are shaped to cover every case the paint has to handle: the tab you are
//! in and two you are not, a pane and an agent sharing a tab, a mid-turn agent
//! and one waiting on you, and a notification whose message is long enough to
//! wrap.

use crate::model::{Branch, Indicator, NamedColor, Placement, Row, RowContent};

/// A notification entry: the agent that raised it and the message it carries.
pub struct Notification {
    pub agent: &'static str,
    pub color: Option<NamedColor>,
    pub message: &'static str,
}

fn tab(index: &str, name: &str, placement: Placement) -> Row {
    Row::new(RowContent::Window {
        index: index.to_string(),
        name: name.to_string(),
        placement,
        color: None,
    })
}

fn pane(index: &str, title: &str, branch: Branch, placement: Placement) -> Row {
    Row::new(RowContent::Pane {
        index: index.to_string(),
        title: title.to_string(),
        branch,
        placement,
        color: None,
    })
}

fn agent(
    index: &str,
    label: &str,
    branch: Branch,
    placement: Placement,
    color: NamedColor,
    indicator: Indicator,
) -> Row {
    Row::new(RowContent::Agent {
        index: index.to_string(),
        label: label.to_string(),
        branch,
        placement,
        color: Some(color),
    })
    .with(indicator)
}

/// The tab tree, in the order it is drawn and navigated.
pub fn tree() -> Vec<Row> {
    vec![
        tab("1", "wrangler", Placement::Here),
        pane("0", "nvim", Branch::More, Placement::Focused),
        agent(
            "1",
            "claude · wrangler",
            Branch::More,
            Placement::Here,
            NamedColor::Cyan,
            Indicator::Working,
        ),
        pane("2", "cargo watch", Branch::Last, Placement::Focused),
        tab("2", "notes", Placement::Unfocused),
        pane("0", "nvim", Branch::More, Placement::Unfocused),
        agent(
            "1",
            "copilot · docs",
            Branch::Last,
            Placement::Unfocused,
            NamedColor::Magenta,
            Indicator::Attention,
        ),
        tab("3", "infra", Placement::Unfocused),
        pane("0", "ssh prod-1", Branch::More, Placement::Unfocused),
        pane("1", "k9s", Branch::Last, Placement::Unfocused),
    ]
}

/// The notification entries, newest first.
pub fn notifications() -> Vec<Notification> {
    vec![Notification {
        agent: "copilot",
        color: Some(NamedColor::Magenta),
        message: "Permission required to run cargo test --release",
    }]
}
