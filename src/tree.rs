//! Building the sidebar's rows from the session's tabs and their panes.
//!
//! The input is the session as the sidebar cares about it: tabs in the order
//! they are drawn, each with the panes it shows, already filtered and ordered.
//! What is derived here is everything that follows from a thing's *position*:
//! its placement, its branch, and the index it is labelled with.

use crate::agents::{Agent, Turn};
use crate::model::{Branch, Indicator, Placement, Row, RowContent, RowKey};

/// The marker an agent's row carries at its right edge.
fn indicator(turn: Turn) -> Indicator {
    match turn {
        Turn::Idle => Indicator::None,
        Turn::Working => Indicator::Working,
        Turn::Attention => Indicator::Attention,
    }
}

/// A pane, and the agent sessions running in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub id: u32,
    pub title: String,
    pub focused: bool,
    /// The agents this pane hosts. A pane hosting agents is drawn as them
    /// instead of as itself: the agent is what the user is looking for, and the
    /// pane it happens to be in is not a second thing to point at.
    pub agents: Vec<Agent>,
}

impl Pane {
    /// A pane hosting nothing, which is how every pane starts out.
    pub fn new(id: u32, title: &str, focused: bool) -> Self {
        Pane {
            id,
            title: title.to_string(),
            focused,
            agents: Vec::new(),
        }
    }
}

/// A tab and the panes it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    /// The tab's own position, which is also what labels its row.
    pub position: usize,
    pub name: String,
    pub active: bool,
    pub panes: Vec<Pane>,
}

/// Where a tab's own row sits: the active tab is one of the two rows carrying
/// the gutter, and any other tab recedes whole.
fn tab_placement(active: bool) -> Placement {
    if active {
        Placement::Here
    } else {
        Placement::Unfocused
    }
}

/// Where a pane's row sits: the other gutter row is the pane focused in the
/// active tab. A pane of a tab you are not in is `Unfocused` whether or not that
/// tab would restore you to it, because a tab you are not in recedes as a block.
fn pane_placement(tab_active: bool, focused: bool) -> Placement {
    match (tab_active, focused) {
        (false, _) => Placement::Unfocused,
        (true, true) => Placement::Here,
        (true, false) => Placement::Focused,
    }
}

/// One row hanging off a tab: a pane, or one of the agents running in a pane.
///
/// A pane contributes itself only when it hosts no agent, so the children of a
/// tab are what the user can actually point at, numbered in one sequence.
enum Child<'a> {
    Pane(&'a Pane),
    Agent(&'a Pane, &'a Agent),
}

fn children(tab: &Tab) -> Vec<Child<'_>> {
    tab.panes
        .iter()
        .flat_map(|pane| {
            if pane.agents.is_empty() {
                vec![Child::Pane(pane)]
            } else {
                pane.agents
                    .iter()
                    .map(|agent| Child::Agent(pane, agent))
                    .collect()
            }
        })
        .collect()
}

/// The rows for one tab: the tab itself, then a child per pane or agent.
fn tab_rows(tab: &Tab) -> Vec<Row> {
    let mut rows = vec![Row::new(RowContent::Window {
        index: (tab.position + 1).to_string(),
        name: tab.name.clone(),
        placement: tab_placement(tab.active),
        color: None,
    })
    .at(RowKey::Tab(tab.position))];

    let children = children(tab);
    let last = children.len().saturating_sub(1);
    for (position, child) in children.iter().enumerate() {
        let index = (position + 1).to_string();
        let branch = if position == last {
            Branch::Last
        } else {
            Branch::More
        };
        rows.push(match child {
            Child::Pane(pane) => Row::new(RowContent::Pane {
                index,
                title: pane.title.clone(),
                branch,
                placement: pane_placement(tab.active, pane.focused),
                color: None,
            })
            .at(RowKey::Pane(pane.id)),
            // An agent's placement is its pane's: the agent is where the pane
            // is, and pointing at it takes you to that pane.
            Child::Agent(pane, agent) => Row::new(RowContent::Agent {
                index,
                label: agent.label.clone(),
                branch,
                placement: pane_placement(tab.active, pane.focused),
                color: None,
            })
            .at(RowKey::Agent(agent.session.clone()))
            .with(indicator(agent.turn)),
        });
    }
    rows
}

