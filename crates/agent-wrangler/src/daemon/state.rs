//! Everything the daemon knows, and what each event does to it.
//!
//! The reading and the clock are reached only through [`World`], so every rule
//! here is exercised against a fake with no files, no processes and no waiting.
//! What is left in the real implementation is the reading itself.

use std::collections::BTreeMap;
use std::path::Path;

use agent_wrangler_core::agent::{self, Agent, Meta, Process, SessionId, Turn};
use agent_wrangler_core::label::{label, Label};
use agent_wrangler_core::notify::Notifier;
use agent_wrangler_core::origin::Origin;
use agent_wrangler_core::payload::dir;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_core::titles;

use crate::proto::{Hook, Sink};

/// Where a session's own account of itself is kept, so it can be read again
/// without waiting for the agent to say something.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    /// Which agent this is, which decides how its files are read.
    pub agent: String,
    /// The file the agent writes the conversation to.
    pub transcript: String,
    /// The modification time last read from it. A file that has not moved needs
    /// no second look, which is what makes watching every session cost one stat
    /// each rather than one scan each.
    pub mtime: Option<u64>,
}

/// The reading, the clock and the process table, behind one seam.
pub trait World {
    /// What an agent's own files say this session is called.
    fn meta(&self, agent: &str, transcript: &str, session: &str) -> Meta;
    /// When a file last changed, or `None` for one that is not there.
    fn mtime(&self, path: &str) -> Option<u64>;
    /// Whether a process is still running, and still the one that was meant.
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
/// An error is the one event whose meaning depends on who raised it: Copilot
/// says whether it can carry on, and one that can is still working rather than
/// waiting. Every other agent's error is something the user has to look at.
/// Anything unrecognised is a session announcing itself, which is also how a
/// session already known re-states where it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The session is starting, or restating itself. Whose turn it is is not
    /// part of what it said.
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

/// One client: where to reach it, and what it would have a call for the user
/// announced with.
///
/// A client says what to announce with rather than announcing anything itself.
/// Every client is handed the same state, so a client that raised its own
/// notifications would raise each call as many times as there are clients
/// holding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    pub sink: Sink,
    pub notify: Option<Notifier>,
}

/// Every agent session the daemon holds, and every client it delivers to.
#[derive(Debug, Default)]
pub struct State {
    registry: Registry,
    /// One per session held, so a transcript can be read again between events.
    sources: BTreeMap<SessionId, Source>,
    /// Where to deliver, newest last. A client that registers twice is one
    /// client, so the same sink is never held twice.
    clients: Vec<Client>,
    /// How many deliveries in a row each client has refused.
    misses: BTreeMap<Sink, u32>,
}

/// How many refusals in a row retire a client.
///
/// One is not enough. A client registers once and has no way of knowing it has
/// been dropped, so a single delivery that failed for a passing reason - the
/// multiplexer busy, a program missing from the path this daemon happened to
/// inherit - would leave that sidebar drawing whatever it last received for
/// good, with nothing said about why.
const REFUSALS: u32 = 3;

/// What an agent's files said, read before anything is locked.
///
/// Reading is separated from filing because a file can take arbitrarily long to
/// open: a hung network mount, a dead sshfs, a named pipe with no writer. Doing
/// it while holding the state would stop every other event on the machine being
/// recorded, and the daemon answers its socket while frozen, so nothing could
/// take over from it either.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    meta: Meta,
    mtime: Option<u64>,
}

/// A call for the user, in the words an announcement is made of.
///
/// It says which agent is asking and which of its sessions, and nothing about
/// where that session is: where an agent is drawn is the business of whatever
/// draws it, and one that is in no multiplexer at all still calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub agent: String,
    pub label: String,
}

/// What taking an event in came to.
///
/// A call is a change as well as an announcement, so the two cannot be reported
/// separately without letting a call be announced that nothing was told about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The event said nothing that is not already held.
    Nothing,
    /// What is held changed, so every client is owed the state.
    Changed,
    /// The change was an agent asking for the user, which is worth saying out
    /// loud as well as drawing.
    Called(Call),
}

impl Applied {
    /// What a change is when nothing about it is worth announcing.
    fn told(changed: bool) -> Self {
        match changed {
            true => Applied::Changed,
            false => Applied::Nothing,
        }
    }

    /// Whether a client would draw anything differently for this.
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

/// What the next look should cover.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Session, agent kind, transcript path.
    pub watch: Vec<(SessionId, String, String)>,
    /// Session and the process said to be running it.
    pub processes: Vec<(SessionId, Process)>,
}

