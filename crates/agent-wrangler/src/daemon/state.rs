//! Everything the daemon knows, and what each event does to it.
//!
//! The reading and the clock come only from [`World`]. Every rule here runs
//! against a fake with no files, no processes and no waiting. Only the reading
//! itself is left in the real implementation.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use agent_wrangler_core::agent::{self, Agent, Meta, Process, SessionId, Turn};
use agent_wrangler_core::label::{label, Label};
use agent_wrangler_core::notify::Notifier;
use agent_wrangler_core::origin::Origin;
use agent_wrangler_core::payload::dir;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_core::titles;

use crate::proto::{Hook, Sink};

/// Where a session's own account of itself is kept. The daemon can read it
/// again without a new message from the agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    /// Which agent this is. The agent kind decides how the daemon reads its
    /// files.
    pub agent: String,
    /// The file the agent writes the conversation to.
    pub transcript: String,
    /// The modification time last read from it. If a file did not move, it
    /// needs no second look. A look at every session then costs one stat for
    /// each session rather than one scan for each session.
    pub mtime: Option<u64>,
}

/// The reading, the clock and the process table, behind one seam.
pub trait World {
    /// What an agent's own files say this session is called.
    fn meta(&self, agent: &str, transcript: &str, session: &str) -> Meta;
    /// When a file last changed, or `None` for one that is not there.
    fn mtime(&self, path: &str) -> Option<u64>;
    /// Whether a process still runs, and is still the intended one.
    fn alive(&self, process: &Process) -> bool;
}

/// The real one: an agent's files, the filesystem, and this machine's processes.
pub struct Real;

impl World for Real {
    fn meta(&self, agent: &str, transcript: &str, session: &str) -> Meta {
        match agent {
            "claude" => titles::claude(transcript),
            "copilot" => match crate::paths::home() {
                Some(home) => titles::copilot(&home, session),
                None => Meta::default(),
            },
            _ => Meta::default(),
        }
    }

    fn mtime(&self, path: &str) -> Option<u64> {
        std::fs::metadata(Path::new(path))
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|since| since.as_millis() as u64)
    }

    fn alive(&self, process: &Process) -> bool {
        crate::platform::running(process)
    }
}

/// What an event says about whose turn it is.
///
/// An error is the one event whose meaning depends on who raised it. Copilot
/// says whether it can carry on. An error that Copilot can carry on from is
/// still work, and not a call for the user. Every other agent's error is
/// something that the user must look at. An unrecognized event is a session
/// that announces itself. A session already known restates where it is the
/// same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The session starts, or restates itself. The event does not say whose
    /// turn it is.
    Announce,
    /// The session is over.
    End,
    Turn(Turn),
}

pub fn event(agent: &str, name: &str, recoverable: Option<bool>) -> Event {
    match name {
        "end" => Event::End,
        "working" => Event::Turn(Turn::Working),
        "needsAttention" => Event::Turn(Turn::Attention),
        "error" if agent == "copilot" && recoverable == Some(true) => Event::Turn(Turn::Working),
        "error" => Event::Turn(Turn::Attention),
        _ => Event::Announce,
    }
}

/// One client: where to reach it, and what it announces a call for the user
/// with.
///
/// A client says what to announce with, and announces nothing itself. Every
/// client gets the same state. A client that raises its own notifications
/// raises each call once for every client that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    pub sink: Sink,
    pub notify: Option<Notifier>,
}

/// Every agent session the daemon holds, and every client it delivers to.
#[derive(Debug, Default)]
pub struct State {
    registry: Registry,
    /// One per session held, so the daemon can read a transcript again between
    /// events.
    sources: BTreeMap<SessionId, Source>,
    /// Where to deliver, newest last. A client that registers twice is one
    /// client, so this list never holds the same sink twice.
    clients: Vec<Client>,
    /// When each client last said anything at all.
    ///
    /// A client that says nothing for [`SILENCE`] is a client that the daemon
    /// gives up on. This map holds one entry for each client, and never an
    /// entry for a client that left.
    spoke: BTreeMap<Sink, Instant>,
}

