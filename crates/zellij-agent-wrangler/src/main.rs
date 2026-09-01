use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use agent_wrangler_core::agent::AGENTS_MESSAGE;
use agent_wrangler_sidebar::{
    Application, ClientMessage, Effect, Input, Options, PaneId, Permission, Registration,
    UserAction,
};
use agent_wrangler_ui::{ansi, Rect};
use zellij_agent_wrangler::adapter;
use zellij_tile::prelude::*;

/// The directory that the client runs from. This directory cannot disappear.
///
/// Zellij otherwise uses the directory where the sidebar pane opened. A removed
/// worktree can take that directory away while the session remains. Every later
/// spawn then fails before it reaches even an absolute client path. None of
/// these calls reads a working directory, so the root is safe.
const RUN_FROM: &str = "/";
const CALL: &str = "call";

/// The word that names this kind of client to the client program.
///
/// The daemon reaches a zellij sidebar with `zellij pipe`, and this word asks
/// the daemon to do that. The client program accepts a set of these words, and
/// it refuses a word outside that set with an exit status and nothing drawn.
const REGISTER_KIND: &str = "zellij";

/// The wait of a stale frame before the draw, in seconds.
///
/// Zero is not immediate. The timer comes back behind the events in the queue,
/// and that queue is the wait that this code wants. A measurement at ten tabs
/// shows this behavior. With nothing else in progress, the timer arrives 1ms
/// after the request. During a tab switch, the timer arrives 22ms after the
/// request. By that time the application reduced the visibility change and the
/// pane and tab reports of the switch. One frame can therefore show the end of
/// the switch. A fixed 15ms wait was measured beside zero. It coalesced no
/// better, because the queue was the longer of the two. It only moved every
/// frame 15ms later.
const SETTLE: f64 = 0.0;

#[derive(Default)]
struct Plugin {
    application: Application,
    plugin_id: u32,
    schedule: adapter::RenderSchedule,
    /// The CLI pipe that the daemon holds open to this session. The id comes
    /// from the last message that arrived on that pipe.
    ///
    /// The id is how this plugin answers on that pipe, and it arrives only on a
    /// message. Nothing this plugin says to the daemon can therefore be said
    /// before the daemon has said something first.
    ///
    /// Each pipe process carries an id of its own. When the daemon replaces a
    /// pipe that died, this holds the old id until a message on the new one
    /// arrives.
    pipe_id: Option<String>,
    /// What this sidebar owes the daemon, oldest first.
    ///
    /// Nothing is written the moment it is raised. A plugin writes on a pipe
    /// only while it handles a message from that pipe, and the id that this
    /// plugin holds between messages can already belong to a pipe that the
    /// daemon replaced. Every line therefore waits for the next message, where
    /// the id is fresh because the message carried it.
    ///
    /// A line that is already here is not added again, so a daemon that stopped
    /// sending leaves a list that is as long as the number of sessions and no
    /// longer.
    owed: Vec<ClientMessage>,
}

register_plugin!(Plugin);

