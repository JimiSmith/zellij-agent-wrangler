//! How the rows of a client are built from the tabs of the session and their
//! panes.
//!
//! The input is the session as a client cares about it. The input holds the
//! tabs in the order they are drawn. Each tab holds the panes it shows, already
//! filtered and ordered. What is derived here is everything that follows from
//! the *position* of a thing: its placement, its branch, and the index it is
//! labeled with.

use agent_wrangler_core::agent::{Agent, Turn};
use agent_wrangler_core::label::label;

use crate::model::{
    Branch, Indicator, NamedColor, PaneId, Placement, Row, RowContent, RowKey, TabId, TabPosition,
};
use crate::options::DrawingOptions;

/// The marker the row of an agent carries at its right edge. If the client is
/// asked not to say whose turn it is, the marker is nothing at all.
fn indicator(agent: &Agent, options: &DrawingOptions) -> Indicator {
    match (options.turn_state, agent.turn) {
        (false, _) | (_, Turn::Idle) => Indicator::None,
        (_, Turn::Working) => Indicator::Working,
        (_, Turn::Attention) => Indicator::Attention,
    }
}

/// The rows one agent draws, wherever it is drawn: its own row, and the status
/// line under it when the template spells one.
///
/// Both rows carry `key`. The selection bar therefore covers the pair, and a
/// click anywhere in the pair reaches the agent. [`selectable_row_keys`] folds
/// the two back to one thing to navigate.
///
/// [`selectable_row_keys`]: crate::selection::selectable_row_keys
fn agent_rows(
    agent: &Agent,
    index: String,
    branch: Branch,
    placement: Placement,
    options: &DrawingOptions,
    key: RowKey,
) -> Vec<Row> {
    let mut rows = vec![Row::new(RowContent::Agent {
        index: index.clone(),
        label: label(agent, options.label),
        branch,
        placement,
        color: NamedColor::for_agent(agent),
    })
    .with_indicator(indicator(agent, options))
    .with_key(key.clone())];
    let spelled = options
        .status_line
        .as_ref()
        .and_then(|template| template.spell(agent));
    if let Some(text) = spelled {
        rows.push(
            Row::new(RowContent::AgentStatus {
                index,
                text,
                branch,
                placement,
            })
            .with_key(key),
        );
    }
    rows
}

/// A pane, and the agent sessions that run in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub id: PaneId,
    pub title: String,
    pub focused: bool,
    /// The agents this pane hosts. A pane with agents is drawn as those agents
    /// instead of as itself. The user looks for the agent, and the pane the
    /// agent is in is not a second thing to point at.
    pub agents: Vec<Agent>,
}

impl Pane {
    /// A pane that hosts nothing, which is how every pane starts.
    pub fn new(id: impl Into<PaneId>, title: &str, focused: bool) -> Self {
        Pane {
            id: id.into(),
            title: title.to_string(),
            focused,
            agents: Vec::new(),
        }
    }
}

/// A tab and the panes it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    /// The tab's own stable identity.
    pub id: TabId,
    /// The current position of the tab, which orders its row.
    pub position: TabPosition,
    /// The number that the row draws in front of the name.
    ///
    /// The order and the number are two facts. A host is free to number its
    /// tabs from any value, and to leave a gap where a tab closed. So the row
    /// draws this and never the position.
    pub displayed_index: String,
    pub name: String,
    pub active: bool,
    pub panes: Vec<Pane>,
}

/// Where a tab's own row sits: the active tab is one of the two rows that carry
/// the gutter, and any other tab recedes whole.
fn tab_placement(active: bool) -> Placement {
    if active {
        Placement::FocusedPane
    } else {
        Placement::OtherTab
    }
}

/// Where the row of a pane sits: the other gutter row is the pane focused in the
/// active tab. A pane of a tab you are not in is always `Unfocused`. That pane
/// can be the pane the tab restores you to, or not. A tab you are not in recedes
/// as a block.
fn pane_placement(tab_active: bool, focused: bool) -> Placement {
    match (tab_active, focused) {
        (false, _) => Placement::OtherTab,
        (true, true) => Placement::FocusedPane,
        (true, false) => Placement::SameTab,
    }
}

/// One row that hangs off a tab: a pane, or one of the agents that run in a
/// pane.
///
/// A pane that hosts no agent contributes itself. The children of a tab are
/// therefore what the user can point at, numbered in one sequence.
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
    Row::new(RowContent::Tab {
        index: tab.displayed_index.clone(),
        name: tab.name.clone(),
        placement: tab_placement(tab.active),
        color: None,
    })
}

/// The branch a child at `position` carries, with `last` as the position of the
/// last child.
fn branch_at(position: usize, last: usize) -> Branch {
    if position == last {
        Branch::Last
    } else {
        Branch::More
    }
}