/// The tree, in the order it is drawn and navigated.
pub fn build_tree(tabs: &[Tab]) -> Vec<Row> {
    tabs.iter().flat_map(tab_rows).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::SessionId;

    fn pane(id: u32, title: &str, focused: bool) -> Pane {
        Pane::new(id, title, focused)
    }

    fn hosting(mut pane: Pane, labels: &[&str]) -> Pane {
        pane.agents = labels
            .iter()
            .map(|label| {
                Agent::new(
                    SessionId::new(label).unwrap(),
                    "claude",
                    label,
                    Some(pane.id),
                )
            })
            .collect();
        pane
    }

    fn tab(position: usize, name: &str, active: bool, panes: Vec<Pane>) -> Tab {
        Tab {
            position,
            name: name.to_string(),
            active,
            panes,
        }
    }

    fn session() -> Vec<Tab> {
        vec![
            tab(
                0,
                "editor",
                true,
                vec![pane(1, "nvim", false), pane(2, "bash", true)],
            ),
            tab(1, "server", false, vec![pane(3, "node", true)]),
        ]
    }

    fn placements(rows: &[Row]) -> Vec<Placement> {
        rows.iter()
            .map(|row| match &row.content {
                RowContent::Window { placement, .. }
                | RowContent::Pane { placement, .. }
                | RowContent::Agent { placement, .. } => *placement,
                other => panic!("unexpected row: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_tab_is_followed_by_its_panes() {
        let rows = build_tree(&session());
        let keys: Vec<Option<RowKey>> = rows.iter().map(|row| row.key.clone()).collect();
        assert_eq!(
            keys,
            vec![
                Some(RowKey::Tab(0)),
                Some(RowKey::Pane(1)),
                Some(RowKey::Pane(2)),
                Some(RowKey::Tab(1)),
                Some(RowKey::Pane(3)),
            ]
        );
    }

    #[test]
    fn only_the_active_tab_and_its_focused_pane_are_here() {
        assert_eq!(
            placements(&build_tree(&session())),
            vec![
                Placement::Here,      // the active tab
                Placement::Focused,   // its other pane
                Placement::Here,      // the pane focused in it
                Placement::Unfocused, // a tab you are not in
                Placement::Unfocused, // and its panes, focused there or not
            ]
        );
    }

    #[test]
    fn the_last_pane_of_a_tab_closes_the_tree() {
        let rows = build_tree(&session());
        let branches: Vec<Branch> = rows
            .iter()
            .filter_map(|row| match &row.content {
                RowContent::Pane { branch, .. } => Some(*branch),
                _ => None,
            })
            .collect();
        assert_eq!(branches, vec![Branch::More, Branch::Last, Branch::Last]);
    }

    #[test]
    fn rows_are_labelled_with_their_one_based_position() {
        let rows = build_tree(&session());
        assert_eq!(indices(&rows), vec!["1", "1", "2", "2", "1"]);
    }

    fn indices(rows: &[Row]) -> Vec<&str> {
        rows.iter()
            .map(|row| match &row.content {
                RowContent::Window { index, .. }
                | RowContent::Pane { index, .. }
                | RowContent::Agent { index, .. } => index.as_str(),
                other => panic!("unexpected row: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_pane_hosting_an_agent_is_drawn_as_that_agent() {
        let panes = vec![hosting(pane(1, "bash", true), &["wrangler"])];
        let rows = build_tree(&[tab(0, "editor", true, panes)]);
        assert_eq!(
            rows[1].content,
            RowContent::Agent {
                index: "1".to_string(),
                label: "wrangler".to_string(),
                branch: Branch::Last,
                placement: Placement::Here,
                color: None,
            }
        );
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn a_pane_hosting_two_agents_contributes_two_rows() {
        let panes = vec![hosting(pane(1, "bash", false), &["one", "two"])];
        let rows = build_tree(&[tab(0, "editor", true, panes)]);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter()
                .skip(1)
                .map(|row| row.key.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(RowKey::Agent(SessionId::new("one").unwrap())),
                Some(RowKey::Agent(SessionId::new("two").unwrap())),
            ]
        );
    }

    #[test]
    fn agents_and_panes_are_numbered_in_one_sequence() {
        // The second pane hosts two agents, so the plain pane after it is the
        // fourth child rather than the third.
        let panes = vec![
            pane(1, "nvim", false),
            hosting(pane(2, "bash", false), &["one", "two"]),
            pane(3, "cargo", false),
        ];
        let rows = build_tree(&[tab(0, "editor", true, panes)]);
        assert_eq!(indices(&rows), vec!["1", "1", "2", "3", "4"]);
    }

    #[test]
    fn the_last_agent_of_a_tab_closes_the_tree() {
        let panes = vec![
            pane(1, "nvim", false),
            hosting(pane(2, "bash", false), &["one"]),
        ];
        let rows = build_tree(&[tab(0, "editor", true, panes)]);
        assert_eq!(
            rows.iter()
                .skip(1)
                .map(|row| match &row.content {
                    RowContent::Pane { branch, .. } | RowContent::Agent { branch, .. } => *branch,
                    other => panic!("unexpected row: {other:?}"),
                })
                .collect::<Vec<_>>(),
            vec![Branch::More, Branch::Last]
        );
    }

    #[test]
    fn an_agents_turn_is_the_marker_its_row_carries() {
        for (turn, marker) in [
            (Turn::Idle, Indicator::None),
            (Turn::Working, Indicator::Working),
            (Turn::Attention, Indicator::Attention),
        ] {
            let mut pane = hosting(pane(1, "bash", false), &["one"]);
            pane.agents[0].turn = turn;
            let rows = build_tree(&[tab(0, "editor", true, vec![pane])]);
            assert_eq!(rows[1].indicator, marker, "{turn:?}");
        }
    }

    #[test]
    fn a_pane_row_never_carries_a_marker() {
        let rows = build_tree(&session());
        assert!(rows.iter().all(|row| row.indicator == Indicator::None));
    }

    #[test]
    fn an_agent_sits_where_its_pane_sits() {
        let panes = vec![hosting(pane(1, "bash", true), &["one"])];
        let rows = build_tree(&[tab(0, "editor", false, panes)]);
        assert_eq!(placements(&rows), vec![Placement::Unfocused; 2]);
    }

    #[test]
    fn a_tab_with_no_panes_still_draws_its_own_row() {
        let rows = build_tree(&[tab(0, "empty", false, vec![])]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, Some(RowKey::Tab(0)));
    }
}
