//! A zellij sidebar pane listing the session's tabs and their panes as a tree.
//!
//! The tree is resolved from the tabs and panes zellij reports, which arrive as
//! two separate events, so both are held and the tree is rebuilt from whichever
//! arrived. A rebuild that changes nothing drawn asks for no repaint: a plugin
//! reprints its whole pane every time, and pane updates are frequent.
//!
//! What each line of the last frame pointed at is held until the next one, since
//! a click arrives as a line number and says nothing about what was on it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use zellij_tile::prelude::*;

use agent_wrangler_core::agent::{self, Agent, Turn, AGENTS_MESSAGE};
use agent_wrangler_core::registry::Registry;

use agent_wrangler_ui::frame::{compose, Frame, Note};
use agent_wrangler_ui::model::{NamedColor, Notification, Row, RowKey};
use agent_wrangler_ui::{ansi, selection, tree, Rect};

use zellij_agent_wrangler::agents;
use zellij_agent_wrangler::calls::Answered;
use zellij_agent_wrangler::client::Client;
use zellij_agent_wrangler::options::Options;
use zellij_agent_wrangler::session::{self, Focus};

/// The message the sidebars of one session carry their shared selection in.
///
/// It is broadcast rather than addressed: a message naming this plugin's url
/// reaches no running instance and launches another one instead, while an
/// unaddressed message reaches every plugin there is. Every instance therefore
/// hears every other one, and answers only to the name it knows.
const SELECTION_MESSAGE: &str = "wrangler:selection";

/// The message that turns the sidebar off for the whole session.
const OFF_MESSAGE: &str = "wrangler:off";

/// The message saying the agent hooks have been installed, so that the sidebars
/// asked to install them do it once between them rather than once each.
const INSTALLED_MESSAGE: &str = "wrangler:hooks-installed";

/// The directory the client is run from.
///
/// Zellij runs a plugin's command from the plugin's own working directory,
/// which is the directory the sidebar's pane was opened in. A directory can be
/// removed while the session holding it is still up - a worktree that has been
/// torn down is the ordinary way - and the spawn changes directory before it
/// reaches the program, so every later run fails with the same error a missing
/// client gives, whatever the client's path says. The root is the one directory
/// that cannot go, and none of the calls made here reads a working directory.
const RUN_FROM: &str = "/";

/// The context key naming which call a run was, so that the answer coming back
/// says what failed rather than only that something did.
const CALL: &str = "call";

/// What the sidebar says when it cannot run the client at all. The client is
/// how a sidebar is sent anything, so this is also why the tree is empty.
const UNREACHABLE: &str = "the sidebar could not run its client";

/// What a refused sidebar says beneath its heading.
const REFUSED: &str = "the sidebar cannot read the session";

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
    /// The client, and whether running it has ever worked. Named when the
    /// layout is read and given up on the first time a run fails.
    client: Client,
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
    /// The pane the user was last on in this sidebar's own tab.
    ///
    /// Zellij remembers which pane each tab was left focused on, and going to a
    /// row is done from the sidebar, which by then holds the focus itself. Left
    /// alone, a tab the user visited the sidebar in is a tab they come back to
    /// the sidebar in. This is where they were before they came to it.
    left_behind: Option<u32>,
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