/// The rows for one tab: the tab itself, then a child per pane or agent.
fn tab_rows(tab: &Tab, options: &DrawingOptions) -> Vec<Row> {
    let mut rows = vec![window_row(tab).with_key(RowKey::Tab(tab.id.clone()))];

    let children = children(tab);
    let last = children.len().saturating_sub(1);
    for (position, child) in children.iter().enumerate() {
        let index = (position + 1).to_string();
        let branch = branch_at(position, last);
        match child {
            Child::Pane(pane) => rows.push(
                Row::new(RowContent::Pane {
                    index,
                    title: pane.title.clone(),
                    branch,
                    placement: pane_placement(tab.active, pane.focused),
                    color: None,
                })
                .with_key(RowKey::Pane(pane.id.clone())),
            ),
            // An agent takes the placement of its pane. The agent is where the
            // pane is, and the row takes you to that pane.
            Child::Agent(pane, agent) => rows.extend(agent_rows(
                agent,
                index,
                branch,
                pane_placement(tab.active, pane.focused),
                options,
                RowKey::Agent(agent.session.clone()),
            )),
        }
    }
    rows
}

/// Every agent a session runs, in the order their blocks are drawn.
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

/// The block of one agent: the same sessions the tree holds, gathered under the
/// tabs they are in and with everything else left out.
///
/// The row of a tab appears here as a heading for the sessions under it, and it
/// points at nothing. The tab itself is one row up in the tree, and two rows for
/// one thing are two rows the selection must tell apart.
fn kind_rows(tabs: &[Tab], kind: &str, options: &DrawingOptions) -> Vec<Row> {
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
            rows.extend(agent_rows(
                agent,
                (position + 1).to_string(),
                branch_at(position, last),
                pane_placement(tab.active, pane.focused),
                options,
                RowKey::Section(agent.session.clone()),
            ));
        }
    }
    rows
}