/// How long a client may say nothing before the daemon gives up on it.
///
/// A client answers one question by speaking: can it still send a message? An
/// open connection does not answer that question. It says that the kernel kept
/// the connection, and says nothing about the process behind it.
///
/// This is three times [`agent_wrangler_core::told::BEAT`], so two lost beats
/// retire nobody. A client that
/// is retired goes deaf for good, because it registers once. So this must also
/// cover a sidebar restarting, and a daemon restarting with its clients
/// connecting to it again. The tmux client bounds its reconnect at about two
/// seconds and holds a test on that bound.
pub const SILENCE: Duration = Duration::from_secs(90);

/// What an agent's files said, read before the daemon takes the lock.
///
/// The reading is separate from the filing, because a file can take
/// arbitrarily long to open. Examples are a hung network mount, a dead sshfs,
/// and a named pipe with no writer. If the daemon holds the state during the
/// read, no other event on the machine gets a record. The daemon answers its socket while it
/// is frozen, so nothing can take over from it either.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    meta: Meta,
    mtime: Option<u64>,
}

/// A call for the user, in the words that make an announcement.
///
/// It says which agent asks, and which of its sessions. Where that session is
/// comes along unread. The daemon does not know what the variables in an
/// origin point at. A call from an agent in no multiplexer at all carries an
/// empty origin, and is still a call. The notifier knows what the variables
/// mean, and is the only reason that they are here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub agent: String,
    pub label: String,
    pub origin: Origin,
}

/// The result of an event that the daemon took in.
///
/// A call is a change as well as an announcement, so one result covers both.
/// Two separate results let a call go out while no client got the change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The event said nothing that is not already held.
    Nothing,
    /// What is held changed, so the daemon owes every client the state.
    Changed,
    /// The change was an agent that asks for the user. The daemon draws this
    /// change and also says it out loud.
    Called(Call),
}

impl Applied {
    /// The result for a change with nothing to announce.
    fn told(changed: bool) -> Self {
        match changed {
            true => Applied::Changed,
            false => Applied::Nothing,
        }
    }

    /// Whether a client draws anything differently for this.
    pub fn changed(&self) -> bool {
        !matches!(self, Applied::Nothing)
    }

    /// The call to announce, for the one event that is news wherever the user
    /// is.
    pub fn call(&self) -> Option<&Call> {
        match self {
            Applied::Called(call) => Some(call),
            _ => None,
        }
    }
}

/// What the next look must cover.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Session, agent kind, transcript path.
    pub watch: Vec<(SessionId, String, String)>,
    /// Session, and the process that is said to run it.
    pub processes: Vec<(SessionId, Process)>,
}

/// What the look found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// Session, the transcript's new modification time, and what it now says.
    pub moved: Vec<(SessionId, u64, Meta)>,
    /// Sessions whose process no longer runs.
    pub dead: Vec<SessionId>,
}

/// This function carries out a plan. It touches the filesystem and the process
/// table, and holds nothing. Every other event can carry on while it runs.
pub fn look(plan: &Plan, world: &dyn World, since: &BTreeMap<SessionId, Option<u64>>) -> Look {
    Look {
        dead: plan
            .processes
            .iter()
            .filter(|(_, process)| !world.alive(process))
            .map(|(session, _)| session.clone())
            .collect(),
        moved: plan
            .watch
            .iter()
            .filter_map(|(session, agent, transcript)| {
                let mtime = world.mtime(transcript)?;
                if since.get(session).copied().flatten() == Some(mtime) {
                    return None;
                }
                Some((
                    session.clone(),
                    mtime,
                    world.meta(agent, transcript, session.as_str()),
                ))
            })
            .collect(),
    }
}

/// This function reads what a hook named. It touches the filesystem and
/// nothing else.
pub fn read_hook(hook: &Hook, world: &dyn World) -> Reading {
    Reading {
        meta: world.meta(&hook.agent, &hook.transcript, &hook.session_id),
        mtime: world.mtime(&hook.transcript),
    }
}

