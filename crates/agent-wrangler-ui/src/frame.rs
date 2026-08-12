//! Every line of the pane, in the order it is drawn and navigated.
//!
//! The pane is two regions. The tree fills it; the notification area is pinned
//! to the foot and capped at a share of the pane, and an entry is admitted only
//! if it fits whole, so a title never appears over a cut-off message. Both
//! regions are navigated in one order, and every line of a notification entry
//! carries that entry's key, so a click anywhere in it selects the same thing.
//!
//! How many lines an entry takes depends on the width, which is why this is
//! composed at paint time rather than held: the tree above it is resolved when
//! the session changes, and neither knows what the other needs until there is a
//! pane to divide.

use ratatui_core::layout::Rect;

use crate::model::{Indicator, Notification, Row, RowContent, RowKey};
use crate::options::View;
use crate::render::{notification_body_field, wrap};

/// The heading the notification area is drawn under.
const NOTIFICATIONS_HEADING: &str = "notifications";

/// The share of the pane the calls at the foot may take.
const NOTIFICATION_SHARE: usize = 4;

/// Something a client has to say about itself, where every other row says
/// something about the session.
///
/// A client is the only thing that knows it is broken, so this is how it says
/// so: what it could not do leads the pane, since that is why the tree beneath
/// it is missing rows.
pub struct Note<'a> {
    pub heading: &'a str,
    pub text: &'a str,
}

/// A heading over a wrapped message.
fn notice(heading: &str, text: &str, width: usize) -> Vec<Row> {
    let mut rows = vec![Row::new(RowContent::Header {
        text: heading.to_string(),
    })];
    for line in wrap(text, notification_body_field(width)) {
        rows.push(Row::new(RowContent::NotificationBody { text: line }));
    }
    rows
}

/// The rows a notification entry is drawn as: its title over the wrapped lines
/// of its message.
///
/// Every line carries the entry's own key, so a click anywhere in it lands on
/// the same thing.
fn notification_rows(entry: &Notification, width: usize) -> Vec<Row> {
    let key = RowKey::Notification(entry.session.clone());
    let mut rows = vec![Row::new(RowContent::NotificationTitle {
        title: entry.agent.clone(),
        color: entry.color,
    })
    .with(Indicator::Attention)
    .at(key.clone())];
    for line in wrap(&entry.message, notification_body_field(width)) {
        rows.push(Row::new(RowContent::NotificationBody { text: line }).at(key.clone()));
    }
    rows
}

/// The notification area for a pane `width` columns wide, given `cap` lines to
/// fill. Empty when that leaves no room for the heading and one whole entry
/// beside it, and when the client was asked not to list the calls at all.
fn notification_area(
    notices: &[Notification],
    width: usize,
    cap: usize,
    options: &View,
) -> Vec<Row> {
    if !options.notifications {
        return Vec::new();
    }
    let mut rows = vec![Row::new(RowContent::Header {
        text: NOTIFICATIONS_HEADING.to_string(),
    })];
    for entry in notices {
        let entry = notification_rows(entry, width);
        if rows.len() + entry.len() > cap {
            break;
        }
        rows.extend(entry);
    }
    if rows.len() < 2 {
        return Vec::new();
    }
    rows
}

/// The lines of one pane, ready to be drawn, and the pane they were composed
/// for.
///
/// The two travel together because neither says anything without the other: how
/// many lines there are was decided by the height, and where a line is cut by
/// the width.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    lines: Vec<Row>,
    area: Rect,
}

