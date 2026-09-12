//! The loop that holds the sidebar state and draws it.
//!
//! One thread owns the application and draws. Every other thread sends this one
//! kind of event and touches no state:
//!
//! ```text
//! socket reader  ->  StateArrived, ClientStopped
//! change ticker  ->  TopologyChanged
//! input reader   ->  UserAction
//! child runner   ->  CommandFinished
//!                        |
//!                        v
//!                 std::sync::mpsc
//!                        |
//!                        v
//!           this thread: the application, then the frame
//! ```
//!
//! Nothing here needs a runtime, an async crate or a signal handler.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use agent_wrangler_core::agent::{self, Agent, Record, SessionId};
use agent_wrangler_core::registry::Registry;
use agent_wrangler_sidebar::{
    AgentSnapshot, Application, Effect, Input, Options, PaneId, Permission, ProgramToRun,
    UserAction,
};
use agent_wrangler_ui::render::Sidebar as SidebarWidget;
use ratatui::DefaultTerminal;

use crate::control::{self, ControlClient};
use crate::heartbeat::HeartbeatSettings;
use crate::tmux_location::{SidebarRegistration, TmuxLocation};
use crate::topology;
use crate::{client, tmux_query, FatalError};

/// The variable that names the pane a process runs in.
///
/// Every hook captures it, and this program reads its own. So a hook and this
/// program name the same pane with the same text, and nothing has to translate
/// between them.
const PANE_VAR: &str = "TMUX_PANE";

/// The variable that names the tmux server and the session a process started
/// in. Only its first field is read, which is the server.
const SERVER_VAR: &str = "TMUX";

/// How long this program waits between asks when nothing tells it that
/// something moved.
///
/// This is the whole feed, so a change lands up to this late. It costs one tmux
/// process per sidebar per tick, for as long as the sidebar runs. Half a second
/// is fast enough to feel live, and slow enough that ten sidebars cost twenty
/// processes a second and no more.
pub const ASK_AGAIN_AFTER: Duration = Duration::from_millis(500);

/// One thing that reaches the thread which owns the application.
#[derive(Debug)]
pub enum ClientEvent {
    /// One state payload, exactly as it came off the socket.
    StateArrived(String, client::ClientConnection),
    User(UserAction),
    /// The socket reader gave up. This carries what it said.
    ClientStopped(Option<String>),
    /// Something in tmux moved, so the shape of the session must be read again.
    TopologyChanged,
    /// One whole answer to the topology question, from the control client.
    TopologyAnswered(String),
    /// The control connection ended. The timer must continue to poll.
    ControlStopped,
    /// The user asked this program to stop.
    QuitRequested,
    /// A program that an effect started has finished.
    CommandFinished {
        call: String,
        exit: Option<i32>,
        stderr: Vec<u8>,
    },
}

/// Reads the state of the agents that this session shows.
///
/// The payload arrives on a transport that frames its messages by the line, so
/// the record breaks travel escaped and the framing newline is still on the
/// end. Both are undone before anything reads the header, because the header is
/// split on a real newline.
///
/// A record is kept when the pane that it names belongs to this tmux server.
/// The pane id alone is not enough: two servers number their panes from the
/// same counter, so `%1` names a pane on each.
pub fn read_agents(payload: &str, server_socket: &str) -> Option<AgentSnapshot> {
    let payload = agent::restore_record_breaks(payload);
    let (format, records) = agent::read_state_message(&payload)?;
    if format != agent::FORMAT {
        return Some(AgentSnapshot::Incompatible);
    }
    let mut registry = Registry::default();
    let mut panes: BTreeMap<SessionId, PaneId> = BTreeMap::new();
    for line in records.split('\n') {
        let Record::Known(record) = Agent::decode(line) else {
            continue;
        };
        let same_server = record
            .origin
            .get(SERVER_VAR)
            .and_then(|tmux| tmux.split(',').next())
            .is_some_and(|server| !server.is_empty() && server == server_socket);
        if !same_server {
            continue;
        }
        if let Some(pane) = record.origin.get(PANE_VAR).filter(|pane| !pane.is_empty()) {
            panes.insert(record.session.clone(), PaneId::new(pane));
        }
        registry.report(record);
    }
    Some(AgentSnapshot::Compatible { registry, panes })
}