impl State {
    /// This method takes in what a hook reported, with what its files already
    /// said.
    pub fn apply_hook(&mut self, hook: &Hook, reading: Reading) -> Applied {
        let Some(session) = SessionId::new(&hook.session_id) else {
            return Applied::Nothing;
        };
        let event = event(&hook.agent, &hook.event, hook.recoverable);
        if event == Event::End {
            self.sources.remove(&session);
            return Applied::told(self.registry.end(&session));
        }

        let meta = Meta {
            dir: dir(&hook.cwd),
            ..reading.meta
        };
        let turn = match event {
            Event::Turn(turn) => turn,
            _ => Turn::Idle,
        };
        // Only a call for the user carries the time that it was raised. A time
        // on every event makes two identical reports look different, and
        // reorders the notification area for nothing.
        let raised = match turn {
            Turn::Attention => hook.at,
            _ => 0,
        };
        let record = Agent {
            process: hook.process,
            turn,
            raised,
            ..Agent::new(
                session.clone(),
                &hook.agent,
                meta,
                Origin::decode(&hook.origin),
            )
        };

        self.sources.insert(
            session.clone(),
            Source {
                agent: hook.agent.clone(),
                transcript: hook.transcript.clone(),
                mtime: reading.mtime,
            },
        );

        let changed = match event {
            Event::Announce => self.registry.start(record),
            _ => self.registry.report(record),
        };

        // A call is announced from what was filed rather than from what
        // arrived, because a hook reports only what it can find. The title that
        // a session took two events ago is part of what it is called now.
        //
        // A hook that told nobody anything new is a call that nobody has to
        // hear about. An agent that restates where it is therefore does not
        // announce itself over and over.
        match (changed, turn) {
            (true, Turn::Attention) => match self.registry.get(&session) {
                Some(filed) => Applied::Called(Call {
                    agent: filed.agent.clone(),
                    label: label(filed, Label::Name),
                    origin: filed.origin.clone(),
                }),
                None => Applied::Changed,
            },
            (changed, _) => Applied::told(changed),
        }
    }

    /// This method reads what the hook named and files it, in one step.
    ///
    /// The daemon does the two halves separately, so the reading happens with
    /// no lock held. This method makes the same two calls with nothing in
    /// between. It is for tests about what a hook does, and not about the
    /// order that is safe.
    #[cfg(test)]
    pub fn on_hook(&mut self, hook: &Hook, world: &dyn World) -> Applied {
        self.apply_hook(hook, read_hook(hook, world))
    }

    /// The user reached a session that called for them.
    pub fn on_seen(&mut self, session: &str) -> bool {
        match SessionId::new(session) {
            Some(session) => self.registry.seen(&session),
            None => false,
        }
    }

    /// This method registers a client for delivery from now on. A client that
    /// registers twice is still one client, and states afresh what it announces
    /// a call with.
    ///
    /// Side effect: a register is a line from the client, so this method starts
    /// the client's clock. A client therefore has a whole [`SILENCE`] to
    /// connect and to speak for itself. A daemon that restored its clients from
    /// disk gives each of them the same.
    pub fn register(&mut self, client: Client) {
        self.spoke.insert(client.sink.clone(), Instant::now());
        match self
            .clients
            .iter_mut()
            .find(|held| held.sink == client.sink)
        {
            Some(held) => *held = client,
            None => self.clients.push(client),
        }
    }

    /// This method records that a client said something at `now`.
    ///
    /// Any line from a client counts. What the line says is a separate
    /// question, and a client with something to say sends no separate beat.
    ///
    /// A line from a sink that no client holds is passed over. That is a client
    /// that the daemon already gave up on, and a late line does not bring it
    /// back. It registers again or it stays gone.
    pub fn spoke(&mut self, sink: &Sink, now: Instant) {
        if self.clients.iter().any(|held| &held.sink == sink) {
            self.spoke.insert(sink.clone(), now);
        }
    }

    /// Every client that has said nothing for [`SILENCE`].
    pub fn silent(&self, now: Instant) -> Vec<Sink> {
        self.spoke
            .iter()
            .filter(|(_, spoke)| now.duration_since(**spoke) >= SILENCE)
            .map(|(sink, _)| sink.clone())
            .collect()
    }

    /// This method gives up on one client, whatever the daemon knew about it.
    /// It returns whether there was one to give up on.
    pub fn retire(&mut self, sink: &Sink) -> bool {
        self.spoke.remove(sink);
        let held = self.clients.iter().any(|held| &held.sink == sink);
        self.clients.retain(|held| &held.sink != sink);
        held
    }

    pub fn clients(&self) -> Vec<Client> {
        self.clients.clone()
    }