/// What the look found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// Session, the transcript's new modification time, and what it now says.
    pub moved: Vec<(SessionId, u64, Meta)>,
    /// Sessions whose process is no longer running.
    pub dead: Vec<SessionId>,
}

/// Carry out a plan. Touches the filesystem and the process table, and holds
/// nothing, so it is safe to do this while every other event carries on.
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

/// Read what a hook named. Touches the filesystem and nothing else.
pub fn read_hook(hook: &Hook, world: &dyn World) -> Reading {
    Reading {
        meta: world.meta(&hook.agent, &hook.transcript, &hook.session_id),
        mtime: world.mtime(&hook.transcript),
    }
}

impl State {
    /// Take in what a hook reported, given what its files already said.
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
        // Only a call for the user carries when it was raised. Giving every
        // event a time would make two identical reports look different, and
        // reorder the notification area for nothing.
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
        // arrived, because a hook reports only what it could find: the title a
        // session took two events ago is part of what it is called now.
        //
        // A hook that told nobody anything new is a call nobody has to hear
        // about, which is what keeps an agent restating where it is from
        // announcing itself over and over.
        match (changed, turn) {
            (true, Turn::Attention) => match self.registry.get(&session) {
                Some(filed) => Applied::Called(Call {
                    agent: filed.agent.clone(),
                    label: label(filed, Label::Name),
                }),
                None => Applied::Changed,
            },
            (changed, _) => Applied::told(changed),
        }
    }

    /// Read what the hook named and file it, in one step.
    ///
    /// The daemon does the two halves separately so the reading happens with
    /// nothing locked. This is the same two calls with nothing in between, for
    /// tests that are about what a hook does rather than about when it is safe
    /// to do it.
    #[cfg(test)]
    pub fn on_hook(&mut self, hook: &Hook, world: &dyn World) -> Applied {
        self.apply_hook(hook, read_hook(hook, world))
    }

    /// The user reached a session that was calling for them.
    pub fn on_seen(&mut self, session: &str) -> bool {
        match SessionId::new(session) {
            Some(session) => self.registry.seen(&session),
            None => false,
        }
    }

    /// Deliver to this client from now on. A client that says so twice is still
    /// one client, and says afresh what it would have a call announced with.
    pub fn register(&mut self, client: Client) {
        self.misses.remove(&client.sink);
        match self
            .clients
            .iter_mut()
            .find(|held| held.sink == client.sink)
        {
            Some(held) => *held = client,
            None => self.clients.push(client),
        }
    }

    /// Note that a client could not be reached, and say whether it has now been
    /// given up on.
    pub fn missed(&mut self, sink: &Sink) -> bool {
        let misses = self.misses.entry(sink.clone()).or_default();
        *misses += 1;
        if *misses < REFUSALS {
            return false;
        }
        self.clients.retain(|held| &held.sink != sink);
        self.misses.remove(sink);
        true
    }

    /// Note that a client was reached, so the refusals before it do not count
    /// towards giving up on it.
    pub fn reached(&mut self, sink: &Sink) {
        self.misses.remove(sink);
    }

    pub fn clients(&self) -> Vec<Client> {
        self.clients.clone()
    }

    /// What a call for the user is announced with, once each.
    ///
    /// A notifier is the user's rather than any one client's: two clients asking
    /// for the same one describe one desktop to tell, and telling it twice is
    /// the same notification twice. Two asking for different ones are two
    /// places to tell, and each is told.
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
    /// Holding no agents is sent exactly like holding some, because a client
    /// that cannot tell an empty state from an empty message has no way to
    /// ignore a message that was not this.
    pub fn payload(&self) -> String {
        agent::state(&self.registry.encode())
    }

    /// Every session held, for saying what is there rather than for changing
    /// it.
    #[cfg(test)]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// What the next look should cover: the transcripts held, and the processes
    /// to ask after. Reads nothing.
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

    /// Take in what the look found. `true` when this changed anything.
    ///
    /// This is the whole reason for a daemon rather than a hook that reports and
    /// exits. A session titles itself, or is given a color, without any hook
    /// firing at all, and an agent that is killed fires no end event; neither is
    /// visible to anything that only listens.
    pub fn observe(&mut self, look: Look) -> bool {
        let mut changed = false;

        // An agent whose process has gone is gone, whatever it last said.
        for session in look.dead {
            self.sources.remove(&session);
            changed |= self.registry.end(&session);
        }

        for (session, mtime, found) in look.moved {
            // A session that ended while the look was happening is not one to
            // bring back.
            let Some(source) = self.sources.get_mut(&session) else {
                continue;
            };
            source.mtime = Some(mtime);
            let Some(held) = self.registry.get(&session).cloned() else {
                continue;
            };
            // The directory is not in the transcript, so it is kept from what
            // the last hook said rather than blanked by a scan that never looks
            // for it.
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

    /// Plan, look and take in, in one step. Separate in the daemon for the same
    /// reason the hook path is.
    #[cfg(test)]
    pub fn poll(&mut self, world: &dyn World) -> bool {
        let plan = self.plan();
        let look = look(&plan, world, &self.mtimes());
        self.observe(look)
    }

    /// What each held transcript last read as, which is what tells a file that
    /// has moved from one that has not.
    pub fn mtimes(&self) -> BTreeMap<SessionId, Option<u64>> {
        self.sources
            .iter()
            .map(|(session, source)| (session.clone(), source.mtime))
            .collect()
    }

    /// The state as it is kept between runs: every session, with the file its
    /// account of itself is read from.
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

    /// Take back a snapshot, dropping every session that cannot be vouched for.
    ///
    /// A daemon that has restarted has no idea which of these are still running.
    /// A record naming a live process is kept; every other one is dropped,
    /// because a live agent says so again on its very next event of any kind,
    /// while a dead one would otherwise be drawn for good.
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

    /// A world with no files and no processes: what a transcript would say, when
    /// it last changed, and what is running under each pid, all said outright.
    ///
    /// A pid maps to the process currently holding it, so handing one number to
    /// another process is something the tests can say happened.
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

    /// The process the hooks in these tests report themselves as.
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
        // Saying it again is not a change, and a session nobody knows is not one
        // to answer.
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
        // A sidebar that was reloaded with the option turned off is the same
        // client saying something different, not a second one.
        let mut state = State::default();
        state.register(client("proto", notifier("notify-send")));
        assert_eq!(state.notifiers(), vec![notifier("notify-send").unwrap()]);
        state.register(client("proto", None));
        assert!(state.notifiers().is_empty());
    }

    #[test]
    fn a_call_is_announced_once_however_many_clients_ask_for_the_same_thing() {
        // The reason a client is not left to raise its own: every one of them
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
        state.register(client("proto", notifier("notify-send")));
        for _ in 1..REFUSALS {
            state.missed(&client("proto", None).sink);
        }
        assert!(state.missed(&client("proto", None).sink));
        assert!(state.notifiers().is_empty());
    }

    #[test]
    fn a_client_is_given_up_on_only_after_refusing_repeatedly() {
        let mut state = State::default();
        let one = client("proto", None);
        state.register(one.clone());
        for _ in 1..REFUSALS {
            assert!(!state.missed(&one.sink));
            assert_eq!(state.clients(), vec![one.clone()]);
        }
        assert!(state.missed(&one.sink));
        assert!(state.clients().is_empty());
    }

    #[test]
    fn reaching_a_client_forgives_the_times_it_was_missed() {
        let mut state = State::default();
        let one = client("proto", None);
        state.register(one.clone());
        for _ in 1..REFUSALS {
            state.missed(&one.sink);
        }
        state.reached(&one.sink);
        // The count starts again, so a client that answers now and then is
        // never retired by refusals spread over hours.
        for _ in 1..REFUSALS {
            assert!(!state.missed(&one.sink));
        }
        assert_eq!(state.clients(), vec![one]);
    }

    #[test]
    fn registering_again_forgives_the_times_a_client_was_missed() {
        let mut state = State::default();
        let one = client("proto", None);
        state.register(one.clone());
        for _ in 1..REFUSALS {
            state.missed(&one.sink);
        }
        state.register(one.clone());
        assert!(!state.missed(&one.sink));
        assert_eq!(state.clients(), vec![one]);
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
            })
        );
    }

    #[test]
    fn a_call_is_announced_by_what_the_session_is_called_now() {
        // A hook carries what it could find at the time, and a title found two
        // events ago is still what the session is called.
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
        // An agent restating a call it is already making is the same call, and
        // hearing about it twice is being told twice.
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
        // The whole point of watching: a session takes a title, or is given a
        // color, without any hook firing at all.
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
        // A file that has not moved since is not read again.
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
        // starts next, and a record that asked only after the number would be
        // told a stranger was its agent.
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
        // Nothing can vouch for it, so nothing can condemn it either; it goes
        // when its own end event says so.
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
        // Pid 99 was never running, so the record naming it cannot be vouched
        // for and is not brought back.
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
