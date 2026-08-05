//! A zellij sidebar pane listing tabs, their panes as a tree, and the agent
//! sessions running in them.
//!
//! The rows are fixed: this draws one hardcoded arrangement and lets you move
//! through it, which is enough to settle how the sidebar looks and how it takes
//! input before anything resolves live state.
//!
//! The pane is drawn as two regions. The tree scrolls; the notification area is
//! pinned to the foot and capped at a quarter of the pane, and an entry is
//! admitted only if it fits whole, so a title never appears over a cut-off
//! message. Both regions are navigated in one order, and every line of a
//! notification entry carries that entry's id, so a click anywhere in it selects
//! the same thing.

mod fixture;
mod model;
mod render;

use std::collections::BTreeMap;

use zellij_tile::prelude::*;

use fixture::Notification;
use model::{Row, RowContent};
use render::{notification_rows, paint};

/// The heading the notification area is drawn under.
const NOTIFICATIONS_HEADING: &str = "notifications";

/// One drawn line: the row it paints and the selection id it answers to. A line
/// with no id (a heading, or padding) cannot be selected or clicked.
struct Line {
    row: Row,
    id: Option<usize>,
}

#[derive(Default)]
struct State {
    tree: Vec<Row>,
    notifications: Vec<Notification>,
    selected: usize,
    /// The id each screen line was drawn from, so a click resolves against the
    /// frame it landed on rather than against a tree that may since have moved.
    painted: Vec<Option<usize>>,
}

register_plugin!(State);

impl State {
    /// The notification area for a pane `width` columns wide, given `cap` lines
    /// to fill. Empty when that leaves no room for the heading and one whole
    /// entry beside it.
    fn notification_area(&self, width: usize, cap: usize) -> Vec<Line> {
        let mut lines = vec![Line {
            row: Row::new(RowContent::Header {
                text: NOTIFICATIONS_HEADING.to_string(),
            })
            .inert(),
            id: None,
        }];
        for (index, entry) in self.notifications.iter().enumerate() {
            let rows = notification_rows(entry.agent, entry.color, entry.message, width);
            if lines.len() + rows.len() > cap {
                break;
            }
            let id = self.tree.len() + index;
            lines.extend(rows.into_iter().map(|row| Line { row, id: Some(id) }));
        }
        if lines.len() < 2 {
            return Vec::new();
        }
        lines
    }

    /// Every line of the pane, in the order it is drawn and navigated: the tree,
    /// then enough blank padding to hold the notification area at the foot.
    fn lines(&self, width: usize, height: usize) -> Vec<Line> {
        let area = self.notification_area(width, height / 4);
        let mut lines: Vec<Line> = self
            .tree
            .iter()
            .enumerate()
            .map(|(id, row)| Line {
                row: row.clone(),
                id: Some(id),
            })
            .collect();
        lines.truncate(height.saturating_sub(area.len()));
        while lines.len() + area.len() < height {
            lines.push(Line {
                row: Row::new(RowContent::Blank).inert(),
                id: None,
            });
        }
        lines.extend(area);
        lines
    }

    /// Move the selection `step` places through the ids the last frame drew.
    fn step(&mut self, step: isize) {
        let ids: Vec<usize> = self.painted.iter().flatten().copied().fold(
            Vec::new(),
            |mut ids: Vec<usize>, id: usize| {
                if ids.last() != Some(&id) {
                    ids.push(id);
                }
                ids
            },
        );
        let Some(at) = ids.iter().position(|&id| id == self.selected) else {
            self.selected = ids.first().copied().unwrap_or(0);
            return;
        };
        let next = (at as isize + step).clamp(0, ids.len() as isize - 1) as usize;
        self.selected = ids[next];
    }
}

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        self.tree = fixture::tree();
        self.notifications = fixture::notifications();
        subscribe(&[EventType::Key, EventType::Mouse]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) => match key.bare_key {
                BareKey::Down | BareKey::Char('j') => {
                    self.step(1);
                    true
                }
                BareKey::Up | BareKey::Char('k') => {
                    self.step(-1);
                    true
                }
                _ => false,
            },
            Event::Mouse(Mouse::LeftClick(line, _)) => {
                match usize::try_from(line).ok().and_then(|l| self.painted.get(l)) {
                    Some(&Some(id)) => {
                        self.selected = id;
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let lines = self.lines(cols, rows);
        self.painted = lines.iter().map(|line| line.id).collect();
        let painted: Vec<String> = lines
            .iter()
            .map(|line| paint(&line.row, cols, line.id == Some(self.selected)))
            .collect();
        print!("{}", painted.join("\r\n"));
    }
}
