//! The loop that holds the sidebar state and draws it.
//!
//! One thread owns the application and draws. Every other thread sends this one
//! kind of event and touches no state:
//!
//! ```text
//! socket reader  ->  StateArrived, ClientStopped
//! change ticker  ->  TopologyChanged
//! input reader   ->  QuitRequested
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
};
use agent_wrangler_ui::render::Sidebar as SidebarWidget;
use ratatui::DefaultTerminal;

use crate::control::{self, ControlClient};
use crate::heartbeat::HeartbeatSettings;
use crate::tmux_location::TmuxLocation;
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
    StateArrived(String),
    /// The socket reader gave up. This carries what it said.
    ClientStopped(Option<String>),
    /// Something in tmux moved, so the shape of the session must be read again.
    TopologyChanged,
    /// One whole answer to the topology question, from the control client.
    TopologyAnswered(String),
    /// The user asked this program to stop.
    QuitRequested,
    /// A program that an effect started has finished.
    CommandFinished {
        call: String,
        exit: Option<i32>,
        stderr: Vec<u8>,
    },
}

/// Whether a key press stops this program.
///
/// Raw mode stops Ctrl-C raising an interrupt, so this program must read the
/// key and act on it. Without this the pane holds a sidebar that the user
/// cannot leave.
fn is_quit(byte: u8) -> bool {
    // `q`, `Q`, Ctrl-C and Ctrl-Q.
    matches!(byte, b'q' | b'Q' | 0x03 | 0x11)
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
        let stopped = client::run_client(
            |payload| {
                events
                    .send(ClientEvent::StateArrived(payload.to_string()))
                    .map_err(|_| std::io::Error::other("the sidebar stopped"))
            },
            &heartbeat,
            &notifier,
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
/// Every byte except a request to quit is dropped. This sidebar draws, and it
/// takes no other key.
fn start_input_reader(events: Sender<ClientEvent>) {
    thread::spawn(move || {
        use std::io::Read;
        let mut input = std::io::stdin();
        let mut byte = [0u8; 1];
        while let Ok(1) = input.read(&mut byte) {
            if is_quit(byte[0]) && events.send(ClientEvent::QuitRequested).is_err() {
                return;
            }
        }
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

/// The state that the drawing thread holds beside the application.
struct Sidebar {
    application: Application,
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
            Effect::Tell(_) => {
                // Do nothing. This client opens its socket for reading alone.
                // The thread that reads the socket writes the heartbeat, which
                // is the only message that this client sends.
            }
            Effect::Broadcast(_) => {
                // Do nothing. One sidebar draws one session here, so this
                // client has no other sidebar to send a message to.
            }
            Effect::FocusPane(_) | Effect::SwitchTab(_) => {
                // Do nothing. This sidebar draws the session and changes
                // nothing in it. It reads one key, and that key stops the
                // program.
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
    /// If tmux refuses the question, this function keeps the reports that it
    /// already holds. A session that is closing answers nothing, and tmux ends
    /// this program together with the session.
    fn ask_about_the_session(&mut self) {
        if let Some(control) = self.control.as_mut() {
            if control.ask_about_the_session(&self.session).is_ok() {
                return;
            }
            // The control client went. The timer is still running, so the next
            // tick asks again through a child process.
            self.control = None;
        }
        if let Ok(answer) = tmux_query::read_topology(&self.session) {
            self.read_the_session(&answer.windows, &answer.panes);
        }
    }

    /// Feeds one answer about the session to the application.
    ///
    /// The two halves are the same bytes whichever transport carried them, so
    /// one reader serves both.
    fn read_the_session(&mut self, reported_windows: &str, reported_panes: &str) {
        let windows = topology::read_windows(reported_windows);
        let panes = topology::read_panes(reported_panes);
        self.reduce(Input::TabsReported(topology::tab_reports(&windows)));
        self.reduce(Input::LayoutReported(topology::session_layout(
            &windows,
            &panes,
            &self.own_pane,
        )));
        self.reduce(Input::FocusObserved(topology::focus(
            &windows,
            &panes,
            &self.own_pane,
        )));
        self.reduce(Input::EventSettled);
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
                    lines: view.frame.lines(),
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
        own_pane,
        server_socket: location.server_socket().to_string(),
        control: control::start_control_client(&target, events.clone()),
        session: target,
        terminal,
        events,
    };
    // The application runs no effect until it holds a permission. Tmux has no
    // permission to grant or to refuse, so this reports a granted permission
    // once and reports no other.
    sidebar.reduce(Input::PermissionReported(Permission::Granted));
    sidebar.reduce(Input::VisibilityChanged(true));
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
            ClientEvent::TopologyAnswered(text) => {
                if let Some(answer) = tmux_query::split_answer(&text) {
                    sidebar.read_the_session(&answer.windows, &answer.panes);
                }
            }
            ClientEvent::StateArrived(payload) => {
                if let Some(snapshot) = read_agents(&payload, &sidebar.server_socket) {
                    sidebar.reduce(Input::Agents(snapshot));
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

    #[test]
    fn the_keys_that_stop_this_program_are_the_ones_a_user_would_try() {
        // Raw mode stops Ctrl-C raising an interrupt, so this program must read
        // the key. A sidebar that answered none of these would be a pane that
        // the user cannot leave.
        assert!(is_quit(b'q'));
        assert!(is_quit(b'Q'));
        assert!(is_quit(0x03), "Ctrl-C");
        assert!(is_quit(0x11), "Ctrl-Q");
        assert!(!is_quit(b'j'));
        assert!(!is_quit(b'\r'));
    }
}