    /// Whether any agent waits for the user.
    ///
    /// A client has something to say back only while this is true, because the
    /// one thing it says is that a call was answered. The daemon writes down
    /// its transports more often while this is true, so that a client gets the
    /// chance to say it.
    pub fn anyone_calling(&self) -> bool {
        !self.registry.calling().is_empty()
    }

    /// What a call for the user is announced with, once each.
    ///
    /// A notifier belongs to the user rather than to any one client. Two
    /// clients that ask for the same notifier describe one desktop to tell, and
    /// two messages to it are the same notification twice. Two clients that ask
    /// for different notifiers are two places to tell, and each one is told.
    pub fn notifiers(&self) -> Vec<Notifier> {
        let mut notifiers: Vec<Notifier> = Vec::new();
        for notifier in self
            .clients
            .iter()
            .filter_map(|client| client.notify.as_ref())
        {
            if !notifiers.contains(notifier) {
                notifiers.push(notifier.clone());
            }
        }
        notifiers
    }

    /// What every client is sent: the whole state, every time, as a message
    /// that says so.
    ///
    /// A state with no agents is sent exactly like a state with some. A client
    /// that cannot tell an empty state from an empty message cannot ignore a
    /// message that was not this one.
    pub fn payload(&self) -> String {
        agent::state(&self.registry.encode())
    }

    /// Every session held, for a report of what is there rather than for a
    /// change to it.
    #[cfg(test)]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// What the next look must cover: the transcripts held, and the processes
    /// to ask after. This method reads nothing.
    pub fn plan(&self) -> Plan {
        Plan {
            watch: self
                .sources
                .iter()
                .map(|(session, source)| {
                    (
                        session.clone(),
                        source.agent.clone(),
                        source.transcript.clone(),
                    )
                })
                .collect(),
            processes: self
                .registry
                .iter()
                .filter_map(|agent| {
                    agent
                        .process
                        .map(|process| (agent.session.clone(), process))
                })
                .collect(),
        }
    }

    /// This method takes in what the look found. If the look changed anything,
    /// this method returns `true`.
    ///
    /// This is the whole reason for a daemon rather than a hook that reports
    /// and exits. A session titles itself, or gets a color, with no hook at
    /// all. An agent that is killed fires no end event. A program that only
    /// listens sees neither of them.
    pub fn observe(&mut self, look: Look) -> bool {
        let mut changed = false;

        // An agent whose process went away is gone, whatever it last said.
        for session in look.dead {
            self.sources.remove(&session);
            changed |= self.registry.end(&session);
        }

        for (session, mtime, found) in look.moved {
            // A session that ended during the look is not one to bring back.
            let Some(source) = self.sources.get_mut(&session) else {
                continue;
            };
            source.mtime = Some(mtime);
            let Some(held) = self.registry.get(&session).cloned() else {
                continue;
            };
            // The directory is not in the transcript. The daemon keeps it from
            // what the last hook said, because a scan never looks for it.
            let record = Agent {
                meta: Meta {
                    dir: held.meta.dir.clone(),
                    ..found
                },
                ..held
            };
            changed |= self.registry.report(record);
        }

        changed
    }

    /// This method plans, looks and takes in, in one step. The daemon does the
    /// three parts separately, for the same reason as the hook path.
    #[cfg(test)]
    pub fn poll(&mut self, world: &dyn World) -> bool {
        let plan = self.plan();
        let look = look(&plan, world, &self.mtimes());
        self.observe(look)
    }

    /// What each held transcript last read as. This tells a file that moved
    /// from a file that did not move.
    pub fn mtimes(&self) -> BTreeMap<SessionId, Option<u64>> {
        self.sources
            .iter()
            .map(|(session, source)| (session.clone(), source.mtime))
            .collect()
    }

    /// The state as it is kept between runs: every session, with the file that
    /// the daemon reads its account of itself from.
    pub fn snapshot(&self) -> Vec<(String, Source)> {
        self.registry
            .iter()
            .map(|agent| {
                let source = self
                    .sources
                    .get(&agent.session)
                    .cloned()
                    .unwrap_or_default();
                (agent.encode(), source)
            })
            .collect()
    }

