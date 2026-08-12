//! Building a client's rows from the session's tabs and their panes.
//!
//! The input is the session as a client cares about it: tabs in the order
//! they are drawn, each with the panes it shows, already filtered and ordered.
//! What is derived here is everything that follows from a thing's *position*:
//! its placement, its branch, and the index it is labelled with.

use agent_wrangler_core::agent::{Agent, Turn};
use agent_wrangler_core::label::label;

use crate::model::{Branch, Indicator, NamedColor, Placement, Row, RowContent, RowKey};
use crate::options::View;

/// The marker an agent's row carries at its right edge, which is nothing at all
/// when the client has been asked not to say whose turn it is.
fn indicator(agent: &Agent, options: &View) -> Indicator {
    match (options.turn_state, agent.turn) {
        (false, _) | (_, Turn::Idle) => Indicator::None,
        (_, Turn::Working) => Indicator::Working,
        (_, Turn::Attention) => Indicator::Attention,
    }
}

/// The row one agent draws, wherever it is drawn.
fn agent_row(
    agent: &Agent,
    index: String,
    branch: Branch,
    placement: Placement,
    options: &View,
) -> Row {
    Row::new(RowContent::Agent {
        index,
        label: label(agent, options.label),
        branch,
        placement,
        color: NamedColor::of(agent),
    })
    .with(indicator(agent, options))
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

/// The row a tab draws for itself.
fn window_row(tab: &Tab) -> Row {
    Row::new(RowContent::Window {
        index: (tab.position + 1).to_string(),
        name: tab.name.clone(),
        placement: tab_placement(tab.active),
        color: None,
    })
}

/// The branch a child at `position` carries, given the position of the last one.
fn branch_at(position: usize, last: usize) -> Branch {
    if position == last {
        Branch::Last
    } else {
        Branch::More
    }
}

/// The rows for one tab: the tab itself, then a child per pane or agent.
fn tab_rows(tab: &Tab, options: &View) -> Vec<Row> {
    let mut rows = vec![window_row(tab).at(RowKey::Tab(tab.position))];

    let children = children(tab);
    let last = children.len().saturating_sub(1);
    for (position, child) in children.iter().enumerate() {
        let index = (position + 1).to_string();
        let branch = branch_at(position, last);
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
            Child::Agent(pane, agent) => agent_row(
                agent,
                index,
                branch,
                pane_placement(tab.active, pane.focused),
                options,
            )
            .at(RowKey::Agent(agent.session.clone())),
        });
    }
    rows
}