fn cells(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

impl Plugin {
    fn reduce(&mut self, input: Input, settle: bool) {
        let mut repaint = false;
        let mut effects = VecDeque::new();
        effects.extend(self.application.reduce(input).effects);
        self.drain(&mut repaint, &mut effects);
        if settle {
            effects.extend(self.application.reduce(Input::EventSettled).effects);
            self.drain(&mut repaint, &mut effects);
        }
        // This code arranges the draw and does not perform it. A change
        // arrives as several events. The timer draws the frame after the last
        // event instead of after each event.
        if repaint && self.schedule.invalidate() {
            set_timeout(SETTLE);
        }
    }

    fn drain(&mut self, repaint: &mut bool, effects: &mut VecDeque<Effect>) {
        while let Some(effect) = effects.pop_front() {
            if effect == Effect::Repaint {
                *repaint = true;
                continue;
            }
            if let Some(observation) = self.execute(effect) {
                effects.extend(self.application.reduce(observation).effects);
            }
        }
    }

    /// Writes everything that this sidebar owes the daemon, and one heartbeat.
    ///
    /// Side effect: this method writes on the pipe that the daemon holds open.
    /// Call it only while this plugin handles a message from that pipe. Zellij
    /// holds a line written at any other moment and hands it over on the next
    /// message, and the daemon can have replaced the pipe by then.
    ///
    /// The heartbeat says that this sidebar can still send a message. That is
    /// what keeps the session a client. A session whose last sidebar closed
    /// answers nothing, and the daemon gives up on it.
    ///
    /// Every sidebar of a session answers on the same pipe. The daemon holds one
    /// clock for the session and does not ask which sidebar spoke, so no
    /// sidebar has to be chosen to speak for the rest.
    fn answer_on_pipe(&mut self) {
        let Some(id) = self.pipe_id.clone() else {
            return;
        };
        for told in self.owed.drain(..).chain([ClientMessage::Beat]) {
            cli_pipe_output(&id, &format!("{}\n", told.encode()));
        }
    }

    fn execute(&mut self, effect: Effect) -> Option<Input> {
        match effect {
            Effect::Repaint => None,
            Effect::RefreshFocus => {
                Some(Input::FocusObserved(get_focused_pane_info().ok().map(
                    |(tab_id, pane)| adapter::focus(tab_id, pane, self.plugin_id),
                )))
            }
            Effect::RefreshPaneTitle(pane) => {
                let title = adapter::numeric_pane(&pane)
                    .and_then(|id| get_pane_info(zellij_tile::prelude::PaneId::Terminal(id)))
                    .map(|pane| pane.title);
                Some(Input::PaneTitleObserved { pane, title })
            }
            Effect::Run(command) => {
                let mut words = vec![command.program];
                words.extend(command.args);
                let words: Vec<&str> = words.iter().map(String::as_str).collect();
                run_command_with_env_variables_and_cwd(
                    &words,
                    BTreeMap::new(),
                    PathBuf::from(RUN_FROM),
                    BTreeMap::from([(CALL.to_string(), command.call)]),
                );
                None
            }
            Effect::Tell(told) => {
                // Nothing goes out here. A focus change usually raises this
                // effect, and a focus change is not a pipe message, so this
                // plugin has no turn to write in. The line waits for the next
                // message from the daemon. The daemon writes down every held
                // pipe on a beat for exactly that reason, and quickens that beat
                // while an agent waits for the user.
                if !self.owed.contains(&told) {
                    self.owed.push(told);
                }
                None
            }
            Effect::Broadcast(message) => {
                let (name, payload) = adapter::encode_message(message);
                let mut message = MessageToPlugin::new(name);
                if let Some(payload) = payload {
                    message = message.with_payload(payload);
                }
                pipe_message_to_plugin(message);
                None
            }
            Effect::FocusPane(pane) => {
                if let Some(id) = adapter::numeric_pane(&pane) {
                    focus_pane_with_id(zellij_tile::prelude::PaneId::Terminal(id), false, false);
                }
                None
            }
            Effect::SwitchTab(tab) => {
                if let Some(position) = self.application.tab_position(&tab) {
                    switch_tab_to(position.one_based() as u32);
                }
                None
            }
            Effect::StopSessionDiscovery => {
                unsubscribe(&[EventType::SessionUpdate]);
                None
            }
            Effect::CloseSidebar => {
                close_self();
                None
            }
        }
    }

    /// What one host event says, as the inputs that carry it.
    ///
    /// A host event carries more than one fact in one case. A session update
    /// names the session and it names how the daemon reaches this sidebar, and
    /// those are two inputs.
    fn update_input(&self, event: Event) -> Vec<Input> {
        match event {
            Event::Visible(visible) => vec![Input::VisibilityChanged(visible)],
            Event::TabUpdate(tabs) => vec![Input::TabsReported(adapter::tabs(tabs))],
            Event::PaneUpdate(panes) => vec![Input::LayoutReported(adapter::layout(
                panes,
                self.plugin_id,
            ))],
            Event::CommandChanged(zellij_tile::prelude::PaneId::Terminal(id), ..)
            | Event::CwdChanged(zellij_tile::prelude::PaneId::Terminal(id), ..) => {
                vec![Input::PaneChanged(PaneId::new(id.to_string()))]
            }
            Event::SessionUpdate(sessions, _) => sessions
                .into_iter()
                .find(|session| session.is_current_session)
                .map(|session| {
                    // The daemon reaches a zellij sidebar through the session,
                    // so the name is both what the pane draws and how this
                    // client is registered.
                    vec![
                        Input::SessionNamed(session.name.clone()),
                        Input::RegistrationReported(Registration {
                            kind: REGISTER_KIND.to_string(),
                            id: session.name,
                        }),
                    ]
                })
                .unwrap_or_default(),
            Event::RunCommandResult(exit, _, stderr, context) => vec![Input::CommandFinished {
                exit,
                stderr,
                call: context.get(CALL).cloned().unwrap_or_default(),
            }],
            Event::PermissionRequestResult(status) => {
                vec![Input::PermissionReported(match status {
                    PermissionStatus::Granted => Permission::Granted,
                    PermissionStatus::Denied => Permission::Denied,
                })]
            }
            Event::Key(key) => match key.bare_key {
                BareKey::Down | BareKey::Char('j') => vec![Input::User(UserAction::Next)],
                BareKey::Up | BareKey::Char('k') => vec![Input::User(UserAction::Previous)],
                BareKey::Enter => vec![Input::User(UserAction::Activate)],
                BareKey::Char(' ') => vec![Input::User(UserAction::OpenOrClosePreview)],
                BareKey::Char('q') => vec![Input::User(UserAction::Quit)],
                _ => Vec::new(),
            },
            Event::Mouse(Mouse::LeftClick(line, _)) => usize::try_from(line)
                .ok()
                .map(|line| Input::User(UserAction::Click(line)))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl ZellijPlugin for Plugin {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.application = Application::new(Options::from_configuration(&configuration));
        self.plugin_id = get_plugin_ids().plugin_id;
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::MessageAndLaunchOtherPlugins,
            PermissionType::RunCommands,
            // What `cli_pipe_output` needs. Without it the call is dropped in
            // silence: no error, no log, and nothing on the pane.
            PermissionType::ReadCliPipes,
        ]);
        subscribe(&[
            EventType::Key,
            EventType::Mouse,
            EventType::Timer,
            EventType::Visible,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::CommandChanged,
            EventType::CwdChanged,
            EventType::PermissionRequestResult,
            EventType::SessionUpdate,
            EventType::RunCommandResult,
        ]);
        // `Visible` reports later changes. Zellij does not always emit an
        // initial `Visible(true)` for a new plugin pane. This code seeds that
        // fact now. The first tab and pane reports give the focus.
        self.application.reduce(Input::VisibilityChanged(true));
    }

    fn update(&mut self, event: Event) -> bool {
        // The timer carries no facts. It marks the moment for the draw of a
        // frame that the earlier events owe. It is the only event that draws a
        // frame.
        if let Event::Timer(_) = event {
            return self.schedule.due();
        }
        let inputs = self.update_input(event);
        match inputs.split_last() {
            // The settle runs once, after the last input of the event. An
            // event that carries two facts is still one event.
            Some((last, rest)) => {
                for input in rest {
                    self.reduce(input.clone(), false);
                }
                self.reduce(last.clone(), true);
            }
            None => self.reduce(Input::EventSettled, false),
        }
        false
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        // The id is taken before anything else acts on the message, so an
        // effect that this message raises finds the pipe already open. A fresh
        // pipe process carries a fresh id, so the newest one wins.
        let from_the_daemon = matches!(&message.source, PipeSource::Cli(_));
        if let PipeSource::Cli(id) = &message.source {
            self.pipe_id = Some(id.clone());
        }
        if message.source == PipeSource::Plugin(self.plugin_id) {
            return false;
        }
        let input = if message.name == AGENTS_MESSAGE {
            adapter::agents(
                message.payload.as_deref().unwrap_or_default(),
                // Agent reports cannot belong here until the session has a
                // name. An empty name matches no Zellij session.
                self.application.session_name().unwrap_or_default(),
            )
            .map(Input::Agents)
        } else {
            adapter::decode_message(&message.name, message.payload.as_deref()).map(Input::Message)
        };
        if let Some(input) = input {
            self.reduce(input, false);
        }
        // This is the plugin's turn to write, and the only one it gets. A
        // message from another plugin is not a turn, because it carries no
        // pipe id of the daemon's.
        if from_the_daemon {
            self.answer_on_pipe();
        }
        false
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let area = Rect::new(0, 0, cells(cols), cells(rows));
        let rendered = self.application.render(area);
        print!(
            "{}",
            ansi::frame_to_ansi(
                &rendered.frame,
                rendered.selection.as_ref(),
                rendered.offset
            )
        );
    }
}