/// Starts the thread that registers this client and reads its socket.
///
/// Side effect: this spawns a thread that runs `agent-wrangler` and `tmux`, and
/// that holds a socket open for as long as this program runs.
fn start_socket_reader(
    events: Sender<ClientEvent>,
    heartbeat: HeartbeatSettings,
    notifier: Vec<String>,
) {
    thread::spawn(move || {
        let writer = client::ClientWriter::default();
        let stopped = client::run_client_with_writer(
            |payload| {
                let connection = writer.connection().expect("the payload has a connection");
                events
                    .send(ClientEvent::StateArrived(payload.to_string(), connection))
                    .map_err(|_| std::io::Error::other("the sidebar stopped"))
            },
            &heartbeat,
            &notifier,
            &writer,
        );
        let said = stopped.err().map(|why| why.to_string());
        let _ = events.send(ClientEvent::ClientStopped(said));
    });
}

/// Starts the thread that says, on a timer, that the session must be read
/// again.
///
/// This is the fallback feed. A control client says at once that something
/// moved, and a server that does not know the flags that a control client needs
/// leaves the sidebar with this timer and nothing else.
///
/// The first tick is immediate, so the sidebar draws the session rather than an
/// empty pane while it waits out the first interval.
fn start_change_ticker(events: Sender<ClientEvent>, every: Duration) {
    thread::spawn(move || {
        while events.send(ClientEvent::TopologyChanged).is_ok() {
            thread::sleep(every);
        }
    });
}

/// Starts the thread that reads the keyboard.
///
/// Side effect: this reads the standard input for as long as this program runs.
/// The decoder retains incomplete escape sequences between reads.
fn start_input_reader(events: Sender<ClientEvent>) {
    thread::spawn(move || {
        use std::io::Read;
        let mut input = std::io::stdin();
        let mut byte = [0u8; 1];
        let mut decoder = crate::input::InputDecoder::default();
        while let Ok(1) = input.read(&mut byte) {
            if let Some(action) = decoder.push(byte[0]) {
                if events.send(ClientEvent::User(action)).is_err() {
                    return;
                }
            }
        }
        let _ = events.send(ClientEvent::QuitRequested);
    });
}

/// Runs one program that an effect asked for, and reports what it did.
///
/// Side effect: this spawns a thread and a process. The thread keeps a slow
/// program from holding up the drawing.
fn start_child(events: Sender<ClientEvent>, program: ProgramToRun) {
    thread::spawn(move || {
        let finished = std::process::Command::new(&program.program)
            .args(&program.args)
            .output();
        let event = match finished {
            Ok(output) => ClientEvent::CommandFinished {
                call: program.call,
                exit: output.status.code(),
                stderr: output.stderr,
            },
            Err(why) => ClientEvent::CommandFinished {
                call: program.call,
                exit: None,
                stderr: why.to_string().into_bytes(),
            },
        };
        let _ = events.send(event);
    });
}

/// Control replies arrive in request order. A new snapshot invalidates every
/// outstanding request, including replies already queued on the event channel.
#[derive(Default)]
struct TopologyQueries {
    pending: usize,
    obsolete: usize,
}

impl TopologyQueries {
    fn requested(&mut self) {
        self.pending += 1;
    }

    fn answered(&mut self) -> bool {
        if self.pending == 0 {
            return false;
        }
        self.pending -= 1;
        if self.obsolete > 0 {
            self.obsolete -= 1;
            return false;
        }
        true
    }
}

fn adopt_snapshot(
    application: &mut Application,
    queries: &mut TopologyQueries,
    snapshot: AgentSnapshot,
) -> Vec<Effect> {
    queries.obsolete = queries.pending;
    [Input::VisibilityChanged(false), Input::Agents(snapshot)]
        .into_iter()
        .flat_map(|input| application.reduce(input).effects)
        .collect()
}

/// The state that the drawing thread holds beside the application.
struct Sidebar {
    application: Application,
    connection: Option<client::ClientConnection>,
    queries: TopologyQueries,
    panes: Vec<topology::ReportedPane>,
    /// The pane that this program runs in. It is drawn as the sidebar of its
    /// window rather than as a pane that the user can go to.
    own_pane: String,
    /// The tmux server that holds this program, which tells one server's panes
    /// from another's.
    server_socket: String,
    /// The session that this sidebar draws, as tmux names it.
    session: String,
    /// The pane, with raw mode on and the alternate screen entered. It reads
    /// its own size on every draw, and it writes only the cells that changed.
    terminal: DefaultTerminal,
    events: Sender<ClientEvent>,
    /// The control client, held only when the server knew the two flags.
    ///
    /// While this sidebar holds a control client, tmux reports every change at
    /// once, and this sidebar sends its questions to that same client. Without
    /// one, the ticker asks and a child process answers. One reader parses the
    /// answer either way.
    control: Option<ControlClient>,
}

