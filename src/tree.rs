//! Building the sidebar's rows from the session's tabs and their panes.
//!
//! The input is the session as the sidebar cares about it: tabs in the order
//! they are drawn, each with the panes it shows, already filtered and ordered.
//! What is derived here is everything that follows from a thing's *position*:
//! its placement, its branch, and the index it is labelled with.

use crate::model::{Branch, Placement, Row, RowContent, RowKey};

/// A pane, as one row of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub id: u32,
    pub title: String,
    pub focused: bool,
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

/// The rows for one tab: the tab itself, then a child per pane.
fn tab_rows(tab: &Tab) -> Vec<Row> {
    let mut rows = vec![Row::new(RowContent::Window {
        index: (tab.position + 1).to_string(),
        name: tab.name.clone(),
        placement: tab_placement(tab.active),
        color: None,
    })
    .at(RowKey::Tab(tab.position))];

    let last = tab.panes.len().saturating_sub(1);
    for (position, pane) in tab.panes.iter().enumerate() {
        rows.push(
            Row::new(RowContent::Pane {
                index: (position + 1).to_string(),
                title: pane.title.clone(),
                branch: if position == last {
                    Branch::Last
                } else {
                    Branch::More
                },
                placement: pane_placement(tab.active, pane.focused),
                color: None,
            })
            .at(RowKey::Pane(pane.id)),
        );
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

    fn pane(id: u32, title: &str, focused: bool) -> Pane {
        Pane {
            id,
            title: title.to_string(),
            focused,
        }
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
                RowContent::Window { placement, .. } | RowContent::Pane { placement, .. } => {
                    *placement
                }
                other => panic!("unexpected row: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_tab_is_followed_by_its_panes() {
        let rows = build_tree(&session());
        let keys: Vec<Option<RowKey>> = rows.iter().map(|row| row.key).collect();
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
        let indices: Vec<&str> = rows
            .iter()
            .map(|row| match &row.content {
                RowContent::Window { index, .. } | RowContent::Pane { index, .. } => index.as_str(),
                other => panic!("unexpected row: {other:?}"),
            })
            .collect();
        assert_eq!(indices, vec!["1", "1", "2", "2", "1"]);
    }

    #[test]
    fn a_tab_with_no_panes_still_draws_its_own_row() {
        let rows = build_tree(&[tab(0, "empty", false, vec![])]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, Some(RowKey::Tab(0)));
    }
}
