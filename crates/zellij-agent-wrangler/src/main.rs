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

use agent_wrangler_core::agent::{self, Agent, Turn, AGENTS_MESSAGE};
use agent_wrangler_core::registry::Registry;

use zellij_agent_wrangler::agents;
use zellij_agent_wrangler::calls::{self, Answered};
use zellij_agent_wrangler::model::{Notification, Row, RowContent, RowKey};
use zellij_agent_wrangler::options::Options;
use zellij_agent_wrangler::render::{notification_body_field, notification_rows, paint, wrap};
use zellij_agent_wrangler::selection;
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

/// The message carrying where the user is.
///
/// Only the sidebar whose tab is on screen is sent the events that move the
/// focus; the rest hold whatever it was when their own tab was last looked at,
/// which is by definition the moment before the user left. Where the user is is
/// a fact about the session rather than about a pane, so the one sidebar that
/// can read it tells the others rather than each of them guessing.
const FOCUS_MESSAGE: &str = "wrangler:focus";

/// The message saying the agent hooks have been installed, so that the sidebars
/// asked to install them do it once between them rather than once each.
const INSTALLED_MESSAGE: &str = "wrangler:hooks-installed";

/// What a refused sidebar says beneath its heading.
const REFUSED: &str = "the sidebar cannot read the session";

/// The heading the notification area is drawn under.
const NOTIFICATIONS_HEADING: &str = "notifications";

#[derive(Default)]
struct State {
    /// What the layout asked this sidebar for.
    options: Options,
    tabs: Vec<TabInfo>,
    panes: PaneManifest,
    /// The tree as it was last resolved, which is what the selection and the
    /// paint both read.
    rows: Vec<Row>,
    /// The calls as the foot last listed them.
    ///
    /// They are held beside the tree rather than read at paint time because
    /// they are drawn from the registry rather than from the tree: a tree that
    /// has not moved is no proof that nothing has.
    notices: Vec<Notification>,
    /// The agent sessions running in this zellij session.
    ///
    /// Every sidebar holds the whole set, because what arrives reaches all of
    /// them at once and each sidebar draws every tab.
    registry: Registry,
    /// The calls this sidebar has answered but has not yet been told about, so
    /// that a row already arrived at does not come back for the round trip.
    answered: Answered,
    /// What zellij calls the session this sidebar is in, once it has been told.
    ///
    /// It is the only thing that says which of the agents described to the
    /// sidebar are in front of the user, so until it is known none of them are.
    session_name: Option<String>,
    /// Whether this sidebar has asked to be sent the agents, which is done once
    /// and only once there is a session name to ask on behalf of.
    registered: bool,
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
    /// Whether the agent hooks have been installed by one of the sidebars of
    /// this session, so that the rest leave the config alone.
    installed: bool,
    /// Whether a hook client of another version has reported to this sidebar.
    /// It stays set: the client that reported is installed, and will keep
    /// reporting until it is replaced.
    mismatched: bool,
    /// Whether this sidebar's own pane holds the focus, and so whether a key
    /// pressed now would reach it.
    focused: bool,
}

register_plugin!(State);

/// Ask zellij where this plugin's client is, which is one synchronous round
/// trip and the only reading the sidebar does outside its subscriptions.
fn focus() -> Option<Focus> {
    get_focused_pane_info()
        .ok()
        .map(|(tab, pane)| Focus { tab, pane })
}

/// What the sidebar says when the hook client reporting to it was built against
/// another version of the record format.
const MISMATCH: &str = "the hook client is a different version; install both again";