impl Frame {
    pub fn lines(&self) -> &[Row] {
        &self.lines
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    /// What each line points at, in screen order.
    ///
    /// A client holds this from one frame to the next, so a click resolves
    /// against the frame it landed on rather than against a tree that may since
    /// have moved.
    pub fn keys(&self) -> Vec<Option<RowKey>> {
        self.lines.iter().map(|row| row.key.clone()).collect()
    }
}

/// Divide a pane between what the client has to say about itself, the tree, and
/// the calls at the foot.
///
/// The notes lead, the tree follows, and the calls are pinned to the foot with
/// blank rows filling whatever is left between them.
pub fn compose(
    notes: &[Note<'_>],
    tree: &[Row],
    notices: &[Notification],
    area: Rect,
    options: &View,
) -> Frame {
    let width = area.width as usize;
    let height = area.height as usize;
    let calls = notification_area(notices, width, height / NOTIFICATION_SHARE, options);
    let mut lines: Vec<Row> = notes
        .iter()
        .flat_map(|note| notice(note.heading, note.text, width))
        .collect();
    lines.extend(tree.iter().cloned());
    let room = height.saturating_sub(calls.len());
    lines.truncate(room);
    lines.resize(room, Row::new(RowContent::Blank));
    lines.extend(calls);
    Frame { lines, area }
}

#[cfg(test)]
mod tests {
    use super::*;

    use agent_wrangler_core::agent::SessionId;

    use crate::model::{Placement, RowContent};

    fn call(id: &str, message: &str) -> Notification {
        Notification {
            session: SessionId::new(id).unwrap(),
            agent: "claude".to_string(),
            color: None,
            message: message.to_string(),
        }
    }

    fn tree() -> Vec<Row> {
        vec![Row::new(RowContent::Window {
            index: "1".to_string(),
            name: "editor".to_string(),
            placement: Placement::Here,
            color: None,
        })
        .at(RowKey::Tab(0))]
    }

    /// A pane 24 columns wide, which is about what a sidebar is given.
    fn pane(height: u16) -> Rect {
        Rect::new(0, 0, 24, height)
    }

    fn compose_into(height: u16, notices: &[Notification]) -> Frame {
        compose(&[], &tree(), notices, pane(height), &View::default())
    }

    #[test]
    fn a_frame_is_exactly_as_tall_as_the_pane() {
        for height in [1u16, 8, 40] {
            let frame = compose_into(height, &[]);
            assert_eq!(frame.lines().len(), height as usize);
            assert_eq!(frame.area(), pane(height));
        }
    }

    #[test]
    fn the_calls_sit_at_the_foot_under_their_heading() {
        let frame = compose_into(12, &[call("one", "editor")]);
        let lines = frame.lines();
        assert_eq!(
            lines[9].content,
            RowContent::Header {
                text: NOTIFICATIONS_HEADING.to_string()
            }
        );
        assert_eq!(
            lines[10].content,
            RowContent::NotificationTitle {
                title: "claude".to_string(),
                color: None
            }
        );
        assert_eq!(
            lines[11].content,
            RowContent::NotificationBody {
                text: "editor".to_string()
            }
        );
        // The tree keeps the top, and the gap between them is blank.
        assert_eq!(lines[0], tree()[0]);
        assert!(lines[1..9]
            .iter()
            .all(|row| row.content == RowContent::Blank));
    }

    /// The entries listed at the foot, by the session each points at.
    fn listed(frame: &Frame) -> Vec<Option<RowKey>> {
        frame
            .lines()
            .iter()
            .filter(|row| matches!(row.content, RowContent::NotificationTitle { .. }))
            .map(|row| row.key.clone())
            .collect()
    }

    fn entry(id: &str) -> Option<RowKey> {
        Some(RowKey::Notification(SessionId::new(id).unwrap()))
    }

    #[test]
    fn every_line_of_an_entry_points_at_the_same_thing() {
        let frame = compose_into(24, &[call("one", "a message long enough to wrap")]);
        let keys = frame.keys();
        assert_eq!(
            keys.iter().filter(|key| **key == entry("one")).count(),
            3,
            "a title and the two lines it wrapped to"
        );
    }

    #[test]
    fn an_entry_too_tall_for_the_area_is_left_out_rather_than_cut() {
        // A title over a cut-off message says an agent is calling and then stops
        // saying where from, which is worse than not listing it at all.
        let frame = compose_into(24, &[call("one", &"where it is ".repeat(12))]);
        assert!(listed(&frame).is_empty());
    }

    #[test]
    fn the_calls_stop_at_the_last_one_that_fits() {
        let frame = compose_into(12, &[call("one", "editor"), call("two", "notes")]);
        assert_eq!(listed(&frame), vec![entry("one")]);
    }

    #[test]
    fn a_pane_too_short_for_a_whole_entry_lists_none() {
        let frame = compose_into(4, &[call("one", "editor")]);
        assert!(frame
            .lines()
            .iter()
            .all(|row| !matches!(row.content, RowContent::NotificationTitle { .. })));
    }

    #[test]
    fn a_client_told_not_to_list_the_calls_lists_none() {
        let quiet = View {
            notifications: false,
            ..View::default()
        };
        let frame = compose(&[], &tree(), &[call("one", "editor")], pane(12), &quiet);
        assert!(frame
            .lines()
            .iter()
            .all(|row| !matches!(row.content, RowContent::NotificationTitle { .. })));
    }

    #[test]
    fn what_a_client_says_about_itself_leads_the_pane() {
        let note = Note {
            heading: "no client",
            text: "the sidebar could not run its client",
        };
        let frame = compose(&[note], &tree(), &[], pane(12), &View::default());
        assert_eq!(
            frame.lines()[0].content,
            RowContent::Header {
                text: "no client".to_string()
            }
        );
        assert!(frame.lines()[1..]
            .iter()
            .any(|row| matches!(row.content, RowContent::NotificationBody { .. })));
    }

    #[test]
    fn a_tree_taller_than_the_pane_is_cut_rather_than_pushing_the_calls_off() {
        let tall: Vec<Row> = std::iter::repeat_n(tree()[0].clone(), 40).collect();
        let frame = compose(
            &[],
            &tall,
            &[call("one", "editor")],
            pane(12),
            &View::default(),
        );
        assert_eq!(frame.lines().len(), 12);
        assert_eq!(
            frame.lines()[11].content,
            RowContent::NotificationBody {
                text: "editor".to_string()
            }
        );
    }
}