    /// This method takes back a snapshot. It drops every session that nothing
    /// can vouch for.
    ///
    /// A daemon that restarted does not know which of these sessions still run.
    /// A record that names a live process is kept. Every other record is
    /// dropped, because a live agent says so again on its very next event of
    /// any kind. A dead record that is kept is drawn for good.
    pub fn restore(&mut self, saved: Vec<(String, Source)>, world: &dyn World) {
        for (line, source) in saved {
            let agent_wrangler_core::agent::Record::Known(agent) = Agent::decode(&line) else {
                continue;
            };
            match agent.process {
                Some(process) if world.alive(&process) => {}
                _ => continue,
            }
            self.sources.insert(agent.session.clone(), source);
            self.registry.report(agent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use agent_wrangler_core::agent::Started;

    /// A world with no files and no processes. A test states three things
    /// outright: what a transcript says, the time it last changed, and what
    /// runs under each pid.
    ///
    /// A pid maps to the process that holds it now. A test can therefore state
    /// that one number went to another process.
    #[derive(Default)]
    struct Fake {
        meta: RefCell<BTreeMap<String, Meta>>,
        mtime: RefCell<BTreeMap<String, u64>>,
        alive: RefCell<BTreeMap<u32, Option<Started>>>,
    }

    impl Fake {
        fn says(&self, transcript: &str, meta: Meta, mtime: u64) {
            self.meta.borrow_mut().insert(transcript.to_string(), meta);
            self.mtime
                .borrow_mut()
                .insert(transcript.to_string(), mtime);
        }

        fn running(&self, process: Process) {
            self.alive.borrow_mut().insert(process.pid, process.started);
        }

        fn killed(&self, pid: u32) {
            self.alive.borrow_mut().remove(&pid);
        }
    }

    impl World for Fake {
        fn meta(&self, _agent: &str, transcript: &str, _session: &str) -> Meta {
            self.meta
                .borrow()
                .get(transcript)
                .cloned()
                .unwrap_or_default()
        }

        fn mtime(&self, path: &str) -> Option<u64> {
            self.mtime.borrow().get(path).copied()
        }

        fn alive(&self, process: &Process) -> bool {
            self.alive.borrow().get(&process.pid) == Some(&process.started)
        }
    }

    /// The process that the hooks in these tests report themselves as.
    const AGENT: Process = Process {
        pid: 4242,
        started: Some(Started(918_273)),
    };

    fn titled(title: &str) -> Meta {
        Meta {
            title: title.to_string(),
            ..Meta::default()
        }
    }

    fn hook(event: &str) -> Hook {
        Hook {
            agent: "claude".to_string(),
            event: event.to_string(),
            session_id: "one".to_string(),
            cwd: "/home/u/wrangler".to_string(),
            transcript: "/t/one.jsonl".to_string(),
            recoverable: None,
            origin: Origin::from(|name| match name {
                "ZELLIJ_SESSION_NAME" => Some("proto".to_string()),
                "ZELLIJ_PANE_ID" => Some("7".to_string()),
                _ => None,
            })
            .encode(),
            process: Some(AGENT),
            at: 1000,
        }
    }

    fn session(text: &str) -> SessionId {
        SessionId::new(text).unwrap()
    }

    fn notifier(program: &str) -> Option<Notifier> {
        Notifier::new(vec![program.to_string()])
    }

    fn client(session: &str, notify: Option<Notifier>) -> Client {
        Client {
            sink: Sink::Zellij {
                session: session.to_string(),
            },
            notify,
        }
    }

    #[test]
    fn each_event_says_whose_turn_it_is() {
        assert_eq!(event("claude", "start", None), Event::Announce);
        assert_eq!(event("claude", "end", None), Event::End);
        assert_eq!(event("claude", "working", None), Event::Turn(Turn::Working));
        assert_eq!(
            event("claude", "needsAttention", None),
            Event::Turn(Turn::Attention)
        );
    }

    #[test]
    fn an_unrecognised_event_registers_the_session() {
        assert_eq!(event("claude", "", None), Event::Announce);
        assert_eq!(event("claude", "wat", Some(true)), Event::Announce);
    }

    #[test]
    fn only_a_copilot_error_it_can_carry_on_from_is_still_working() {
        assert_eq!(
            event("copilot", "error", Some(true)),
            Event::Turn(Turn::Working)
        );
        assert_eq!(
            event("copilot", "error", Some(false)),
            Event::Turn(Turn::Attention)
        );
        assert_eq!(
            event("copilot", "error", None),
            Event::Turn(Turn::Attention)
        );
        assert_eq!(
            event("claude", "error", Some(true)),
            Event::Turn(Turn::Attention)
        );
    }

    #[test]
    fn a_hook_files_the_session_it_names() {
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        let mut state = State::default();
        assert!(state.on_hook(&hook("start"), &world).changed());
        let held = state.registry().get(&session("one")).unwrap();
        assert_eq!(held.meta.title, "the port");
        assert_eq!(held.meta.dir, "wrangler");
        assert_eq!(held.origin.get("ZELLIJ_PANE_ID"), Some("7"));
        assert_eq!(held.process, Some(AGENT));
    }

    #[test]
    fn an_event_naming_no_session_is_nothing_to_file() {
        let world = Fake::default();
        let mut state = State::default();
        let mut nameless = hook("start");
        nameless.session_id = String::new();
        assert!(!state.on_hook(&nameless, &world).changed());
        assert!(state.registry().is_empty());
    }

    #[test]
    fn the_end_of_a_session_takes_it_away() {
        let world = Fake::default();
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        assert!(state.on_hook(&hook("end"), &world).changed());
        assert!(state.registry().is_empty());
    }

    #[test]
    fn only_a_call_for_the_user_carries_when_it_was_raised() {
        let world = Fake::default();
        let mut state = State::default();
        state.on_hook(&hook("needsAttention"), &world);
        assert_eq!(state.registry().get(&session("one")).unwrap().raised, 1000);
        state.on_hook(&hook("working"), &world);
        assert_eq!(state.registry().get(&session("one")).unwrap().raised, 0);
    }

    #[test]
    fn arriving_at_a_session_answers_it() {
        let world = Fake::default();
        let mut state = State::default();
        state.on_hook(&hook("needsAttention"), &world);
        assert!(state.on_seen("one"));
        assert_eq!(
            state.registry().get(&session("one")).unwrap().turn,
            Turn::Idle
        );
        // A second report of the same thing is not a change. A session that
        // nobody knows is not one to answer.
        assert!(!state.on_seen("one"));
        assert!(!state.on_seen("nobody"));
    }

    #[test]
    fn a_client_registering_twice_is_still_one_client() {
        let mut state = State::default();
        let one = client("proto", None);
        state.register(one.clone());
        state.register(one.clone());
        assert_eq!(state.clients(), vec![one]);
    }

    #[test]
    fn a_client_registering_again_says_afresh_what_to_announce_with() {
        // A sidebar that was reloaded with the option off is the same client
        // with something different to say. It is not a second client.
        let mut state = State::default();
        state.register(client("proto", notifier("notify-send")));
        assert_eq!(state.notifiers(), vec![notifier("notify-send").unwrap()]);
        state.register(client("proto", None));
        assert!(state.notifiers().is_empty());
    }

    #[test]
    fn a_call_is_announced_once_however_many_clients_ask_for_the_same_thing() {
        // The reason that a client does not raise its own: every one of them
        // holds the same call, and the user has one desktop.
        let mut state = State::default();
        state.register(client("one", notifier("notify-send")));
        state.register(client("two", notifier("notify-send")));
        assert_eq!(state.notifiers(), vec![notifier("notify-send").unwrap()]);
    }

    #[test]
    fn clients_asking_for_different_things_are_each_told() {
        let mut state = State::default();
        state.register(client("one", notifier("notify-send")));
        state.register(client("two", notifier("/opt/announce")));
        state.register(client("three", None));
        assert_eq!(
            state.notifiers(),
            vec![
                notifier("notify-send").unwrap(),
                notifier("/opt/announce").unwrap()
            ]
        );
    }

    #[test]
    fn a_client_given_up_on_takes_its_notifier_with_it() {
        let mut state = State::default();
        let one = client("proto", notifier("notify-send"));
        state.register(one.clone());
        assert!(state.retire(&one.sink));
        assert!(state.notifiers().is_empty());
    }

    #[test]
    fn a_client_is_given_up_on_only_after_it_stayed_quiet_for_long_enough() {
        let mut state = State::default();
        let one = client("proto", None);
        state.register(one.clone());
        let now = Instant::now();
        assert!(state.silent(now).is_empty(), "it only just registered");
        assert_eq!(state.silent(now + SILENCE), vec![one.sink]);
    }

    #[test]
    fn a_client_that_spoke_is_quiet_from_that_moment_and_not_from_before_it() {
        let mut state = State::default();
        let one = client("proto", None);
        state.register(one.clone());
        let now = Instant::now();
        state.spoke(&one.sink, now + SILENCE);
        assert!(
            state.silent(now + SILENCE).is_empty(),
            "the line restarts the clock"
        );
        assert_eq!(state.silent(now + SILENCE + SILENCE), vec![one.sink]);
    }

    #[test]
    fn a_line_from_a_client_that_already_left_brings_nothing_back() {
        // A client that the daemon gave up on registers again or stays gone. A
        // late line on a transport that is closing must not resurrect a clock
        // for a client that nothing delivers to.
        let mut state = State::default();
        let one = client("proto", None);
        state.register(one.clone());
        assert!(state.retire(&one.sink));
        state.spoke(&one.sink, Instant::now());
        assert!(state.silent(Instant::now() + SILENCE).is_empty());
        assert!(state.clients().is_empty());
    }

    #[test]
    fn registering_again_starts_the_clock_again() {
        let mut state = State::default();
        let one = client("proto", None);
        state.register(one.clone());
        let now = Instant::now();
        assert_eq!(state.silent(now + SILENCE), vec![one.sink.clone()]);
        state.register(one.clone());
        assert!(state.silent(now + SILENCE).is_empty());
        assert_eq!(state.clients(), vec![one]);
    }

    #[test]
    fn the_silence_covers_more_than_one_lost_beat() {
        // One is not enough. A client registers once, and it cannot know that
        // the daemon dropped it. A single beat can go missing for a passing
        // reason: a sidebar holds a pipe id that the daemon replaced, or the
        // multiplexer is busy. One lost beat is then enough to leave that
        // sidebar with whatever it last received, for good, and with no word
        // about why.
        assert!(SILENCE >= agent_wrangler_core::told::BEAT * 3);
    }

    #[test]
    fn an_agent_asking_for_the_user_is_a_call_to_announce() {
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        let mut state = State::default();
        assert_eq!(
            state.on_hook(&hook("needsAttention"), &world).call(),
            Some(&Call {
                agent: "claude".to_string(),
                label: "the port".to_string(),
                origin: Origin::decode(&hook("needsAttention").origin),
            })
        );
    }

    #[test]
    fn a_call_carries_where_the_agent_said_it_was() {
        // The daemon reads none of it. The origin is carried so that a notifier
        // that knows what a session name is can be told which one to speak to.
        // The alternative is whatever environment the daemon was spawned into,
        // which it holds from then on.
        let world = Fake::default();
        let mut state = State::default();
        let called = state.on_hook(&hook("needsAttention"), &world);
        let origin = &called.call().expect("a call").origin;
        assert_eq!(origin.get("ZELLIJ_SESSION_NAME"), Some("proto"));
        assert_eq!(origin.get("ZELLIJ_PANE_ID"), Some("7"));
    }

    #[test]
    fn a_call_is_announced_by_what_the_session_is_called_now() {
        // A hook carries what it can find at the time. A title found two events
        // ago is still what the session is called.
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        world.says("/t/one.jsonl", Meta::default(), 2);
        let applied = state.on_hook(&hook("needsAttention"), &world);
        assert_eq!(
            applied.call().map(|call| call.label.as_str()),
            Some("the port")
        );
    }

    #[test]
    fn an_untitled_session_is_announced_by_where_it_is_working() {
        let world = Fake::default();
        let mut state = State::default();
        let applied = state.on_hook(&hook("needsAttention"), &world);
        assert_eq!(
            applied.call().map(|call| call.label.as_str()),
            Some("wrangler")
        );
    }

    #[test]
    fn nothing_but_a_call_for_the_user_is_announced() {
        let world = Fake::default();
        let mut state = State::default();
        for event in ["start", "working", "end"] {
            assert_eq!(state.on_hook(&hook(event), &world).call(), None, "{event}");
        }
    }

    #[test]
    fn a_hook_that_says_nothing_new_announces_nothing() {
        // An agent that restates a call it already makes reports the same call.
        // Two reports of it are the same notification twice.
        let world = Fake::default();
        let mut state = State::default();
        assert!(state
            .on_hook(&hook("needsAttention"), &world)
            .call()
            .is_some());
        let again = state.on_hook(&hook("needsAttention"), &world);
        assert_eq!(again, Applied::Nothing);
    }

    #[test]
    fn asking_again_after_being_answered_is_another_call() {
        let world = Fake::default();
        let mut state = State::default();
        state.on_hook(&hook("needsAttention"), &world);
        state.on_seen("one");
        assert!(state
            .on_hook(&hook("needsAttention"), &world)
            .call()
            .is_some());
    }

    #[test]
    fn a_transcript_that_has_moved_is_read_again() {
        // The whole point of the watch: a session takes a title, or gets a
        // color, with no hook at all.
        let world = Fake::default();
        world.says("/t/one.jsonl", Meta::default(), 1);
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        assert_eq!(
            state.registry().get(&session("one")).unwrap().meta.title,
            ""
        );

        world.says("/t/one.jsonl", titled("named at last"), 2);
        assert!(state.poll(&world));
        assert_eq!(
            state.registry().get(&session("one")).unwrap().meta.title,
            "named at last"
        );
        // A file that did not move since then is not read again.
        assert!(!state.poll(&world));
    }

    #[test]
    fn watching_keeps_the_directory_no_transcript_mentions() {
        let world = Fake::default();
        world.says("/t/one.jsonl", Meta::default(), 1);
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        world.says("/t/one.jsonl", titled("named"), 2);
        state.poll(&world);
        assert_eq!(
            state.registry().get(&session("one")).unwrap().meta.dir,
            "wrangler"
        );
    }

    #[test]
    fn an_agent_that_was_killed_without_saying_so_is_reaped() {
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        assert!(!state.poll(&world));

        world.killed(4242);
        assert!(state.poll(&world));
        assert!(state.registry().is_empty());
    }

    #[test]
    fn a_number_handed_on_to_another_process_does_not_vouch_for_the_agent() {
        // A pid outlives nothing: the system gives the same number to whatever
        // starts next. A record that asks only after the number is told that a
        // stranger is its agent.
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        assert!(!state.poll(&world));

        world.running(Process {
            pid: AGENT.pid,
            started: Some(Started(999_999)),
        });
        assert!(state.poll(&world));
        assert!(state.registry().is_empty());
    }

    #[test]
    fn an_agent_that_could_not_say_what_process_it_is_is_left_alone() {
        // Nothing can vouch for it, so nothing can condemn it either. It goes
        // on its own end event and on nothing else.
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        let mut state = State::default();
        let mut anonymous = hook("start");
        anonymous.process = None;
        state.on_hook(&anonymous, &world);
        assert!(!state.poll(&world));
        assert!(!state.registry().is_empty());
    }

    #[test]
    fn a_restart_keeps_only_what_is_still_running() {
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        world.running(AGENT);
        let mut before = State::default();
        before.on_hook(&hook("start"), &world);
        let mut second = hook("start");
        second.session_id = "two".to_string();
        second.process = Some(Process {
            pid: 99,
            started: None,
        });
        before.on_hook(&second, &world);
        let saved = before.snapshot();
        assert_eq!(saved.len(), 2);

        let mut after = State::default();
        after.restore(saved, &world);
        assert!(after.registry().get(&session("one")).is_some());
        // Pid 99 never ran, so nothing can vouch for the record that names it.
        // The daemon does not bring it back.
        assert!(after.registry().get(&session("two")).is_none());
    }

    #[test]
    fn what_a_restart_keeps_is_still_watched() {
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        world.running(AGENT);
        let mut before = State::default();
        before.on_hook(&hook("start"), &world);

        let mut after = State::default();
        after.restore(before.snapshot(), &world);
        world.says("/t/one.jsonl", titled("renamed"), 2);
        assert!(after.poll(&world));
        assert_eq!(
            after.registry().get(&session("one")).unwrap().meta.title,
            "renamed"
        );
    }
}