/// A count of lines or columns zellij reports, as the measure a pane is drawn
/// in.
///
/// Saturating rather than truncating: a pane larger than this does not exist,
/// and one drawn as the remainder would be a frame composed for a pane that is
/// not the one being printed into.
fn cells(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

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

/// The first line of what a run put on its error stream, which is the whole of
/// what is worth repeating: the rest is a usage message or a backtrace, and the
/// pane is thirty columns wide.
fn said(stderr: &[u8]) -> &str {
    std::str::from_utf8(stderr)
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
}

/// The right to speak to the other sidebars of this session.
///
/// A sidebar acts on its own behalf in `update`, where zellij tells it what the
/// user did, and it hears the other sidebars in `pipe`. Only the first is given
/// one of these, so a sidebar that answered a broadcast with a broadcast of its
/// own does not compile.
///
/// That rule is worth a type because breaking it is unbounded: two sidebars
/// each speaking because the other spoke have nothing between them that slows
/// down, and every lap of it costs whatever the handlers cost. A sidebar has
/// something to say when the user did something, and never because another
/// sidebar said something.
struct Voice(());

impl Voice {
    /// The right to speak, held by whoever is handling what the user did.
    fn heard_from_the_user() -> Self {
        Voice(())
    }

    /// Say something to every sidebar of this session.
    ///
    /// Side effect: broadcasts. The sender is reached by its own message like
    /// everyone else, and drops it: see [`State::pipe`].
    fn say(&self, message: MessageToPlugin) {
        pipe_message_to_plugin(message);
    }
}

impl State {
    /// Run the client, saying which call this is, if it is a client still worth
    /// running.
    ///
    /// Side effect: spawns a process through zellij. What it came to arrives
    /// back as a `RunCommandResult` naming this call, and is what decides
    /// whether the client is run again at all.
    fn run(&self, call: &str, words: &[&str]) {
        let Some(path) = self.client.path() else {
            return;
        };
        let mut command = vec![path];
        command.extend_from_slice(words);
        run_command_with_env_variables_and_cwd(
            &command,
            BTreeMap::new(),
            PathBuf::from(RUN_FROM),
            BTreeMap::from([(CALL.to_string(), call.to_string())]),
        );
    }

    /// Take in how a run of the client went, and say whether the pane changed.
    ///
    /// Anything but a clean exit is read as the client being unrunnable rather
    /// than as this call going wrong: the sidebar makes the same three calls
    /// over and over, so a call that failed once fails every time, and what is
    /// worth drawing is that the sidebar has stopped trying.
    fn ran(
        &mut self,
        exit: Option<i32>,
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) -> bool {
        match exit {
            Some(0) => self.client.reached(),
            _ => {
                let call = context.get(CALL).map(String::as_str).unwrap_or_default();
                self.client.failed(call, said(stderr))
            }
        }
    }

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
    /// The focus is read rather than heard about. Zellij answers this against
    /// the client asking, not the tab the sidebar is in, so every sidebar of a
    /// client is given that client's focus wherever it sits, and a sidebar has
    /// no use for any other client's: the user of another terminal looking
    /// somewhere else is not this sidebar's user arriving.
    fn resolve_asking(&mut self) -> bool {
        if let Some(fresh) = focus() {
            self.focus = Some(fresh);
            self.left_behind =
                session::left_behind_by(&self.panes, self.plugin_id, fresh, self.left_behind);
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
                self.run("seen", &["seen", agent.session.as_str()]);
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

    /// Ask for this zellij session's agents to be sent here, and say what a call
    /// for the user should be announced with.
    ///
    /// Side effect: runs the client, which is what carries the request. It is
    /// asked once per sidebar, on whichever of the session name and the
    /// permission to run anything arrives second.
    ///
    /// The notifier is named rather than used. Every sidebar of every session
    /// holds the same calls, so a sidebar that raised its own notification would
    /// raise one per sidebar for a call that happened once.
    fn register(&mut self) {
        let Some(name) = self.session_name.clone() else {
            return;
        };
        if self.registered || !self.allowed() {
            return;
        }
        self.registered = true;
        let mut words = vec!["register", "zellij", name.as_str()];
        let notifier = self
            .options
            .desktop
            .as_ref()
            .map(|notifier| notifier.words())
            .unwrap_or_default();
        if !notifier.is_empty() {
            words.push("--notify");
            words.extend(notifier.iter().map(String::as_str));
        }
        self.run("register", &words);
    }

    /// Rebuild the tree from the tabs and panes last reported, and say whether
    /// what would be drawn changed.
    fn resolve(&mut self) -> bool {
        let focus = self.focus;
        let mut resolved = session::session(&self.tabs, &self.panes, focus);
        self.leave_if_alone(&resolved);
        agents::place(&mut resolved, &self.registry);
        let rows = tree::build_tree(&resolved, &self.options.view);
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
    /// Side effect: runs the hook client, which rewrites each agent's config,
    /// and tells the other sidebars it has. They are told rather than left to
    /// work it out, because two of them installing at once would be two
    /// processes writing one file.
    ///
    /// Attempted from `update` rather than from wherever the tree is rebuilt,
    /// because it needs a voice and rebuilding happens on what the other
    /// sidebars say as well as on what the user does.
    fn install_hooks(&mut self, voice: &Voice) {
        if self.options.install_hooks.is_none() {
            return;
        }
        if self.installed || !self.allowed() || !self.is_where_the_user_is() {
            return;
        }
        self.installed = true;
        self.run("install-hooks", &["install-hooks"]);
        voice.say(MessageToPlugin::new(INSTALLED_MESSAGE));
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
        let keys = selection::keys(&self.rows, &self.registry, &self.options.view);
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
                color: NamedColor::of(agent),
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
        let label = agents::label(agent, self.options.view.label);
        match (tab.is_empty(), label.is_empty()) {
            (true, _) => label,
            (false, true) => tab,
            (false, false) => format!("{tab} · {label}"),
        }
    }

    /// Every line of the pane, in the order it is drawn and navigated.
    ///
    /// What the sidebar has to say about itself leads, since that is why the
    /// tree beneath it is missing rows: a hook client of another version, and a
    /// client that could not be run at all. A sidebar zellij has refused says
    /// that and nothing else, because without the permission it has read
    /// nothing to draw.
    fn frame(&self, area: Rect) -> Frame {
        if self.permission == Some(PermissionStatus::Denied) {
            let refused = [Note {
                heading: "no permission",
                text: REFUSED,
            }];
            return compose(&refused, &[], &[], area, &self.options.view);
        }
        // Held rather than named inline: a note borrows its text, and the only
        // one composed here is composed from two pieces.
        let unreachable = self.client.why().map(|why| format!("{UNREACHABLE}. {why}"));
        let mut notes: Vec<Note> = Vec::new();
        if self.mismatched {
            notes.push(Note {
                heading: "out of step",
                text: MISMATCH,
            });
        }
        if let Some(text) = &unreachable {
            notes.push(Note {
                heading: "no client",
                text,
            });
        }
        compose(&notes, &self.rows, &self.notices, area, &self.options.view)
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
            Some(RowKey::Pane(id)) => self.go_to_pane(id),
            Some(RowKey::Tab(position)) => match session::first_pane(&self.panes, position) {
                Some(id) => self.go_to_pane(id),
                // A tab with nothing the sidebar lists is still a tab to go to.
                // Tabs are numbered from one here and from zero everywhere else
                // the sidebar handles them.
                None => {
                    self.stand_down(position);
                    switch_tab_to(position as u32 + 1);
                }
            },
            // Opening an entry goes where the agent is now. Arriving is what
            // answers it, along with every other call raised from that pane.
            Some(RowKey::Agent(session))
            | Some(RowKey::Section(session))
            | Some(RowKey::Notification(session)) => {
                if let Some(id) = self.registry.get(&session).and_then(agents::pane) {
                    self.go_to_pane(id);
                }
            }
            None => {}
        }
    }

    /// Go to a pane, giving this tab back to the user first if the pane is in
    /// another one.
    ///
    /// Side effect: moves the focus, twice when the destination is elsewhere.
    fn go_to_pane(&self, id: u32) {
        if let Some(tab) = session::tab_of_pane(&self.panes, id) {
            self.stand_down(tab);
        }
        focus_pane_with_id(PaneId::Terminal(id), false, false);
    }

    /// Put this tab's focus back where the user left it, on the way out of it.
    ///
    /// Zellij remembers the pane each tab was last focused on, and a row is
    /// opened from the sidebar, which is holding the focus by then. A sidebar
    /// that simply left would make itself the pane its own tab returns to, so
    /// coming back would land on the sidebar rather than on the work.
    ///
    /// Side effect: moves the focus within this tab. Nothing moves when the
    /// destination is this tab, since that leaves nothing behind.
    fn stand_down(&self, going_to: usize) {
        let back = session::stand_down_to(&self.panes, self.plugin_id, self.left_behind, going_to);
        if let Some(id) = back {
            focus_pane_with_id(PaneId::Terminal(id), false, false);
        }
    }

    /// Put the selection where `key` is and tell the other sidebars, so the
    /// sidebars of one session read as one sidebar that follows you.
    fn select(&mut self, key: Option<RowKey>, voice: &Voice) {
        if let Some(key) = &key {
            voice.say(MessageToPlugin::new(SELECTION_MESSAGE).with_payload(key.encode()));
        }
        self.selected = key;
    }

    /// Move the selection `step` places through the keys the last frame drew.
    fn step(&mut self, step: isize, voice: &Voice) {
        let mut keys: Vec<RowKey> = Vec::new();
        for key in self.painted.iter().flatten() {
            if keys.last() != Some(key) {
                keys.push(key.clone());
            }
        }
        let selection = self.selection();
        let Some(at) = keys.iter().position(|key| Some(key) == selection.as_ref()) else {
            self.select(keys.first().cloned(), voice);
            return;
        };
        let next = (at as isize + step).clamp(0, keys.len() as isize - 1) as usize;
        self.select(keys.get(next).cloned(), voice);
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
        let mine = agents::ours(records, self.session_name.as_deref().unwrap_or_default());
        let mut changed = self.registry.adopt(&mine);
        changed |= self.suppress();
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
        self.client = Client::new(self.options.client());
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
            // How a run of the client answers. Without it a client that cannot
            // be run fails silently, once per state change, for as long as the
            // session lasts.
            EventType::RunCommandResult,
        ]);
    }

    fn update(&mut self, event: Event) -> bool {
        // What zellij reports here is what the user did, which is the one thing
        // a sidebar is entitled to speak about.
        let voice = Voice::heard_from_the_user();
        let drawn = match event {
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
            Event::RunCommandResult(exit, _, stderr, context) => self.ran(exit, &stderr, &context),
            Event::PermissionRequestResult(status) => {
                self.permission = Some(status);
                // Asking before the answer arrives runs nothing: a plugin
                // without permission to run a command has its runs dropped.
                self.register();
                true
            }
            Event::Key(key) => match key.bare_key {
                BareKey::Down | BareKey::Char('j') => {
                    self.step(1, &voice);
                    true
                }
                BareKey::Up | BareKey::Char('k') => {
                    self.step(-1, &voice);
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
                    voice.say(MessageToPlugin::new(OFF_MESSAGE));
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
                        self.select(Some(key), &voice);
                        self.activate();
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        // Whether the hooks can be installed yet turns on the permission and on
        // where the user is, so it is asked again on every event until one of
        // them says yes. It is asked once and answered once: see `installed`.
        self.install_hooks(&voice);
        drawn
    }

    /// Take in what another sidebar said, or what the agents were described as.
    ///
    /// Every broadcast reaches its own sender, so anything a sidebar says to the
    /// others is ignored when it comes back. What describes the agents comes
    /// from the command line and is never this plugin's own.
    ///
    /// Nothing here has a [`Voice`], which is the point: everything reached from
    /// here is something another sidebar said, and a sidebar that spoke because
    /// another sidebar spoke would never stop.
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
        let pane = Rect::new(0, 0, cells(cols), cells(rows));
        let frame = self.frame(pane);
        self.painted = frame.keys();
        // The bar says where a keystroke would land, so a sidebar the keys are
        // not reaching draws none: the gutter is what says where you are, and
        // it keeps saying so from every tab.
        let selected = self.focused.then(|| self.selection()).flatten();
        print!("{}", ansi::pane(&frame, selected.as_ref()));
    }
}