impl Sidebar {
    /// Feeds one input to the application and runs every effect that comes
    /// back.
    fn reduce(&mut self, input: Input) {
        for effect in self.application.reduce(input).effects {
            self.run(effect);
        }
    }

    /// Runs one effect.
    ///
    /// Every arm is written out. A new effect then cannot arrive without a
    /// decision about what this client does with it.
    fn run(&mut self, effect: Effect) {
        match effect {
            Effect::Repaint => {
                // Do nothing. The loop draws after every event, so the draw
                // that this effect asks for already happens. A draw here as
                // well would compose the same frame more than once per event.
            }
            Effect::RefreshFocus => {
                // Do nothing. The next question to tmux reports the focus
                // together with everything else, so a question now would cost a
                // second process for the same answer.
            }
            Effect::RefreshPaneTitle(_) => {
                // Do nothing. The pane question that this client already
                // asks reports every pane title, so no title can be out of date
                // on its own.
            }
            Effect::Run(program) => start_child(self.events.clone(), program),
            Effect::Tell(message) => {
                if let Some(connection) = &self.connection {
                    connection.send(message);
                }
            }
            Effect::Broadcast(message) => {
                // This manually started instance is its only subscriber.
                self.reduce(Input::Message(message));
            }
            Effect::FocusPane(_) | Effect::SwitchTab(_) => {
                if let Some(mut command) =
                    tmux_query::build_activation_command(&self.session, &effect, &self.panes)
                {
                    // A target can close after the last report. Its stable id
                    // then fails without selecting a replacement.
                    let _ = command.output();
                    let _ = self.events.send(ClientEvent::TopologyChanged);
                }
            }
            Effect::StopSessionDiscovery => {
                // Do nothing. This client reads its session once, from its
                // own pane. It runs no search for a session, so this arm has
                // no search to stop.
            }
            Effect::CloseSidebar => {
                let _ = self.events.send(ClientEvent::QuitRequested);
            }
        }
    }

    /// Asks tmux about the session.
    ///
    /// Side effect: this writes on the control client when one is held, and the
    /// answer arrives later as an event. Without one it runs `tmux` and reads
    /// the answer now.
    ///
    /// If tmux refuses the question, this function keeps the rows but disables
    /// focus effects. A session that is closing can answer nothing.
    fn ask_about_the_session(&mut self) {
        if let Some(control) = self.control.as_mut() {
            if control.ask_about_the_session(&self.session).is_ok() {
                self.queries.requested();
                return;
            }
            // The control client went. The timer is still running, so the next
            // tick asks again through a child process.
            self.control = None;
            self.queries.obsolete = self.queries.pending;
        }
        if let Ok(answer) = tmux_query::read_topology(&self.session) {
            self.read_the_session(&answer);
        } else {
            self.reduce(Input::VisibilityChanged(false));
        }
    }

    /// Feeds one answer about the session to the application.
    ///
    /// The reports contain the same bytes whichever transport carried them, so
    /// one reader serves both.
    fn read_the_session(&mut self, answer: &tmux_query::TopologyAnswer) {
        self.panes = topology::read_panes(&answer.panes);
        for effect in apply_topology(&mut self.application, answer, &self.own_pane) {
            self.run(effect);
        }
    }

    /// Puts the state of the application on the pane.
    ///
    /// Side effect: this asks the terminal for its size and writes to the
    /// standard output. Only the cells that differ from the last frame are
    /// written, so this costs little when nothing moved.
    ///
    /// The size is read here and nowhere else. Nothing reports a resize to this
    /// program, so a pane that the user made narrower is drawn again at its new
    /// width because every draw reads the width afresh.
    fn draw(&mut self) -> Result<(), FatalError> {
        let application = &mut self.application;
        self.terminal
            .draw(|pane| {
                let area = pane.area();
                let view = application.render(area);
                let widget = SidebarWidget {
                    lines: &view.frame.lines()[view.offset.min(view.frame.lines().len())..],
                    selected: view.selection.as_ref(),
                };
                pane.render_widget(widget, area);
            })
            .map_err(FatalError::TerminalRefused)?;
        Ok(())
    }
}

impl Drop for Sidebar {
    /// Turns raw mode off and leaves the alternate screen, which gives the
    /// pane back as this program found it.
    ///
    /// This sidebar owns the pane for as long as it draws, so dropping the
    /// sidebar is the moment to return the pane. The terminal that this struct
    /// holds drops after this function runs, and it shows the cursor as it
    /// drops.
    fn drop(&mut self) {
        restore_input_modes();
        ratatui::restore();
    }
}