/// A heading over a wrapped message: how the sidebar says something about
/// itself, where every other row says something about the session.
fn notice(heading: &str, text: &str, width: usize) -> Vec<Row> {
    let mut rows = vec![Row::new(RowContent::Header {
        text: heading.to_string(),
    })];
    for line in wrap(text, notification_body_field(width)) {
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
    ///
    /// A sidebar is only sent events while its own tab is on screen, so this is
    /// the one reading that is worth passing on: every other sidebar is either
    /// hearing nothing or about to be told.
    fn resolve_asking(&mut self) -> bool {
        if let Some(fresh) = focus().filter(|fresh| Some(*fresh) != self.focus) {
            self.focus = Some(fresh);
            pipe_message_to_plugin(
                MessageToPlugin::new(FOCUS_MESSAGE).with_payload(fresh.encode()),
            );
        }
        self.answer();
        self.resolve()
    }

    /// Answer the calls raised from the pane the user is in.
    ///
    /// Arriving at an agent's pane is not an event of its own: it is whichever
    /// change moved the focus, which is why this is read off where the user is
    /// rather than raised anywhere.
    ///
    /// Side effect: runs the client, which is how the answer reaches what the
    /// state is drawn from. Every sidebar puts its own row down, since they all
    /// hold the same agents, but only one of them says so out loud: the same
    /// answer given by each sidebar of a session is the same answer given as
    /// many times.
    fn answer(&mut self) -> bool {
        let Some(pane) = self.focus.and_then(|focus| focus.listed()) else {
            return false;
        };
        let calling: Vec<Agent> = self
            .registry
            .iter()
            .filter(|agent| agents::pane(agent) == Some(pane) && agent.turn == Turn::Attention)
            .cloned()
            .collect();
        let mut changed = false;
        for agent in &calling {
            self.answered.answer(agent);
            changed |= self.registry.seen(&agent.session);
        }
        if self.allowed() && self.is_where_the_user_is() {
            for agent in &calling {
                run_command(
                    &[self.options.client(), "seen", agent.session.as_str()],
                    BTreeMap::new(),
                );
            }
        }
        changed
    }

    /// Put back the calls the user has already turned up to.
    ///
    /// What the sidebar draws is handed to it whole and was written before the
    /// answer got there, so it still says the agent is asking. Taking it at its
    /// word would raise the row again for as long as the round trip takes.
    fn suppress(&mut self) -> bool {
        let mut changed = false;
        for session in self.answered.settled(&self.registry) {
            changed |= self.registry.seen(&session);
        }
        changed
    }

    /// Ask for this zellij session's agents to be sent here.
    ///
    /// Side effect: runs the client, which is what carries the request. It is
    /// asked once per sidebar, on whichever of the session name and the
    /// permission to run anything arrives second.
    fn register(&mut self) {
        let Some(name) = self.session_name.clone() else {
            return;
        };
        if self.registered || !self.allowed() {
            return;
        }
        self.registered = true;
        run_command(
            &[self.options.client(), "register", "zellij", &name],
            BTreeMap::new(),
        );
    }

    /// Rebuild the tree from the tabs and panes last reported, and say whether
    /// what would be drawn changed.
    fn resolve(&mut self) -> bool {
        let focus = self.focus;
        let mut resolved = session::session(&self.tabs, &self.panes, focus);
        self.leave_if_alone(&resolved);
        self.install_hooks();
        agents::place(&mut resolved, &self.registry);
        let rows = tree::build_tree(&resolved, &self.options);
        let notices = self.notifications();
        // Whether the keys are coming here is drawn as well as the rows are, so
        // it is a change to what the pane shows even when no row moved.
        let focused = focus.map(|focus| focus.is_plugin(self.plugin_id)) == Some(true);
        let changed = rows != self.rows || notices != self.notices || focused != self.focused;
        self.rows = rows;
        self.notices = notices;
        self.focused = focused;
        changed
    }

    /// Whether zellij has answered this sidebar's request for what it needs.
    /// Nothing that reaches outside the pane is attempted before it has.
    fn allowed(&self) -> bool {
        self.permission == Some(PermissionStatus::Granted)
    }

    /// Whether this sidebar is the one in the tab the user is in.
    ///
    /// Every sidebar of a session hears every message, so anything that must
    /// happen once needs a rule every sidebar reads the same way and only one
    /// answers to. Being where the user is is that rule: exactly one tab is
    /// active, and the sidebar in it is the one whose idea of the focus is
    /// current.
    fn is_where_the_user_is(&self) -> bool {
        match session::tab_of_plugin(&self.panes, self.plugin_id) {
            Some(mine) => self.focus.map(|focus| focus.tab) == Some(mine),
            None => false,
        }
    }

    /// Install the agent hooks, if the sidebar was asked to and no sidebar of
    /// this session has yet.
    ///
    /// Side effect: runs the hook client, which rewrites each agent's config.
    /// The others are told rather than left to work it out, because two of them
    /// installing at once would be two processes writing one file.
    fn install_hooks(&mut self) {
        let Some(client) = self.options.install_hooks.clone() else {
            return;
        };
        if self.installed || !self.allowed() || !self.is_where_the_user_is() {
            return;
        }
        self.installed = true;
        run_command(&[&client, "install-hooks"], BTreeMap::new());
        pipe_message_to_plugin(MessageToPlugin::new(INSTALLED_MESSAGE));
    }

    /// Raise a desktop notification for a call an agent has just made.
    ///
    /// Side effect: runs the command the options name, with the agent's name
    /// and where it is as its last two arguments.
    ///
    /// It is raised for every call, whichever pane is focused, because a
    /// notification is for the user who is not looking at the terminal at all
    /// and pane focus says nothing about that.
    fn raise(&self, agent: &Agent) {
        let Some(notifier) = &self.options.desktop else {
            return;
        };
        if !self.allowed() || !self.is_where_the_user_is() {
            return;
        }
        let command = notifier.command(&agent.agent, &self.where_it_is(agent));
        let command: Vec<&str> = command.iter().map(String::as_str).collect();
        run_command(&command, BTreeMap::new());
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

    /// The selection: what this sidebar holds, resolved against everything it
    /// can currently point at.
    fn selection(&self) -> Option<RowKey> {
        let keys = selection::keys(&self.rows, &self.registry, &self.options);
        selection::selected(&keys, self.selected.as_ref())
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
                color: agents::color(agent),
                message: self.where_it_is(agent),
            })
            .collect()
    }

    /// Where an agent is, as an entry says it: the tab holding its pane, then
    /// the agent's own label.
    fn where_it_is(&self, agent: &Agent) -> String {
        let tab = agents::pane(agent)
            .and_then(|pane| session::tab_of_pane(&self.panes, pane))
            .and_then(|position| self.tabs.iter().find(|tab| tab.position == position))
            .map(|tab| tab.name.clone())
            .unwrap_or_default();
        let label = agents::label(agent, self.options.label);
        match (tab.is_empty(), label.is_empty()) {
            (true, _) => label,
            (false, true) => tab,
            (false, false) => format!("{tab} · {label}"),
        }
    }

    /// The notification area for a pane `width` columns wide, given `cap` lines
    /// to fill. Empty when that leaves no room for the heading and one whole
    /// entry beside it, and when the sidebar was asked not to list the calls at
    /// all.
    fn notification_area(&self, width: usize, cap: usize) -> Vec<Row> {
        if !self.options.notifications {
            return Vec::new();
        }
        let mut rows = vec![Row::new(RowContent::Header {
            text: NOTIFICATIONS_HEADING.to_string(),
        })];
        for entry in &self.notices {
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

    /// Every line of the pane, in the order it is drawn and navigated: the tree,
    /// then enough blank padding to hold the notification area at the foot.
    ///
    /// A sidebar being reported to by a hook client of another version leads
    /// with saying so, since that is why the tree beneath it is missing rows.
    fn lines(&self, width: usize, height: usize) -> Vec<Row> {
        let area = self.notification_area(width, height / 4);
        let mut rows = match self.mismatched {
            true => notice("out of step", MISMATCH, width),
            false => Vec::new(),
        };
        rows.extend(self.rows.iter().cloned());
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
    /// move rather than two. An agent is wherever its pane is, and only a placed
    /// agent has a row to be selected on.
    ///
    /// A tab goes to the first pane it lists rather than to the tab itself.
    /// Going to a tab lands wherever that tab was last left, which is as often
    /// as not that tab's own sidebar - and arriving at a sidebar is arriving
    /// nowhere. Its first pane is somewhere.
    fn activate(&self) {
        match self.selection() {
            Some(RowKey::Pane(id)) => focus_pane_with_id(PaneId::Terminal(id), false, false),
            Some(RowKey::Tab(position)) => match session::first_pane(&self.panes, position) {
                Some(id) => focus_pane_with_id(PaneId::Terminal(id), false, false),
                // A tab with nothing the sidebar lists is still a tab to go to.
                // Tabs are numbered from one here and from zero everywhere else
                // the sidebar handles them.
                None => switch_tab_to(position as u32 + 1),
            },
            // Opening an entry goes where the agent is now. Arriving is what
            // answers it, along with every other call raised from that pane.
            Some(RowKey::Agent(session))
            | Some(RowKey::Section(session))
            | Some(RowKey::Notification(session)) => {
                if let Some(id) = self.registry.get(&session).and_then(agents::pane) {
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

    /// Take in the agents described to the sidebar, and say whether the tree
    /// changed because of it.
    ///
    /// The whole state arrives every time, so what it leaves out has gone and
    /// what it holds is what there is: a sidebar that missed one is put right by
    /// the next rather than left a record behind. Only this zellij session's
    /// agents are taken in, since the rest are in panes this sidebar has none of.
    fn adopt(&mut self, text: &str) -> bool {
        // Anything that is not a whole statement of the state is not one to act
        // on. An empty message is the shape a truncated or foreign one takes,
        // and acting on it would mean forgetting every agent there is.
        let Some((format, records)) = agent::read_state(text) else {
            return false;
        };
        // A client of another version is worth saying so about: it is why an
        // agent that is plainly running has no row, and no amount of looking at
        // the sidebar would otherwise explain it.
        let mismatch = format != agent::FORMAT && !std::mem::replace(&mut self.mismatched, true);
        if format != agent::FORMAT {
            return mismatch;
        }
        let held = self.registry.clone();
        let mine = agents::ours(records, self.session_name.as_deref().unwrap_or_default());
        let mut changed = self.registry.adopt(&mine);
        changed |= self.suppress();
        // Every call that is new since the last state was read, which is the
        // only thing here that says one has just been raised.
        for agent in calls::raised(&held, &self.registry) {
            self.raise(agent);
        }
        // A call raised by the pane the user is already in is answered in the
        // same pass that took it in, and so never draws.
        changed |= self.answer();
        self.answered.prune(&self.registry);
        // A record only reaches the tree through the pane it names, so a change
        // to the registry is not yet a change to what is drawn.
        let drawn = changed && self.resolve();
        drawn || mismatch
    }

    /// Take the name zellij gives the session this sidebar is in, and ask for
    /// that session's agents.
    ///
    /// Side effect: stops listening for the sessions, and asks for the agents.
    /// The name is settled when the session is made, and being woken for every
    /// change to every session on the machine to be told the same name again is
    /// a great deal of waking for nothing.
    fn name_session(&mut self, sessions: &[SessionInfo]) {
        let Some(current) = sessions.iter().find(|session| session.is_current_session) else {
            return;
        };
        unsubscribe(&[EventType::SessionUpdate]);
        self.session_name = Some(current.name.clone());
        self.register();
    }
}

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.options = Options::read(&configuration);
        self.plugin_id = get_plugin_ids().plugin_id;
        // Reading the session's tabs and panes, and going to what a row points
        // at: the sidebar asks for nothing it would not use. Running a command
        // is how it asks to be sent the agents at all, so it is asked for
        // whatever the layout says: a sidebar that cannot run one has nothing
        // to draw.
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::MessageAndLaunchOtherPlugins,
            PermissionType::RunCommands,
        ]);
        subscribe(&[
            EventType::Key,
            EventType::Mouse,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::CommandChanged,
            EventType::CwdChanged,
            EventType::PermissionRequestResult,
            EventType::SessionUpdate,
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
            // Which session this is is asked for once and then never again, so
            // it is taken from the first report that names it.
            Event::SessionUpdate(sessions, _) => {
                self.name_session(&sessions);
                false
            }
            Event::PermissionRequestResult(status) => {
                self.permission = Some(status);
                // Asking before the answer arrives runs nothing: a plugin
                // without permission to run a command has its runs dropped.
                self.register();
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

    /// Take in what another sidebar said, or what the agents were described as.
    ///
    /// Every broadcast reaches its own sender, so anything a sidebar says to the
    /// others is ignored when it comes back. What describes the agents comes
    /// from the command line and is never this plugin's own.
    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.source == PipeSource::Plugin(self.plugin_id) {
            return false;
        }
        match message.name.as_str() {
            OFF_MESSAGE => {
                close_self();
                false
            }
            INSTALLED_MESSAGE => {
                self.installed = true;
                false
            }
            // Taken as read rather than checked, and not passed on: it was sent
            // by the one sidebar that could see it, which has told everyone.
            FOCUS_MESSAGE => match message.payload.as_deref().and_then(Focus::decode) {
                Some(focus) => {
                    self.focus = Some(focus);
                    self.answer();
                    self.resolve()
                }
                None => false,
            },
            SELECTION_MESSAGE => match message.payload.as_deref().and_then(RowKey::decode) {
                Some(key) => {
                    self.selected = Some(key);
                    true
                }
                None => false,
            },
            AGENTS_MESSAGE => self.adopt(message.payload.as_deref().unwrap_or_default()),
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        if self.permission == Some(PermissionStatus::Denied) {
            let rows: Vec<String> = notice("no permission", REFUSED, cols)
                .iter()
                .map(|row| paint(row, cols, false))
                .collect();
            print!("{}", rows.join("\r\n"));
            self.painted.clear();
            return;
        }

        let lines = self.lines(cols, rows);
        self.painted = lines.iter().map(|row| row.key.clone()).collect();
        // The bar says where a keystroke would land, so a sidebar the keys are
        // not reaching draws none: the gutter is what says where you are, and
        // it keeps saying so from every tab.
        let selected = self.focused.then(|| self.selection()).flatten();
        let painted: Vec<String> = lines
            .iter()
            .map(|row| paint(row, cols, row.key.is_some() && row.key == selected))
            .collect();
        print!("{}", painted.join("\r\n"));
    }
}
