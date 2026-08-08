//! Everything the daemon knows, and what each event does to it.
//!
//! The reading and the clock are reached only through [`World`], so every rule
//! here is exercised against a fake with no files, no processes and no waiting.
//! What is left in the real implementation is the reading itself.

use std::collections::BTreeMap;
use std::path::Path;

use agent_wrangler_core::agent::{self, Agent, Meta, SessionId, Turn};
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
    /// Whether a process is still running.
    fn alive(&self, pid: u32) -> bool;
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

    fn alive(&self, pid: u32) -> bool {
        crate::platform::pid_alive(pid)
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

/// Every agent session the daemon holds, and every client it delivers to.
#[derive(Debug, Default)]
pub struct State {
    registry: Registry,
    /// One per session held, so a transcript can be read again between events.
    sources: BTreeMap<SessionId, Source>,
    /// Where to deliver, newest last. A client that registers twice is one
    /// client, so the same sink is never held twice.
    sinks: Vec<Sink>,
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

/// What the next look should cover.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Session, agent kind, transcript path.
    pub watch: Vec<(SessionId, String, String)>,
    /// Session and the process said to be running it.
    pub pids: Vec<(SessionId, u32)>,
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
            .pids
            .iter()
            .filter(|(_, pid)| !world.alive(*pid))
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
    /// Take in what a hook reported, given what its files already said. `true`
    /// when this changed anything a client would draw.
    pub fn apply_hook(&mut self, hook: &Hook, reading: Reading) -> bool {
        let Some(session) = SessionId::new(&hook.session_id) else {
            return false;
        };
        let event = event(&hook.agent, &hook.event, hook.recoverable);
        if event == Event::End {
            self.sources.remove(&session);
            return self.registry.end(&session);
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
            pid: hook.pid,
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
            session,
            Source {
                agent: hook.agent.clone(),
                transcript: hook.transcript.clone(),
                mtime: reading.mtime,
            },
        );

        match event {
            Event::Announce => self.registry.start(record),
            _ => self.registry.report(record),
        }
    }

    /// Read what the hook named and file it, in one step.
    ///
    /// The daemon does the two halves separately so the reading happens with
    /// nothing locked. This is the same two calls with nothing in between, for
    /// tests that are about what a hook does rather than about when it is safe
    /// to do it.
    #[cfg(test)]
    pub fn on_hook(&mut self, hook: &Hook, world: &dyn World) -> bool {
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
    /// one client.
    pub fn register(&mut self, sink: Sink) {
        self.misses.remove(&sink);
        if !self.sinks.contains(&sink) {
            self.sinks.push(sink);
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
        self.sinks.retain(|held| held != sink);
        self.misses.remove(sink);
        true
    }

    /// Note that a client was reached, so the refusals before it do not count
    /// towards giving up on it.
    pub fn reached(&mut self, sink: &Sink) {
        self.misses.remove(sink);
    }

    pub fn sinks(&self) -> Vec<Sink> {
        self.sinks.clone()
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
            pids: self
                .registry
                .iter()
                .filter_map(|agent| agent.pid.map(|pid| (agent.session.clone(), pid)))
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
            match agent.pid {
                Some(pid) if world.alive(pid) => {}
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
    use std::collections::HashSet;

    /// A world with no files and no processes: what a transcript would say, when
    /// it last changed, and which pids are running, all said outright.
    #[derive(Default)]
    struct Fake {
        meta: RefCell<BTreeMap<String, Meta>>,
        mtime: RefCell<BTreeMap<String, u64>>,
        alive: RefCell<HashSet<u32>>,
    }

    impl Fake {
        fn says(&self, transcript: &str, meta: Meta, mtime: u64) {
            self.meta.borrow_mut().insert(transcript.to_string(), meta);
            self.mtime
                .borrow_mut()
                .insert(transcript.to_string(), mtime);
        }

        fn running(&self, pid: u32) {
            self.alive.borrow_mut().insert(pid);
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

        fn alive(&self, pid: u32) -> bool {
            self.alive.borrow().contains(&pid)
        }
    }

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
            pid: Some(4242),
            at: 1000,
        }
    }

    fn session(text: &str) -> SessionId {
        SessionId::new(text).unwrap()
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
        assert!(state.on_hook(&hook("start"), &world));
        let held = state.registry().get(&session("one")).unwrap();
        assert_eq!(held.meta.title, "the port");
        assert_eq!(held.meta.dir, "wrangler");
        assert_eq!(held.origin.get("ZELLIJ_PANE_ID"), Some("7"));
        assert_eq!(held.pid, Some(4242));
    }

    #[test]
    fn an_event_naming_no_session_is_nothing_to_file() {
        let world = Fake::default();
        let mut state = State::default();
        let mut nameless = hook("start");
        nameless.session_id = String::new();
        assert!(!state.on_hook(&nameless, &world));
        assert!(state.registry().is_empty());
    }

    #[test]
    fn the_end_of_a_session_takes_it_away() {
        let world = Fake::default();
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        assert!(state.on_hook(&hook("end"), &world));
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
        let sink = Sink::Zellij {
            session: "proto".to_string(),
        };
        state.register(sink.clone());
        state.register(sink.clone());
        assert_eq!(state.sinks(), vec![sink]);
    }

    #[test]
    fn a_client_is_given_up_on_only_after_refusing_repeatedly() {
        let mut state = State::default();
        let sink = Sink::Zellij {
            session: "proto".to_string(),
        };
        state.register(sink.clone());
        for _ in 1..REFUSALS {
            assert!(!state.missed(&sink));
            assert_eq!(state.sinks(), vec![sink.clone()]);
        }
        assert!(state.missed(&sink));
        assert!(state.sinks().is_empty());
    }

    #[test]
    fn reaching_a_client_forgives_the_times_it_was_missed() {
        let mut state = State::default();
        let sink = Sink::Zellij {
            session: "proto".to_string(),
        };
        state.register(sink.clone());
        for _ in 1..REFUSALS {
            state.missed(&sink);
        }
        state.reached(&sink);
        // The count starts again, so a client that answers now and then is
        // never retired by refusals spread over hours.
        for _ in 1..REFUSALS {
            assert!(!state.missed(&sink));
        }
        assert_eq!(state.sinks(), vec![sink]);
    }

    #[test]
    fn registering_again_forgives_the_times_a_client_was_missed() {
        let mut state = State::default();
        let sink = Sink::Zellij {
            session: "proto".to_string(),
        };
        state.register(sink.clone());
        for _ in 1..REFUSALS {
            state.missed(&sink);
        }
        state.register(sink.clone());
        assert!(!state.missed(&sink));
        assert_eq!(state.sinks(), vec![sink]);
    }

    #[test]
    fn a_transcript_that_has_moved_is_read_again() {
        // The whole point of watching: a session takes a title, or is given a
        // color, without any hook firing at all.
        let world = Fake::default();
        world.says("/t/one.jsonl", Meta::default(), 1);
        world.running(4242);
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
        world.running(4242);
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
        world.running(4242);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        assert!(!state.poll(&world));

        world.killed(4242);
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
        anonymous.pid = None;
        state.on_hook(&anonymous, &world);
        assert!(!state.poll(&world));
        assert!(!state.registry().is_empty());
    }

    #[test]
    fn a_restart_keeps_only_what_is_still_running() {
        let world = Fake::default();
        world.says("/t/one.jsonl", titled("the port"), 1);
        world.running(4242);
        let mut before = State::default();
        before.on_hook(&hook("start"), &world);
        let mut second = hook("start");
        second.session_id = "two".to_string();
        second.pid = Some(99);
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
        world.running(4242);
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