/// Draws the sidebar until something stops this program.
///
/// Side effect: this function takes the terminal, spawns four threads, and runs
/// `tmux` and `agent-wrangler`. It gives the terminal back before it returns,
/// and the panic hook that `ratatui::try_init` installs gives it back if a
/// panic ends the program.
pub fn run_sidebar(options: Options, heartbeat: HeartbeatSettings) -> Result<(), FatalError> {
    let location = TmuxLocation::from_environment()?;
    let _registration = SidebarRegistration::register(&location)?;
    let own_pane = std::env::var(PANE_VAR).unwrap_or_default();
    let session = location.read_session()?;
    // Raw mode on, the alternate screen entered, and a panic hook installed.
    //
    // Raw mode stops the pane echoing a keystroke over the drawing. It also
    // stops Ctrl-C raising an interrupt, so this program reads the input and
    // acts on a request to quit itself.
    //
    // The alternate screen has no history behind it. This program draws a whole
    // pane at a time and has nothing to scroll back through. On the normal
    // screen a host keeps a history for it anyway, which is two thousand lines
    // per sidebar in tmux by default, and a user who scrolls that pane finds
    // blank lines. Leaving the alternate screen also puts the pane back as this
    // program found it, rather than leaving the last frame behind.
    let terminal = ratatui::try_init().map_err(FatalError::TerminalRefused)?;
    let panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_input_modes();
        panic_hook(info);
    }));

    let (events, arriving) = mpsc::channel();
    let notifier = options
        .desktop
        .as_ref()
        .map(|notifier| notifier.program_and_arguments().to_vec())
        .unwrap_or_default();
    start_socket_reader(events.clone(), heartbeat, notifier);
    // The timer runs whether or not a control client does. A control client
    // that stops leaves the sidebar with the timer and no gap in the feed. A
    // tick that arrives while a control client holds an unanswered question
    // costs one line on a pipe.
    start_change_ticker(events.clone(), ASK_AGAIN_AFTER);
    start_input_reader(events.clone());

    let target = session.as_target();
    let mut sidebar = Sidebar {
        application: Application::new(options),
        connection: None,
        queries: TopologyQueries::default(),
        panes: Vec::new(),
        own_pane,
        server_socket: location.server_socket().to_string(),
        control: control::start_control_client(&target, events.clone()),
        session: target,
        terminal,
        events,
    };
    use std::io::Write;
    std::io::stdout()
        .write_all(b"\x1b[?1000h\x1b[?1006h\x1b[?2004h")
        .and_then(|()| std::io::stdout().flush())
        .map_err(FatalError::TerminalRefused)?;
    // The application runs no effect until it holds a permission. Tmux has no
    // permission to grant or to refuse, so this reports a granted permission
    // once and reports no other.
    sidebar.reduce(Input::PermissionReported(Permission::Granted));

    serve(&mut sidebar, arriving)
}

/// Reads events and draws, until the user or the daemon stops this program.
///
/// Every event ends in a draw, and no event decides whether to draw. A resize
/// explains that rule. Tmux reports a resize to no program, and a resize
/// changes nothing in the application. A sidebar that drew only after a change
/// of the application would therefore hold the frame of the old width until
/// something else moved. A draw writes only the cells that differ from the last
/// frame, so a draw that finds nothing changed costs one read of the size and
/// no output.
fn serve(sidebar: &mut Sidebar, arriving: Receiver<ClientEvent>) -> Result<(), FatalError> {
    loop {
        let event = match arriving.recv_timeout(ASK_AGAIN_AFTER) {
            Ok(event) => event,
            // Every sender lives as long as this program, so a disconnect
            // cannot happen. A timeout asks for the same work as a tick from
            // the ticker, so both arms give the same event.
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                ClientEvent::TopologyChanged
            }
        };
        match event {
            ClientEvent::TopologyChanged => sidebar.ask_about_the_session(),
            ClientEvent::ControlStopped => {
                sidebar.control = None;
                sidebar.queries.obsolete = sidebar.queries.pending;
                sidebar.reduce(Input::VisibilityChanged(false));
                sidebar.ask_about_the_session();
            }
            ClientEvent::TopologyAnswered(text) => {
                if !sidebar.queries.answered() {
                    continue;
                }
                if let Some(answer) = tmux_query::split_answer(&text) {
                    sidebar.read_the_session(&answer);
                } else {
                    sidebar.reduce(Input::VisibilityChanged(false));
                }
            }
            ClientEvent::User(action) => sidebar.reduce(Input::User(action)),
            ClientEvent::StateArrived(payload, connection) => {
                if let Some(snapshot) = read_agents(&payload, &sidebar.server_socket) {
                    if sidebar.connection.as_ref() != Some(&connection) {
                        // Forget local suppression before the new connection's
                        // authoritative state. Old answers must not be retried.
                        sidebar.reduce(Input::Agents(AgentSnapshot::Compatible {
                            registry: Registry::default(),
                            panes: BTreeMap::new(),
                        }));
                        sidebar.connection = Some(connection);
                    }
                    for effect in
                        adopt_snapshot(&mut sidebar.application, &mut sidebar.queries, snapshot)
                    {
                        sidebar.run(effect);
                    }
                    sidebar.ask_about_the_session();
                }
            }
            ClientEvent::CommandFinished { call, exit, stderr } => {
                sidebar.reduce(Input::CommandFinished { exit, stderr, call })
            }
            ClientEvent::QuitRequested => return Ok(()),
            ClientEvent::ClientStopped(said) => {
                return match said {
                    Some(why) => Err(FatalError::ClientStopped(why)),
                    None => Ok(()),
                }
            }
        }
        sidebar.draw()?;
    }
}

