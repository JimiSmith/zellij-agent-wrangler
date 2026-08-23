//! The selection, resolved from the exact rows that a frame holds.

use crate::model::{Row, RowKey};

/// Every distinct selectable key, in the order it appears on the screen.
///
/// The wrapped rows of one notification entry repeat the key of that entry. A
/// user can click each row on its own, but the rows are one item for keyboard
/// navigation.
pub fn selectable_row_keys(rows: &[Row]) -> Vec<RowKey> {
    let mut keys = Vec::new();
    for key in rows.iter().filter_map(|row| row.key.as_ref()) {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    keys
}

/// A held key stays selected while it is visible. If the held item is not in
/// this frame, the selection falls back to the first visible item.
pub fn resolve_selection(keys: &[RowKey], held: Option<&RowKey>) -> Option<RowKey> {
    match held {
        Some(held) if keys.contains(held) => Some(held.clone()),
        _ => keys.first().cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RowContent, TabId};
    use agent_wrangler_core::agent::SessionId;

    fn tab(id: &str) -> RowKey {
        RowKey::Tab(TabId::new(id))
    }

    #[test]
    fn keys_include_only_visible_rows_and_deduplicate_wrapped_entries() {
        let notification = RowKey::Notification(SessionId::new("one").unwrap());
        let rows = vec![
            Row::new(RowContent::Blank).with_key(tab("editor")),
            Row::new(RowContent::Blank),
            Row::new(RowContent::Blank).with_key(notification.clone()),
            Row::new(RowContent::Blank).with_key(notification.clone()),
        ];
        assert_eq!(
            selectable_row_keys(&rows),
            vec![tab("editor"), notification]
        );
    }

    #[test]
    fn a_missing_selection_falls_back_to_the_first_visible_item() {
        let keys = vec![tab("editor")];
        assert_eq!(
            resolve_selection(&keys, Some(&RowKey::Pane(9.into()))),
            Some(tab("editor"))
        );
        assert_eq!(resolve_selection(&keys, None), Some(tab("editor")));
        assert_eq!(resolve_selection(&[], Some(&tab("editor"))), None);
    }
}