/// The rows a client draws, in the order they are drawn and navigated.
///
/// In sections mode the same sessions are drawn twice, once where they are and
/// once under what they are. Both blocks therefore carry a heading that says
/// which is which. The option changes the grouping and nothing else: a tab, a
/// pane and an agent are drawn the same wherever they appear.
pub fn build_tree(tabs: &[Tab], options: &DrawingOptions) -> Vec<Row> {
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

    use agent_wrangler_core::agent::{LabelFacts, SessionId, StatusFacts};
    use agent_wrangler_core::label::Label;
    use agent_wrangler_core::origin::Origin;
    use agent_wrangler_core::status_line::StatusTemplate;

    fn pane(id: u32, title: &str, focused: bool) -> Pane {
        Pane::new(id, title, focused)
    }

    fn hosting(pane: Pane, labels: &[&str]) -> Pane {
        running(pane, "claude", labels)
    }

    /// The agents a pane hosts are handed to it here. The place a record names
    /// as its origin is nothing this helper answers for. A tree is built from
    /// panes that already hold their agents.
    fn running(mut pane: Pane, kind: &str, labels: &[&str]) -> Pane {
        pane.agents = labels
            .iter()
            .map(|label| {
                Agent::new(
                    SessionId::new(label).unwrap(),
                    kind,
                    LabelFacts {
                        dir: label.to_string(),
                        ..LabelFacts::default()
                    },
                    Origin::default(),
                )
            })
            .collect();
        pane
    }

    fn tree(tabs: &[Tab]) -> Vec<Row> {
        build_tree(tabs, &DrawingOptions::default())
    }

    fn tab(position: usize, name: &str, active: bool, panes: Vec<Pane>) -> Tab {
        Tab {
            id: TabId::new(name),
            position: TabPosition::at(position),
            displayed_index: (position + 1).to_string(),
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
                RowContent::Tab { placement, .. }
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
                Some(RowKey::Tab(TabId::new("editor"))),
                Some(RowKey::Pane(1.into())),
                Some(RowKey::Pane(2.into())),
                Some(RowKey::Tab(TabId::new("server"))),
                Some(RowKey::Pane(3.into())),
            ]
        );
    }

    #[test]
    fn only_the_active_tab_and_its_focused_pane_are_here() {
        assert_eq!(
            placements(&tree(&session())),
            vec![
                Placement::FocusedPane, // the active tab
                Placement::SameTab,     // its other pane
                Placement::FocusedPane, // the pane focused in it
                Placement::OtherTab,    // a tab you are not in
                Placement::OtherTab,    // and its panes, focused there or not
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

    #[test]
    fn moving_a_tab_changes_its_label_but_not_its_key() {
        let before = tree(&[tab(1, "editor", false, vec![])]);
        let after = tree(&[tab(0, "editor", false, vec![])]);
        assert_eq!(before[0].key, after[0].key);
        assert_eq!(indices(&before), vec!["2"]);
        assert_eq!(indices(&after), vec!["1"]);
    }

    fn indices(rows: &[Row]) -> Vec<&str> {
        rows.iter()
            .map(|row| match &row.content {
                RowContent::Tab { index, .. }
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
                placement: Placement::FocusedPane,
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
        assert_eq!(placements(&rows), vec![Placement::OtherTab; 2]);
    }

    #[test]
    fn a_tab_with_no_panes_still_draws_its_own_row() {
        let rows = tree(&[tab(0, "empty", false, vec![])]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, Some(RowKey::Tab(TabId::new("empty"))));
    }

    #[test]
    fn a_marker_is_not_drawn_at_all_when_the_turn_is_not_wanted() {
        let mut pane = hosting(pane(1, "bash", false), &["one"]);
        pane.agents[0].turn = Turn::Attention;
        let options = DrawingOptions {
            turn_state: false,
            ..DrawingOptions::default()
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
            let options = DrawingOptions {
                label: mode,
                ..DrawingOptions::default()
            };
            match &build_tree(&tabs, &options)[1].content {
                RowContent::Agent { label, .. } => label.clone(),
                other => panic!("unexpected row: {other:?}"),
            }
        };
        assert_eq!(label(Label::Name), "the zellij port");
        assert_eq!(label(Label::Dir), "one");
    }

    /// Options that ask for a status line spelled from the template.
    fn showing(template: &str) -> DrawingOptions {
        DrawingOptions {
            status_line: StatusTemplate::new(template),
            ..DrawingOptions::default()
        }
    }

    /// One pane hosting one agent that reports what it works with.
    fn working() -> Vec<Tab> {
        let mut pane = hosting(pane(1, "bash", true), &["one"]);
        pane.agents[0].status = StatusFacts {
            branch: "main".to_string(),
            model: "claude-opus-5".to_string(),
            context_tokens: 41_000,
        };
        vec![tab(0, "editor", true, vec![pane])]
    }

    #[test]
    fn a_status_row_follows_the_agent_it_describes() {
        let rows = build_tree(
            &working(),
            &showing("{branch} · {model} · {context_tokens}"),
        );
        assert_eq!(rows.len(), 3, "the tab, the agent, and its status line");
        assert_eq!(
            rows[2].content,
            RowContent::AgentStatus {
                index: "1".to_string(),
                text: "main · opus-5 · 41k".to_string(),
                branch: Branch::Last,
                placement: Placement::FocusedPane,
            }
        );
    }

    #[test]
    fn a_status_row_takes_the_index_and_the_branch_of_its_agent() {
        // A second pane makes the agent a child with a sibling after it, so the
        // tree goes on below its status line.
        let mut tabs = working();
        tabs[0].panes.push(pane(2, "nvim", false));
        let rows = build_tree(&tabs, &showing("{branch}"));
        let RowContent::AgentStatus { index, branch, .. } = &rows[2].content else {
            panic!("unexpected row: {:?}", rows[2].content);
        };
        assert_eq!(index, "1");
        assert_eq!(*branch, Branch::More);
    }

    #[test]
    fn no_status_row_is_drawn_without_a_template() {
        let rows = build_tree(&working(), &DrawingOptions::default());
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn no_status_row_is_drawn_for_an_agent_that_reports_none_of_the_values() {
        // The template names three fields and the record fills none of them. A
        // row of bare separators says nothing, so no row is drawn.
        let quiet = vec![tab(
            0,
            "editor",
            true,
            vec![hosting(pane(1, "bash", true), &["one"])],
        )];
        let rows = build_tree(&quiet, &showing("{branch} · {model} · {context_tokens}"));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn a_status_row_carries_the_key_of_the_agent_above_it() {
        // One key over the pair puts the selection bar across both rows and
        // sends a click on either one to the same agent.
        let rows = build_tree(&working(), &showing("{branch}"));
        assert_eq!(rows[1].key, rows[2].key);
        assert_eq!(
            rows[2].key,
            Some(RowKey::Agent(SessionId::new("one").unwrap()))
        );
    }

    #[test]
    fn a_status_row_in_a_block_carries_that_blocks_own_key() {
        let rows = build_tree(
            &working(),
            &DrawingOptions {
                sections: true,
                ..showing("{branch}")
            },
        );
        let section = Some(RowKey::Section(SessionId::new("one").unwrap()));
        let keys: Vec<Option<RowKey>> = rows.iter().map(|row| row.key.clone()).collect();
        assert_eq!(keys.iter().filter(|key| **key == section).count(), 2);
    }

    #[test]
    fn a_status_row_never_carries_a_marker() {
        // The turn state belongs to the agent's own row. Two markers for one
        // agent say the state twice.
        let mut tabs = working();
        tabs[0].panes[0].agents[0].turn = Turn::Attention;
        let rows = build_tree(&tabs, &showing("{branch}"));
        assert_eq!(rows[1].indicator, Indicator::Attention);
        assert_eq!(rows[2].indicator, Indicator::None);
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
            &DrawingOptions {
                sections: true,
                ..DrawingOptions::default()
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
                RowContent::Tab { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["editor", "notes"]);
    }

    #[test]
    fn a_session_drawn_twice_is_drawn_the_same_both_times() {
        // The option changes the grouping and nothing else. What differs is the
        // meaning of position. A session is the second child of its tab and the
        // first of its block. Its index says where it is in the list it is drawn
        // in.
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