// Keep focus effects disabled until all reports from this answer are installed.
fn apply_topology(
    application: &mut Application,
    answer: &tmux_query::TopologyAnswer,
    own_pane: &str,
) -> Vec<Effect> {
    let windows = topology::read_windows(&answer.windows);
    let panes = topology::read_panes(&answer.panes);
    let inputs = [
        Input::VisibilityChanged(false),
        Input::ExcludedPanesChanged(
            panes
                .iter()
                .filter(|pane| pane.is_sidebar || pane.id == own_pane)
                .map(|pane| PaneId::new(pane.id.clone()))
                .collect(),
        ),
        Input::TabsReported(topology::tab_reports(&windows)),
        Input::LayoutReported(topology::session_layout(&windows, &panes, own_pane)),
        Input::VisibilityChanged(topology::has_interactive_client(&answer.clients)),
        Input::FocusObserved(topology::focus(&windows, &panes, own_pane)),
        Input::EventSettled,
    ];
    inputs
        .into_iter()
        .flat_map(|input| application.reduce(input).effects)
        .collect()
}

fn restore_input_modes() {
    use std::io::Write;
    let _ = std::io::stdout().write_all(b"\x1b[?1000l\x1b[?1006l\x1b[?2004l");
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::agent::LabelFacts;
    use agent_wrangler_core::origin::Origin;

    const SERVER: &str = "/tmp/tmux-1000/default";

    /// One record, from a pane of a named server.
    fn record(session: &str, tmux: &str, pane: &str) -> String {
        let origin = Origin::from_lookup(|name| match name {
            "TMUX" => Some(tmux.to_string()),
            "TMUX_PANE" => Some(pane.to_string()),
            _ => None,
        });
        Agent::new(
            SessionId::new(session).unwrap(),
            "claude",
            LabelFacts {
                dir: "quarry".to_string(),
                ..LabelFacts::default()
            },
            origin,
        )
        .encode()
    }

    /// A payload, framed as the daemon frames it.
    fn payload(records: &[String]) -> String {
        agent::escape_record_breaks(&agent::build_state_message(&records.join("\n")))
    }

    fn snapshot(payload: &str) -> AgentSnapshot {
        read_agents(payload, SERVER).expect("a snapshot")
    }

    fn calling_snapshot() -> AgentSnapshot {
        let mut agent = Agent::new(
            SessionId::new("caller").unwrap(),
            "claude",
            LabelFacts::default(),
            Origin::default(),
        );
        agent.turn = agent::Turn::Attention;
        agent.raised = 1;
        let panes = BTreeMap::from([(agent.session.clone(), PaneId::new("%2"))]);
        let mut registry = Registry::default();
        registry.report(agent);
        AgentSnapshot::Compatible { registry, panes }
    }

    fn answer(clients: &str) -> tmux_query::TopologyAnswer {
        tmux_query::TopologyAnswer {
            windows: "@0\t0\t0\tsidebar\n@1\t1\t1\tagent\n".into(),
            panes: "@0\t%0\t1\t1\tsidebar\n@1\t%2\t1\t\tagent\n".into(),
            clients: clients.into(),
        }
    }

    fn tells(effects: &[Effect]) -> usize {
        effects
            .iter()
            .filter(|effect| matches!(effect, Effect::Tell(agent_wrangler_sidebar::ClientMessage::Seen(session)) if session.as_str() == "caller"))
            .count()
    }

    #[test]
    fn marked_panes_are_not_content_in_any_view() {
        use agent_wrangler_ui::model::RowKey;
        for mode in ["tree", "sections", "dashboard"] {
            let options =
                Options::from_configuration(&BTreeMap::from([(mode.into(), "true".into())]));
            let mut app = Application::new(options);
            app.reduce(Input::PermissionReported(Permission::Granted));
            let report = tmux_query::TopologyAnswer {
                windows: "@0\t0\t1\twork\n@1\t1\t0\tother\n".into(),
                panes: "@0\t%0\t1\t1\town\n@0\t%1\t0\t1\tpeer-tree\n@0\t%2\t0\t1\tpeer-sections\n@1\t%3\t1\t1\tpeer-dashboard\n@0\t%4\t0\t\ttmux-agent-wrangler\n@1\t%5\t0\t\teditor\n".into(),
                clients: "0\n".into(),
            };
            apply_topology(&mut app, &report, "%0");
            let view = app.render(agent_wrangler_ui::Rect::new(0, 0, 100, 40));
            let keys: Vec<_> = view
                .interactions
                .iter()
                .flatten()
                .map(|item| &item.key)
                .collect();
            for id in ["%0", "%1", "%2", "%3"] {
                assert!(
                    !keys.contains(&&RowKey::Pane(PaneId::new(id))),
                    "{mode}: {id}"
                );
            }
            if mode != "dashboard" {
                for id in ["%4", "%5"] {
                    assert!(
                        keys.contains(&&RowKey::Pane(PaneId::new(id))),
                        "{mode}: {id}"
                    );
                }
            }
            let layout = topology::session_layout(
                &topology::read_windows(&report.windows),
                &topology::read_panes(&report.panes),
                "%0",
            );
            assert_eq!(layout.tabs[0].content_panes[0].title, "tmux-agent-wrangler");
            assert_eq!(layout.tabs[1].content_panes[0].id, PaneId::new("%5"));
        }
    }

    #[test]
    fn marked_agents_leave_groups_previews_and_notifications_but_can_return() {
        use agent_wrangler_sidebar::Broadcast;
        use agent_wrangler_ui::model::RowKey;
        let peer = SessionId::new("caller").unwrap();
        let ordinary = SessionId::new("ordinary").unwrap();
        for mode in ["tree", "sections", "dashboard"] {
            let options =
                Options::from_configuration(&BTreeMap::from([(mode.into(), "true".into())]));
            let mut app = Application::new(options);
            app.reduce(Input::PermissionReported(Permission::Granted));
            let mut report = tmux_query::TopologyAnswer {
                windows: "@0\t0\t1\twork\n".into(),
                panes: "@0\t%0\t1\t1\town\n@0\t%2\t0\t\tpeer\n@0\t%4\t0\t\tordinary\n".into(),
                clients: "0\n".into(),
            };
            apply_topology(&mut app, &report, "%0");
            let AgentSnapshot::Compatible {
                mut registry,
                mut panes,
            } = calling_snapshot()
            else {
                unreachable!()
            };
            let mut positive = Agent::new(
                ordinary.clone(),
                "claude",
                LabelFacts::default(),
                Origin::default(),
            );
            positive.turn = agent::Turn::Attention;
            positive.raised = 2;
            registry.report(positive);
            panes.insert(ordinary.clone(), PaneId::new("%4"));
            let snapshot = AgentSnapshot::Compatible { registry, panes };
            app.reduce(Input::Agents(snapshot.clone()));
            app.reduce(Input::Message(Broadcast::Selection(RowKey::Agent(
                peer.clone(),
            ))));
            let area = agent_wrangler_ui::Rect::new(0, 0, 100, 40);
            let before = app.render(area);
            assert!(
                before
                    .interactions
                    .iter()
                    .flatten()
                    .any(|item| item.key == RowKey::Agent(peer.clone())),
                "{mode}"
            );
            app.reduce(Input::User(UserAction::OpenOrClosePreview));
            let opened = app.render(area);
            if mode == "dashboard" {
                assert!(opened.frame.lines().iter().any(|row| matches!(
                    row.content,
                    agent_wrangler_ui::model::RowContent::PreviewMessage { .. }
                )));
            }
            let selected_line = opened
                .interactions
                .iter()
                .position(|item| {
                    item.as_ref()
                        .is_some_and(|item| item.key == RowKey::Agent(peer.clone()))
                })
                .unwrap();
            report.panes = report.panes.replace("%2\t0\t\t", "%2\t0\t1\t");
            let effects = apply_topology(&mut app, &report, "%0");
            for action in [UserAction::Activate, UserAction::Click(selected_line, 8)] {
                assert!(!app.reduce(Input::User(action)).effects.iter().any(
                    |effect| matches!(effect, Effect::FocusPane(pane) if pane == &PaneId::new("%2"))
                ));
            }
            assert!(!effects
                .iter()
                .any(|effect| matches!(effect, Effect::Tell(_))));
            for refresh_agents in [false, true] {
                if refresh_agents {
                    let effects = app.reduce(Input::Agents(snapshot.clone())).effects;
                    assert!(!effects
                        .iter()
                        .any(|effect| matches!(effect, Effect::Tell(_))));
                }
                let view = app.render(area);
                assert!(!view.frame.lines().iter().any(|row| matches!(
                    row.content,
                    agent_wrangler_ui::model::RowContent::PreviewMessage { .. }
                )));
                assert_ne!(view.selection, Some(RowKey::Agent(peer.clone())));
                for item in view.interactions.iter().flatten() {
                    assert!(
                        !matches!(&item.key, RowKey::Agent(id) | RowKey::Section(id) | RowKey::Notification(id) if id == &peer),
                        "{mode}: {:?}",
                        item.key
                    );
                }
                assert!(
                    view.interactions
                        .iter()
                        .flatten()
                        .any(|item| item.key == RowKey::Agent(ordinary.clone())),
                    "{mode}"
                );
                if mode != "dashboard" {
                    assert!(
                        view.interactions
                            .iter()
                            .flatten()
                            .any(|item| item.key == RowKey::Notification(ordinary.clone())),
                        "{mode}"
                    );
                }
            }
            report.panes = report.panes.replace("%2\t0\t1\t", "%2\t0\t\t");
            apply_topology(&mut app, &report, "%0");
            let restored = app.render(area);
            assert!(
                restored
                    .interactions
                    .iter()
                    .flatten()
                    .any(|item| item.key == RowKey::Agent(peer.clone())),
                "records survive filtering: {mode}"
            );
            if mode != "dashboard" {
                assert!(
                    restored
                        .interactions
                        .iter()
                        .flatten()
                        .any(|item| item.key == RowKey::Notification(peer.clone())),
                    "call state survives filtering: {mode}"
                );
            }
        }
    }

    #[test]
    fn explicit_quit_is_not_blocked_by_peer_company() {
        let mut app = Application::new(Options::default());
        app.reduce(Input::PermissionReported(Permission::Granted));
        let topology = tmux_query::TopologyAnswer {
            windows: "@0\t0\t1\tshared\n".into(),
            panes: "@0\t%0\t1\t1\town\n@0\t%2\t0\t1\tpeer\n".into(),
            clients: "0\n".into(),
        };
        assert!(!apply_topology(&mut app, &topology, "%0").contains(&Effect::CloseSidebar));
        let quit = app.reduce(Input::User(UserAction::Quit));
        assert!(quit
            .effects
            .contains(&Effect::Broadcast(agent_wrangler_sidebar::Broadcast::Off)));
        // The tmux effect runner delivers broadcasts only to this application.
        let closed = app.reduce(Input::Message(agent_wrangler_sidebar::Broadcast::Off));
        assert_eq!(closed.effects, [Effect::CloseSidebar]);
    }

    #[test]
    fn peer_focus_never_acknowledges_a_call() {
        for peer_window in ["@0", "@1"] {
            let mut app = Application::new(Options::default());
            app.reduce(Input::PermissionReported(Permission::Granted));
            let mut queries = TopologyQueries::default();
            adopt_snapshot(&mut app, &mut queries, calling_snapshot());
            let topology = tmux_query::TopologyAnswer {
                windows: if peer_window == "@0" {
                    "@0\t0\t1\tsidebar\n@1\t1\t0\tother\n".into()
                } else {
                    "@0\t0\t0\tsidebar\n@1\t1\t1\tother\n".into()
                },
                panes: format!("@0\t%0\t0\t1\town\n{peer_window}\t%2\t1\t1\tpeer\n"),
                clients: "0\n".into(),
            };
            assert_eq!(tells(&apply_topology(&mut app, &topology, "%0")), 0);
            assert_eq!(tells(&app.reduce(Input::EventSettled).effects), 0);
            assert_eq!(
                tells(&app.reduce(Input::Agents(calling_snapshot())).effects),
                0
            );
            let content = tmux_query::TopologyAnswer {
                panes: topology.panes.replace("%2\t1\t1\t", "%2\t1\t\t"),
                ..topology
            };
            assert_eq!(tells(&apply_topology(&mut app, &content, "%0")), 1);
        }
    }

    #[test]
    fn new_snapshot_waits_for_observation_after_its_arrival() {
        let mut app = Application::new(Options::default());
        app.reduce(Input::PermissionReported(Permission::Granted));
        apply_topology(&mut app, &answer("0\n1\n"), "%0");
        let mut queries = TopologyQueries::default();
        queries.requested();
        queries.requested();
        assert_eq!(
            tells(&adopt_snapshot(&mut app, &mut queries, calling_snapshot())),
            0
        );
        assert!(!queries.answered());
        queries.requested();
        assert!(!queries.answered());
        assert!(queries.answered());
        assert_eq!(tells(&apply_topology(&mut app, &answer("1\n"), "%0")), 0);
        assert_eq!(tells(&apply_topology(&mut app, &answer("0\n1\n"), "%0")), 1);
    }

    #[test]
    fn detached_topology_cannot_acknowledge_initial_or_fresh_snapshots() {
        for clients in ["", "1\n", "0\ninvalid\n"] {
            let mut app = Application::new(Options::default());
            app.reduce(Input::PermissionReported(Permission::Granted));
            app.reduce(Input::VisibilityChanged(true));
            app.reduce(Input::Agents(calling_snapshot()));
            assert_eq!(tells(&apply_topology(&mut app, &answer(clients), "%0")), 0);
            assert_eq!(
                tells(&app.reduce(Input::Agents(calling_snapshot())).effects),
                0
            );
            assert_eq!(tells(&apply_topology(&mut app, &answer("0\n1\n"), "%0")), 1);
        }
    }

    #[test]
    fn a_record_from_this_server_is_placed_on_its_pane() {
        let wire = payload(&[record("one", &format!("{SERVER},3242,0"), "%12")]);
        let AgentSnapshot::Compatible { registry, panes } = snapshot(&wire) else {
            panic!("a compatible snapshot");
        };
        assert_eq!(registry.iter().count(), 1);
        assert_eq!(
            panes.get(&SessionId::new("one").unwrap()),
            Some(&PaneId::new("%12"))
        );
    }

    #[test]
    fn a_record_from_another_server_is_not_this_sessions_record() {
        // Two tmux servers number their panes from the same counter, so `%12`
        // names a pane on each. Without the server, a sidebar would draw an
        // agent that runs somewhere the user cannot reach from here.
        let wire = payload(&[record("one", "/tmp/tmux-1000/other,3242,0", "%12")]);
        let AgentSnapshot::Compatible { registry, .. } = snapshot(&wire) else {
            panic!("a compatible snapshot");
        };
        assert_eq!(registry.iter().count(), 0);
    }

    #[test]
    fn a_record_from_no_multiplexer_at_all_is_not_this_sessions_record() {
        // An agent that runs outside tmux reports an empty server, and this
        // sidebar must draw none of those agents. So an empty server matches no
        // sidebar. The second assertion covers the sidebar whose own server is
        // empty too, which a plain string comparison would match.
        let wire = payload(&[record("one", "", "")]);
        let AgentSnapshot::Compatible { registry, .. } = snapshot(&wire) else {
            panic!("a compatible snapshot");
        };
        assert_eq!(registry.iter().count(), 0);
        assert_eq!(
            read_agents(&wire, ""),
            Some(AgentSnapshot::Compatible {
                registry: Registry::default(),
                panes: BTreeMap::new(),
            })
        );
    }

    #[test]
    fn a_record_with_no_pane_is_still_this_sessions_record() {
        // The record names this server and names no pane. The agent runs in
        // this session, so the sidebar keeps the record. No row draws it,
        // because a row needs a pane to draw against.
        let wire = payload(&[record("one", &format!("{SERVER},3242,0"), "")]);
        let AgentSnapshot::Compatible { registry, panes } = snapshot(&wire) else {
            panic!("a compatible snapshot");
        };
        assert_eq!(registry.iter().count(), 1);
        assert!(panes.is_empty());
    }

    #[test]
    fn a_state_of_a_format_this_build_does_not_know_says_so() {
        // The number is never written here. Reading the constant keeps this
        // test about the mismatch rather than about today's format.
        let wire = agent::escape_record_breaks(&format!("wrangler {}\n", agent::FORMAT + 1));
        assert_eq!(
            read_agents(&wire, SERVER),
            Some(AgentSnapshot::Incompatible)
        );
    }

    #[test]
    fn a_payload_that_is_not_a_state_message_says_nothing() {
        assert_eq!(read_agents("", SERVER), None);
        assert_eq!(read_agents("not a state\n", SERVER), None);
    }
}