/// Every agent a session is running, in the order their blocks are drawn.
fn kinds(tabs: &[Tab]) -> Vec<&str> {
    let mut kinds: Vec<&str> = tabs
        .iter()
        .flat_map(|tab| tab.panes.iter())
        .flat_map(|pane| pane.agents.iter())
        .map(|agent| agent.agent.as_str())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

/// One agent's block: the same sessions the tree holds, gathered under the tabs
/// they are in and with everything else left out.
///
/// A tab's row appears here as a heading for the sessions beneath it and points
/// at nothing: the tab itself is one row up in the tree, and one thing worth
/// selecting twice would be two rows the selection has to tell apart.
fn kind_rows(tabs: &[Tab], kind: &str, options: &View) -> Vec<Row> {
    let mut rows = vec![Row::new(RowContent::Header {
        text: kind.to_string(),
    })];
    for tab in tabs {
        let agents: Vec<(&Pane, &Agent)> = tab
            .panes
            .iter()
            .flat_map(|pane| pane.agents.iter().map(move |agent| (pane, agent)))
            .filter(|(_, agent)| agent.agent == kind)
            .collect();
        if agents.is_empty() {
            continue;
        }
        rows.push(window_row(tab));
        let last = agents.len() - 1;
        for (position, (pane, agent)) in agents.iter().enumerate() {
            rows.push(
                agent_row(
                    agent,
                    (position + 1).to_string(),
                    branch_at(position, last),
                    pane_placement(tab.active, pane.focused),
                    options,
                )
                .at(RowKey::Section(agent.session.clone())),
            );
        }
    }
    rows
}

/// The rows a client draws, in the order they are drawn and navigated.
///
/// In sections mode the same sessions are drawn twice, once where they are and
/// once under what they are, so both blocks carry a heading saying which is
/// which. Grouping is all the option changes: a tab, a pane and an agent are
/// drawn exactly the same wherever they appear.
pub fn build_tree(tabs: &[Tab], options: &View) -> Vec<Row> {
    let tree = tabs.iter().flat_map(|tab| tab_rows(tab, options));
    if !options.sections {
        return tree.collect();
    }
    let mut rows = vec![Row::new(RowContent::Header {
        text: "tabs".to_string(),
    })];
    rows.extend(tree);
    for kind in kinds(tabs) {
        rows.extend(kind_rows(tabs, kind, options));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    use agent_wrangler_core::agent::{Meta, SessionId};
    use agent_wrangler_core::label::Label;
    use agent_wrangler_core::origin::Origin;

    fn pane(id: u32, title: &str, focused: bool) -> Pane {
        Pane::new(id, title, focused)
    }

    fn hosting(pane: Pane, labels: &[&str]) -> Pane {
        running(pane, "claude", labels)
    }

    /// The agents a pane hosts are handed to it here, so where a record says it
    /// was raised is nothing this has to answer for: a tree is built from panes
    /// that already hold their agents.
    fn running(mut pane: Pane, kind: &str, labels: &[&str]) -> Pane {
        pane.agents = labels
            .iter()
            .map(|label| {
                Agent::new(
                    SessionId::new(label).unwrap(),
                    kind,
                    Meta {
                        dir: label.to_string(),
                        ..Meta::default()
                    },
                    Origin::default(),
                )
            })
            .collect();
        pane
    }

    fn tree(tabs: &[Tab]) -> Vec<Row> {
        build_tree(tabs, &View::default())
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
        let rows = tree(&session());
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
            placements(&tree(&session())),
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
        let rows = tree(&session());
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
        let rows = tree(&session());
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
        let rows = tree(&[tab(0, "editor", true, panes)]);
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
        let rows = tree(&[tab(0, "editor", true, panes)]);
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
        let rows = tree(&[tab(0, "editor", true, panes)]);
        assert_eq!(indices(&rows), vec!["1", "1", "2", "3", "4"]);
    }

    #[test]
    fn the_last_agent_of_a_tab_closes_the_tree() {
        let panes = vec![
            pane(1, "nvim", false),
            hosting(pane(2, "bash", false), &["one"]),
        ];
        let rows = tree(&[tab(0, "editor", true, panes)]);
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
            let rows = tree(&[tab(0, "editor", true, vec![pane])]);
            assert_eq!(rows[1].indicator, marker, "{turn:?}");
        }
    }

    #[test]
    fn a_pane_row_never_carries_a_marker() {
        let rows = tree(&session());
        assert!(rows.iter().all(|row| row.indicator == Indicator::None));
    }

    #[test]
    fn an_agent_sits_where_its_pane_sits() {
        let panes = vec![hosting(pane(1, "bash", true), &["one"])];
        let rows = tree(&[tab(0, "editor", false, panes)]);
        assert_eq!(placements(&rows), vec![Placement::Unfocused; 2]);
    }

    #[test]
    fn a_tab_with_no_panes_still_draws_its_own_row() {
        let rows = tree(&[tab(0, "empty", false, vec![])]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, Some(RowKey::Tab(0)));
    }

    #[test]
    fn a_marker_is_not_drawn_at_all_when_the_turn_is_not_wanted() {
        let mut pane = hosting(pane(1, "bash", false), &["one"]);
        pane.agents[0].turn = Turn::Attention;
        let options = View {
            turn_state: false,
            ..View::default()
        };
        let rows = build_tree(&[tab(0, "editor", true, vec![pane])], &options);
        assert_eq!(rows[1].indicator, Indicator::None);
    }

    #[test]
    fn an_agent_row_is_called_what_the_label_option_asks_for() {
        let mut pane = hosting(pane(1, "bash", false), &["one"]);
        pane.agents[0].meta.title = "the zellij port".to_string();
        let tabs = [tab(0, "editor", true, vec![pane])];
        let label = |mode| {
            let options = View {
                label: mode,
                ..View::default()
            };
            match &build_tree(&tabs, &options)[1].content {
                RowContent::Agent { label, .. } => label.clone(),
                other => panic!("unexpected row: {other:?}"),
            }
        };
        assert_eq!(label(Label::Name), "the zellij port");
        assert_eq!(label(Label::Dir), "one");
    }

    fn sectioned() -> Vec<Tab> {
        vec![
            tab(
                0,
                "editor",
                true,
                vec![
                    pane(1, "nvim", false),
                    running(pane(2, "bash", true), "claude", &["a"]),
                ],
            ),
            tab(
                1,
                "notes",
                false,
                vec![running(pane(3, "bash", false), "copilot", &["b"])],
            ),
        ]
    }

    fn sections(tabs: &[Tab]) -> Vec<Row> {
        build_tree(
            tabs,
            &View {
                sections: true,
                ..View::default()
            },
        )
    }

    #[test]
    fn sections_draw_the_tree_first_and_a_block_per_agent_after_it() {
        let rows = sections(&sectioned());
        let headings: Vec<&str> = rows
            .iter()
            .filter_map(|row| match &row.content {
                RowContent::Header { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headings, vec!["tabs", "claude", "copilot"]);
        // The tree is what it was, heading aside.
        assert_eq!(rows[1..6], tree(&sectioned())[..]);
    }

    #[test]
    fn a_block_holds_its_own_agents_under_the_tabs_they_are_in() {
        let rows = sections(&sectioned());
        let keys: Vec<Option<RowKey>> = rows.iter().map(|row| row.key.clone()).collect();
        assert_eq!(
            keys[6..],
            [
                None, // CLAUDE
                None, // its tab
                Some(RowKey::Section(SessionId::new("a").unwrap())),
                None, // COPILOT
                None, // its tab
                Some(RowKey::Section(SessionId::new("b").unwrap())),
            ]
        );
    }

    #[test]
    fn a_tab_holding_none_of_an_agents_sessions_is_left_out_of_its_block() {
        let rows = sections(&sectioned());
        let names: Vec<&str> = rows
            .iter()
            .skip(6)
            .filter_map(|row| match &row.content {
                RowContent::Window { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["editor", "notes"]);
    }

    #[test]
    fn a_session_drawn_twice_is_drawn_the_same_both_times() {
        // The option groups and nothing else. What differs is what position
        // means: a session is the second child of its tab and the first of its
        // block, and its index says where it is in the list it is drawn in.
        let rows = sections(&sectioned());
        let (tree_row, section_row) = (&rows[3], &rows[8]);
        let same = |row: &Row| match &row.content {
            RowContent::Agent {
                label,
                branch,
                placement,
                ..
            } => (label.clone(), *branch, *placement, row.indicator),
            other => panic!("unexpected row: {other:?}"),
        };
        assert_eq!(same(tree_row), same(section_row));
        assert_ne!(tree_row.key, section_row.key);
    }

    #[test]
    fn nothing_is_added_by_sections_when_no_agent_is_running() {
        let rows = sections(&session());
        assert_eq!(
            rows[0].content,
            RowContent::Header {
                text: "tabs".into()
            }
        );
        assert_eq!(rows[1..], tree(&session())[..]);
    }
}
