//! What a client can point at, and which of those the selection is on.
//!
//! The two regions of the pane are built at different times: the tree is
//! resolved whenever the session changes, and the notification area is composed
//! at paint time, because how many lines an entry takes depends on the pane's
//! width. What there is to point at does not depend on the width, so it is
//! worked out here for both regions at once.

use crate::model::{Row, RowKey};
use crate::options::View;
use agent_wrangler_core::registry::Registry;

/// Every key a client can put on screen, in the order they are drawn: the
/// tree's, then one for each call listed at the foot.
///
/// A key missing from this list is read as a row that has gone, so the list has
/// to cover both regions: leaving the calls out would make a selected entry
/// look like an entry that had been dismissed.
pub fn keys(rows: &[Row], registry: &Registry, options: &View) -> Vec<RowKey> {
    let mut keys: Vec<RowKey> = rows.iter().filter_map(|row| row.key.clone()).collect();
    if options.notifications {
        keys.extend(
            registry
                .calling()
                .into_iter()
                .map(|agent| RowKey::Notification(agent.session.clone())),
        );
    }
    keys
}

/// The selection: what is held if it is still there to point at, and the first
/// row otherwise.
///
/// Falling back rather than holding on is what keeps the selection on something
/// real when the pane it was on closes, and it is also what puts the selection
/// somewhere before the user has chosen anything.
pub fn selected(keys: &[RowKey], held: Option<&RowKey>) -> Option<RowKey> {
    match held {
        Some(held) if keys.contains(held) => Some(held.clone()),
        _ => keys.first().cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Placement, RowContent};
    use agent_wrangler_core::agent::{Agent, Meta, SessionId, Turn};
    use agent_wrangler_core::origin::Origin;

    fn session(text: &str) -> SessionId {
        SessionId::new(text).unwrap()
    }

    fn tree() -> Vec<Row> {
        vec![
            Row::new(RowContent::Window {
                index: "1".to_string(),
                name: "editor".to_string(),
                placement: Placement::Here,
                color: None,
            })
            .at(RowKey::Tab(0)),
            Row::new(RowContent::Blank),
        ]
    }

    fn calling(id: &str) -> Registry {
        let mut registry = Registry::default();
        registry.report(Agent {
            turn: Turn::Attention,
            raised: 1,
            ..Agent::new(session(id), "claude", Meta::default(), Origin::default())
        });
        registry
    }

    #[test]
    fn the_calls_at_the_foot_are_keys_like_any_other() {
        let keys = keys(&tree(), &calling("one"), &View::default());
        assert_eq!(
            keys,
            vec![RowKey::Tab(0), RowKey::Notification(session("one")),]
        );
    }

    #[test]
    fn a_selected_call_is_still_the_selection() {
        // The entry's row is composed at paint time and is in no tree, so a
        // selection tested against the tree alone falls off it and lands back
        // at the top - and opening it goes to the first tab instead of to the
        // agent.
        let held = RowKey::Notification(session("one"));
        let keys = keys(&tree(), &calling("one"), &View::default());
        assert_eq!(selected(&keys, Some(&held)), Some(held));
    }

    #[test]
    fn a_call_that_is_not_listed_cannot_be_selected() {
        let quiet = View {
            notifications: false,
            ..View::default()
        };
        let keys = keys(&tree(), &calling("one"), &quiet);
        assert_eq!(keys, vec![RowKey::Tab(0)]);
        let held = RowKey::Notification(session("one"));
        assert_eq!(selected(&keys, Some(&held)), Some(RowKey::Tab(0)));
    }

    #[test]
    fn a_selection_on_a_row_that_has_gone_falls_back_to_the_first() {
        let keys = keys(&tree(), &Registry::default(), &View::default());
        assert_eq!(
            selected(&keys, Some(&RowKey::Pane(9))),
            Some(RowKey::Tab(0))
        );
        assert_eq!(selected(&keys, None), Some(RowKey::Tab(0)));
    }

    #[test]
    fn a_client_with_nothing_to_point_at_has_no_selection() {
        assert_eq!(selected(&[], Some(&RowKey::Tab(0))), None);
    }
}
