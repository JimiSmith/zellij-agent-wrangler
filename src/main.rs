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

use std::collections::BTreeMap;

use zellij_tile::prelude::*;

use zellij_agent_wrangler::model::{Notification, Row, RowContent, RowKey};
use zellij_agent_wrangler::render::{notification_body_field, notification_rows, paint, wrap};
use zellij_agent_wrangler::session::{self, Focus};
use zellij_agent_wrangler::tree;

/// The message the sidebars of one session carry their shared selection in.
///
/// It is broadcast rather than addressed: a message naming this plugin's url
/// reaches no running instance and launches another one instead, while an
/// unaddressed message reaches every plugin there is. Every instance therefore
/// hears every other one, and answers only to the name it knows.
const SELECTION_MESSAGE: &str = "wrangler:selection";

/// The message that turns the sidebar off for the whole session.
const OFF_MESSAGE: &str = "wrangler:off";

/// What a refused sidebar says beneath its heading.
const REFUSED: &str = "the sidebar cannot read the session";

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
    /// The answer to the permission request, absent until it is given. The
    /// sidebar can read nothing and reach nothing without it.
    permission: Option<PermissionStatus>,
    /// The key each screen line was drawn from, so a click resolves against the
    /// frame it landed on rather than against a tree that may since have moved.
    painted: Vec<Option<RowKey>>,
    /// This instance's own plugin id, which is how it finds the tab it is in.
    plugin_id: u32,
    /// Whether the tab this sidebar is in has ever held a pane besides it.
    ///
    /// A tab is briefly reported as holding only the sidebar while it is still
    /// being built, and leaving then would close a tab the user is opening.
    /// Waiting for company first makes the rule "leave when the last pane goes"
    /// rather than "leave when there is none yet".
    had_company: bool,
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

/// What the pane says once the sidebar has been refused what it needs. Refusal
/// is an answer, and an empty pane would read as a broken sidebar rather than
/// as one that was turned away.
fn refused_rows(width: usize) -> Vec<Row> {
    let mut rows = vec![Row::new(RowContent::Header {
        text: "no permission".to_string(),
    })];
    for line in wrap(REFUSED, notification_body_field(width)) {
        rows.push(Row::new(RowContent::NotificationBody { text: line }));
    }
    rows
}

impl State {
    /// Rebuild the tree from the tabs and panes last reported, and say whether
    /// what would be drawn changed.
    fn resolve(&mut self) -> bool {
        let resolved = session::session(&self.tabs, &self.panes, focus());
        self.leave_if_alone(&resolved);
        let rows = tree::build_tree(&resolved);
        let changed = rows != self.rows;
        self.rows = rows;
        changed
    }

    /// Close this sidebar when the tab holding it has nothing else left.
    ///
    /// A sidebar alone would take the whole tab and keep an empty one alive, and
    /// closing the last pane of a tab closes the tab, so leaving is what lets
    /// the tab go.
    fn leave_if_alone(&mut self, resolved: &[tree::Tab]) {
        let Some(mine) = session::tab_of_plugin(&self.panes, self.plugin_id) else {
            return;
        };
        let Some(tab) = resolved.iter().find(|tab| tab.position == mine) else {
            return;
        };
        if !tab.panes.is_empty() {
            self.had_company = true;
        } else if self.had_company {
            close_self();
        }
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

    /// Put the selection where `key` is and tell the other sidebars, so the
    /// sidebars of one session read as one sidebar that follows you.
    fn select(&mut self, key: Option<RowKey>) {
        self.selected = key;
        if let Some(key) = key {
            pipe_message_to_plugin(
                MessageToPlugin::new(SELECTION_MESSAGE).with_payload(key.encode()),
            );
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
            self.select(keys.first().copied());
            return;
        };
        let next = (at as isize + step).clamp(0, keys.len() as isize - 1) as usize;
        self.select(keys.get(next).copied());
    }
}

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        self.plugin_id = get_plugin_ids().plugin_id;
        // Reading the session's tabs and panes, and going to what a row points
        // at: the sidebar asks for nothing it would not use.
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::MessageAndLaunchOtherPlugins,
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
            Event::PermissionRequestResult(status) => {
                self.permission = Some(status);
                true
            }
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
                // Off is a fact about the session, not about this pane: the
                // sidebars of a session are one sidebar, and one of them left
                // behind would open the rest again.
                BareKey::Char('q') => {
                    pipe_message_to_plugin(MessageToPlugin::new(OFF_MESSAGE));
                    false
                }
                _ => false,
            },
            // A click goes where it points rather than only selecting: the row
            // under the pointer is the one the user chose.
            Event::Mouse(Mouse::LeftClick(line, _)) => {
                match usize::try_from(line).ok().and_then(|l| self.painted.get(l)) {
                    Some(&Some(key)) => {
                        self.select(Some(key));
                        self.activate();
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Adopt a selection another sidebar made. A sidebar hears its own broadcast
    /// too, and has nothing to learn from it.
    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name == OFF_MESSAGE {
            close_self();
            return false;
        }
        if message.name != SELECTION_MESSAGE || message.source == PipeSource::Plugin(self.plugin_id)
        {
            return false;
        }
        match message.payload.as_deref().and_then(RowKey::decode) {
            Some(key) => {
                self.selected = Some(key);
                true
            }
            None => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        if self.permission == Some(PermissionStatus::Denied) {
            let rows: Vec<String> = refused_rows(cols)
                .iter()
                .map(|row| paint(row, cols, false))
                .collect();
            print!("{}", rows.join("\r\n"));
            self.painted.clear();
            return;
        }

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
