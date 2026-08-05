//! A zellij sidebar pane listing the session's tabs and their panes as a tree.
//!
//! The pane is drawn as two regions. The tree fills it; the notification area is
//! pinned to the foot and capped at a quarter of the pane, and an entry is
//! admitted only if it fits whole, so a title never appears over a cut-off
//! message. Both regions are navigated in one order, and every line of a
//! notification entry carries that entry's key, so a click anywhere in it
//! selects the same thing.
//!
//! The tree is resolved from the tabs and panes zellij reports, which arrive as
//! two separate events, so both are held and the tree is rebuilt from whichever
//! arrived. A rebuild that changes nothing drawn asks for no repaint: a plugin
//! reprints its whole pane every time, and pane updates are frequent.

mod model;
mod render;
mod session;
mod tree;

use std::collections::BTreeMap;

use zellij_tile::prelude::*;

use model::{Notification, Row, RowContent, RowKey};
use render::{notification_rows, paint};
use session::Focus;

/// The heading the notification area is drawn under.
const NOTIFICATIONS_HEADING: &str = "notifications";

#[derive(Default)]
struct State {
    tabs: Vec<TabInfo>,
    panes: PaneManifest,
    /// The tree as it was last resolved, which is what the selection and the
    /// paint both read.
    rows: Vec<Row>,
    notifications: Vec<Notification>,
    selected: Option<RowKey>,
    /// The key each screen line was drawn from, so a click resolves against the
    /// frame it landed on rather than against a tree that may since have moved.
    painted: Vec<Option<RowKey>>,
}

register_plugin!(State);

/// Ask zellij where this plugin's client is, which is one synchronous round
/// trip and the only reading the sidebar does outside its subscriptions.
///
/// A focused plugin pane (the sidebar itself, most often) leaves the tab known
/// and no pane focused, so pointing at another tab does not move the gutter off
/// the tab you are in.
fn focus() -> Option<Focus> {
    match get_focused_pane_info() {
        Ok((tab, PaneId::Terminal(pane))) => Some(Focus {
            tab,
            pane: Some(pane),
        }),
        Ok((tab, PaneId::Plugin(_))) => Some(Focus { tab, pane: None }),
        Err(_) => None,
    }
}

impl State {
    /// Rebuild the tree from the tabs and panes last reported, and say whether
    /// what would be drawn changed.
    fn resolve(&mut self) -> bool {
        let rows = tree::build_tree(&session::session(&self.tabs, &self.panes, focus()));
        let changed = rows != self.rows;
        self.rows = rows;
        changed
    }

    /// The selection, falling back to the first row when what it was on has
    /// gone (or when nothing has been selected yet).
    fn selection(&self) -> Option<RowKey> {
        let keys = || self.rows.iter().filter_map(|row| row.key);
        match self.selected {
            Some(selected) if keys().any(|key| key == selected) => Some(selected),
            _ => keys().next(),
        }
    }

    /// The notification area for a pane `width` columns wide, given `cap` lines
    /// to fill. Empty when that leaves no room for the heading and one whole
    /// entry beside it.
    fn notification_area(&self, width: usize, cap: usize) -> Vec<Row> {
        let mut rows = vec![Row::new(RowContent::Header {
            text: NOTIFICATIONS_HEADING.to_string(),
        })];
        for (index, entry) in self.notifications.iter().enumerate() {
            let entry = notification_rows(entry, index, width);
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

    /// Every line of the pane, in the order it is drawn and navigated: the tree,
    /// then enough blank padding to hold the notification area at the foot.
    fn lines(&self, width: usize, height: usize) -> Vec<Row> {
        let area = self.notification_area(width, height / 4);
        let mut rows = self.rows.clone();
        rows.truncate(height.saturating_sub(area.len()));
        rows.resize(
            height.saturating_sub(area.len()),
            Row::new(RowContent::Blank),
        );
        rows.extend(area);
        rows
    }

    /// Go to what the selected row points at.
    ///
    /// A pane brings its tab with it, so selecting a pane of another tab is one
    /// move rather than two. A tab alone lands wherever that tab was left.
    fn activate(&self) {
        match self.selection() {
            Some(RowKey::Pane(id)) => focus_pane_with_id(PaneId::Terminal(id), false, false),
            // Tabs are numbered from one here and from zero everywhere else the
            // sidebar handles them.
            Some(RowKey::Tab(position)) => switch_tab_to(position as u32 + 1),
            Some(RowKey::Notification(_)) | None => {}
        }
    }

    /// Move the selection `step` places through the keys the last frame drew.
    fn step(&mut self, step: isize) {
        let mut keys: Vec<RowKey> = Vec::new();
        for key in self.painted.iter().flatten() {
            if keys.last() != Some(key) {
                keys.push(*key);
            }
        }
        let Some(at) = keys.iter().position(|key| Some(*key) == self.selection()) else {
            self.selected = keys.first().copied();
            return;
        };
        let next = (at as isize + step).clamp(0, keys.len() as isize - 1) as usize;
        self.selected = keys.get(next).copied();
    }
}

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        // Reading the session's tabs and panes, and going to what a row points
        // at: the sidebar asks for nothing it would not use.
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
        ]);
        subscribe(&[
            EventType::Key,
            EventType::Mouse,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::PermissionRequestResult,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                self.tabs = tabs;
                self.resolve()
            }
            Event::PaneUpdate(panes) => {
                self.panes = panes;
                self.resolve()
            }
            // The prompt draws over this pane, so the sidebar is repainted once
            // the answer takes the prompt away.
            Event::PermissionRequestResult(_) => true,
            Event::Key(key) => match key.bare_key {
                BareKey::Down | BareKey::Char('j') => {
                    self.step(1);
                    true
                }
                BareKey::Up | BareKey::Char('k') => {
                    self.step(-1);
                    true
                }
                BareKey::Enter => {
                    self.activate();
                    false
                }
                _ => false,
            },
            // A click goes where it points rather than only selecting: the row
            // under the pointer is the one the user chose.
            Event::Mouse(Mouse::LeftClick(line, _)) => {
                match usize::try_from(line).ok().and_then(|l| self.painted.get(l)) {
                    Some(&Some(key)) => {
                        self.selected = Some(key);
                        self.activate();
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
        self.painted = lines.iter().map(|row| row.key).collect();
        let selected = self.selection();
        let painted: Vec<String> = lines
            .iter()
            .map(|row| paint(row, cols, row.key.is_some() && row.key == selected))
            .collect();
        print!("{}", painted.join("\r\n"));
    }
}
