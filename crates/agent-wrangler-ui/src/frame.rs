//! Every row of the pane, in the order it is drawn and navigated.
//!
//! The pane is two regions. The tree fills the pane. The notification area is
//! pinned to the foot and capped at a share of the pane. An entry that does not
//! fit whole is left out, so a title never appears over a cut-off message. Both
//! regions are navigated in one order. Every row of a notification entry carries
//! the key of that entry, so a click anywhere in the entry selects the same
//! thing.
//!
//! The number of rows an entry takes depends on the width. The whole frame is
//! therefore composed together every time the client draws it. Neither region
//! knows what the other needs until there is a pane to divide.

use ratatui_core::layout::Rect;

use crate::model::{Indicator, Notification, Row, RowContent, RowKey};
use crate::options::DrawingOptions;
use crate::render::{notification_body_field, wrap};

/// The heading the notification area is drawn under.
const NOTIFICATIONS_HEADING: &str = "notifications";

/// The share of the pane the calls at the foot can take.
const NOTIFICATION_SHARE: usize = 4;

/// Something a client has to say about itself, where every other row says
/// something about the session.
///
/// A client is the only thing that knows it is broken, and this is how it says
/// so. What the client failed to do leads the pane, because that failure is why
/// the tree under it lacks rows.
pub struct ClientProblem<'a> {
    pub heading: &'a str,
    pub text: &'a str,
}

/// A heading over a wrapped message.
fn heading_over_message(heading: &str, text: &str, width: usize) -> Vec<Row> {
    let mut rows = vec![Row::new(RowContent::Header {
        text: heading.to_string(),
    })];
    for line in wrap(text, notification_body_field(width)) {
        rows.push(Row::new(RowContent::NotificationBody { text: line }));
    }
    rows
}

/// The rows a notification entry is drawn as: the title over the wrapped rows
/// of the message.
///
/// Every row carries the entry's own key, so a click anywhere in the entry lands
/// on the same thing.
fn notification_rows(entry: &Notification, width: usize) -> Vec<Row> {
    let key = RowKey::Notification(entry.session.clone());
    let mut rows = vec![Row::new(RowContent::NotificationTitle {
        title: entry.agent_program.clone(),
        color: entry.color,
    })
    .with_indicator(Indicator::Attention)
    .with_key(key.clone())];
    for line in wrap(&entry.message, notification_body_field(width)) {
        rows.push(Row::new(RowContent::NotificationBody { text: line }).with_key(key.clone()));
    }
    rows
}

/// The notification area for a pane `width` columns wide, with `cap` rows to
/// fill.
///
/// The area is empty in two cases:
///
/// - `cap` leaves no room for the heading and one whole entry beside it,
/// - the client is asked not to list the calls at all.
fn notification_area(
    notices: &[Notification],
    width: usize,
    cap: usize,
    options: &DrawingOptions,
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

/// The rows of one pane, ready to be drawn, and the pane they were composed for.
///
/// The two travel together, because neither one says anything without the other.
/// The height decides the number of rows, and the width decides where a row is
/// cut.
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
}

/// The pane, divided between what the client has to say about itself, the tree,
/// and the calls at the foot.
///
/// What the client says about itself leads, the tree follows, and the calls are
/// pinned to the foot. Blank rows fill whatever is left between the tree and the
/// calls.
pub fn build_frame(
    problems: &[ClientProblem<'_>],
    tree: &[Row],
    notices: &[Notification],
    area: Rect,
    options: &DrawingOptions,
) -> Frame {
    let width = area.width as usize;
    let height = area.height as usize;
    let calls = notification_area(notices, width, height / NOTIFICATION_SHARE, options);
    let mut lines: Vec<Row> = problems
        .iter()
        .flat_map(|problem| heading_over_message(problem.heading, problem.text, width))
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

    use crate::model::{Placement, RowContent, TabId};

    fn call(id: &str, message: &str) -> Notification {
        Notification {
            session: SessionId::new(id).unwrap(),
            agent_program: "claude".to_string(),
            color: None,
            message: message.to_string(),
        }
    }

    fn tree() -> Vec<Row> {
        vec![Row::new(RowContent::Tab {
            index: "1".to_string(),
            name: "editor".to_string(),
            placement: Placement::FocusedPane,
            color: None,
        })
        .with_key(RowKey::Tab(TabId::new("editor")))]
    }

    /// A pane 24 columns wide, which is about what a sidebar is given.
    fn pane(height: u16) -> Rect {
        Rect::new(0, 0, 24, height)
    }

    fn compose_into(height: u16, notices: &[Notification]) -> Frame {
        build_frame(
            &[],
            &tree(),
            notices,
            pane(height),
            &DrawingOptions::default(),
        )
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
        let keys: Vec<Option<RowKey>> = frame.lines().iter().map(|row| row.key.clone()).collect();
        assert_eq!(
            keys.iter().filter(|key| **key == entry("one")).count(),
            3,
            "a title and the two lines it wrapped to"
        );
    }

    #[test]
    fn an_entry_too_tall_for_the_area_is_left_out_rather_than_cut() {
        // A title over a cut-off message says that an agent calls, and then it
        // stops short of where the call comes from. That is worse than no entry
        // at all.
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
        let quiet = DrawingOptions {
            notifications: false,
            ..DrawingOptions::default()
        };
        let frame = build_frame(&[], &tree(), &[call("one", "editor")], pane(12), &quiet);
        assert!(frame
            .lines()
            .iter()
            .all(|row| !matches!(row.content, RowContent::NotificationTitle { .. })));
    }

    #[test]
    fn what_a_client_says_about_itself_leads_the_pane() {
        let note = ClientProblem {
            heading: "no client",
            text: "the sidebar could not run its client",
        };
        let frame = build_frame(&[note], &tree(), &[], pane(12), &DrawingOptions::default());
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
        let frame = build_frame(
            &[],
            &tall,
            &[call("one", "editor")],
            pane(12),
            &DrawingOptions::default(),
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
