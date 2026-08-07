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

use zellij_agent_wrangler::agents::{
    self, Agent, Registry, Turn, ATTENTION_MESSAGE, END_MESSAGE, START_MESSAGE, SYNC_MESSAGE,
    SYNC_REQUEST_MESSAGE, WORKING_MESSAGE,
};
use zellij_agent_wrangler::model::{Notification, Row, RowContent, RowKey, SessionId};
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
    /// The agent sessions running in this zellij session.
    ///
    /// Every sidebar holds the whole set, because an agent's hooks report to all
    /// of them at once and each sidebar draws every tab. A sidebar that starts
    /// after an agent did asks the others for what they have.
    registry: Registry,
    selected: Option<RowKey>,
    /// The answer to the permission request, absent until it is given. The
    /// sidebar can read nothing and reach nothing without it.
    permission: Option<PermissionStatus>,
    /// The key each screen line was drawn from, so a click resolves against the
    /// frame it landed on rather than against a tree that may since have moved.
    painted: Vec<Option<RowKey>>,
    /// Where the user was when zellij was last asked.
    ///
    /// Asking is a round trip into the server, which cannot be made while a
    /// message from the command line is being handled: that message is itself
    /// waiting on the server, and the two would wait on each other. So it is
    /// asked on the events that move the focus and remembered for the rest.
    focus: Option<Focus>,
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
    /// Take one pane's title as it is now, and say whether the tree changed.
    ///
    /// The tabs and panes zellij reports are sent when the session's *shape*
    /// changes, and a program renaming its own pane does not change that shape,
    /// so the title held for a pane is whatever it was when one was last opened
    /// or closed. Asking for it is a round trip, which is why it is asked for
    /// one pane at a time and only when that pane is known to have changed.
    fn refresh_title(&mut self, id: u32) -> bool {
        let Some(fresh) = get_pane_info(PaneId::Terminal(id)) else {
            return false;
        };
        for panes in self.panes.panes.values_mut() {
            for pane in panes.iter_mut() {
                if !pane.is_plugin && pane.id == id {
                    pane.title = fresh.title.clone();
                }
            }
        }
        self.resolve_asking()
    }

    /// Ask zellij where the user is, then rebuild. Only safe on an event: see
    /// `focus`.
    fn resolve_asking(&mut self) -> bool {
        self.focus = focus();
        self.resolve()
    }

    /// Rebuild the tree from the tabs and panes last reported, and say whether
    /// what would be drawn changed.
    fn resolve(&mut self) -> bool {
        let focus = self.focus;
        // Being at an agent's pane answers what it was asking for, and it is
        // answered here because arriving is not an event of its own: it shows
        // up as whichever change moved the focus.
        if let Some(pane) = focus.and_then(|focus| focus.pane) {
            self.registry.seen(pane);
        }
        let mut resolved = session::session(&self.tabs, &self.panes, focus);
        self.leave_if_alone(&resolved);
        agents::place(&mut resolved, &self.registry);
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
        let keys = || self.rows.iter().filter_map(|row| row.key.as_ref());
        match &self.selected {
            Some(selected) if keys().any(|key| key == selected) => Some(selected.clone()),
            _ => keys().next().cloned(),
        }
    }

    /// The agents calling for the user, as the area lists them: newest first,
    /// one per session, each described by where it is rather than by what it
    /// said.
    ///
    /// The list is read off the registry every time rather than kept, so an
    /// entry is a live pointer: an agent whose call has been answered, or whose
    /// session has ended, simply stops being listed.
    fn notifications(&self) -> Vec<Notification> {
        self.registry
            .calling()
            .into_iter()
            .map(|agent| Notification {
                session: agent.session.clone(),
                agent: agent.agent.clone(),
                color: None,
                message: self.where_it_is(agent),
            })
            .collect()
    }

    /// Where an agent is, as an entry says it: the tab holding its pane, then
    /// the agent's own label.
    fn where_it_is(&self, agent: &Agent) -> String {
        let tab = agent
            .pane
            .and_then(|pane| session::tab_of_pane(&self.panes, pane))
            .and_then(|position| self.tabs.iter().find(|tab| tab.position == position))
            .map(|tab| tab.name.clone())
            .unwrap_or_default();
        match (tab.is_empty(), agent.label.is_empty()) {
            (true, _) => agent.label.clone(),
            (false, true) => tab,
            (false, false) => format!("{tab} · {}", agent.label),
        }
    }

    /// The notification area for a pane `width` columns wide, given `cap` lines
    /// to fill. Empty when that leaves no room for the heading and one whole
    /// entry beside it.
    fn notification_area(&self, width: usize, cap: usize) -> Vec<Row> {
        let mut rows = vec![Row::new(RowContent::Header {
            text: NOTIFICATIONS_HEADING.to_string(),
        })];
        for entry in self.notifications() {
            let entry = notification_rows(&entry, width);
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
    /// move rather than two. A tab alone lands wherever that tab was left. An
    /// agent is wherever its pane is, and only a placed agent has a row to be
    /// selected on.
    fn activate(&self) {
        match self.selection() {
            Some(RowKey::Pane(id)) => focus_pane_with_id(PaneId::Terminal(id), false, false),
            // Tabs are numbered from one here and from zero everywhere else the
            // sidebar handles them.
            Some(RowKey::Tab(position)) => switch_tab_to(position as u32 + 1),
            // Opening an entry goes where the agent is now. Arriving is what
            // answers it, along with every other call raised from that pane.
            Some(RowKey::Agent(session)) | Some(RowKey::Notification(session)) => {
                if let Some(id) = self.registry.get(&session).and_then(|agent| agent.pane) {
                    focus_pane_with_id(PaneId::Terminal(id), false, false);
                }
            }
            None => {}
        }
    }

    /// Put the selection where `key` is and tell the other sidebars, so the
    /// sidebars of one session read as one sidebar that follows you.
    fn select(&mut self, key: Option<RowKey>) {
        if let Some(key) = &key {
            pipe_message_to_plugin(
                MessageToPlugin::new(SELECTION_MESSAGE).with_payload(key.encode()),
            );
        }
        self.selected = key;
    }

    /// Move the selection `step` places through the keys the last frame drew.
    fn step(&mut self, step: isize) {
        let mut keys: Vec<RowKey> = Vec::new();
        for key in self.painted.iter().flatten() {
            if keys.last() != Some(key) {
                keys.push(key.clone());
            }
        }
        let selection = self.selection();
        let Some(at) = keys.iter().position(|key| Some(key) == selection.as_ref()) else {
            self.select(keys.first().cloned());
            return;
        };
        let next = (at as isize + step).clamp(0, keys.len() as isize - 1) as usize;
        self.select(keys.get(next).cloned());
    }

    /// Take in what an agent's hooks reported, and say whether the tree changed
    /// because of it.
    fn hook(&mut self, message: &PipeMessage) -> bool {
        let payload = message.payload.as_deref().unwrap_or_default();
        let changed = match message.name.as_str() {
            START_MESSAGE => Agent::decode(payload)
                .map(|agent| self.registry.start(agent))
                .unwrap_or(false),
            END_MESSAGE => SessionId::new(payload)
                .map(|session| self.registry.end(&session))
                .unwrap_or(false),
            WORKING_MESSAGE => SessionId::new(payload)
                .map(|session| self.registry.mark(&session, Turn::Working, 0))
                .unwrap_or(false),
            // A call for the user carries the moment it was raised, so that
            // every sidebar lists the calls in the same order.
            ATTENTION_MESSAGE => match payload.split_once('\t') {
                Some((session, at)) => SessionId::new(session)
                    .map(|session| {
                        self.registry
                            .mark(&session, Turn::Attention, at.parse().unwrap_or(0))
                    })
                    .unwrap_or(false),
                None => false,
            },
            _ => false,
        };
        // A record only reaches the tree through the pane it names, so a change
        // to the registry is not yet a change to what is drawn.
        changed && self.resolve()
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
            EventType::CommandChanged,
            EventType::CwdChanged,
            EventType::PermissionRequestResult,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                self.tabs = tabs;
                self.resolve_asking()
            }
            Event::PaneUpdate(panes) => {
                self.panes = panes;
                self.resolve_asking()
            }
            // The two things that reach a pane's title: what is running in it,
            // and where it is running. Zellij watches both for the whole
            // session and reports only the panes that changed, so the title is
            // re-read exactly then and the sidebar keeps no clock of its own.
            Event::CommandChanged(PaneId::Terminal(id), ..)
            | Event::CwdChanged(PaneId::Terminal(id), ..) => self.refresh_title(id),
            Event::PermissionRequestResult(status) => {
                self.permission = Some(status);
                // Asking before the answer arrives sends nothing: a plugin
                // without permission to message others has its sends dropped.
                if status == PermissionStatus::Granted {
                    pipe_message_to_plugin(MessageToPlugin::new(SYNC_REQUEST_MESSAGE));
                }
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
                match usize::try_from(line)
                    .ok()
                    .and_then(|l| self.painted.get(l))
                    .cloned()
                {
                    Some(Some(key)) => {
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

    /// Take in what another sidebar said, or what an agent's hooks reported.
    ///
    /// Every broadcast reaches its own sender, so anything a sidebar says to the
    /// others is ignored when it comes back. Hook messages come from the command
    /// line and are never this plugin's own.
    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.source == PipeSource::Plugin(self.plugin_id) {
            return false;
        }
        match message.name.as_str() {
            OFF_MESSAGE => {
                close_self();
                false
            }
            SELECTION_MESSAGE => match message.payload.as_deref().and_then(RowKey::decode) {
                Some(key) => {
                    self.selected = Some(key);
                    true
                }
                None => false,
            },
            // A sidebar that knows nothing has nothing to answer with, and
            // answering would only tell the asker what it already has.
            SYNC_REQUEST_MESSAGE => {
                if !self.registry.is_empty() {
                    pipe_message_to_plugin(
                        MessageToPlugin::new(SYNC_MESSAGE).with_payload(self.registry.encode()),
                    );
                }
                false
            }
            SYNC_MESSAGE => {
                let payload = message.payload.as_deref().unwrap_or_default();
                self.registry.absorb(payload) && self.resolve()
            }
            START_MESSAGE | END_MESSAGE | WORKING_MESSAGE | ATTENTION_MESSAGE => {
                self.hook(&message)
            }
            _ => false,
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
        self.painted = lines.iter().map(|row| row.key.clone()).collect();
        let selected = self.selection();
        let painted: Vec<String> = lines
            .iter()
            .map(|row| paint(row, cols, row.key.is_some() && row.key == selected))
            .collect();
        print!("{}", painted.join("\r\n"));
    }
}
