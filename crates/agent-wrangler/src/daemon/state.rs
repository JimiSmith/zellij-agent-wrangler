//! Everything the daemon knows, and what each event does to it.
//!
//! The reading and the clock come only from [`World`]. Every rule here runs
//! against a fake with no files, no processes and no waiting. Only the reading
//! itself is left in the real implementation.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use agent_wrangler_core::agent::{self, Agent, LabelFacts, Process, SessionId, Turn};
use agent_wrangler_core::label::{label, Label};
use agent_wrangler_core::notify::Notifier;
use agent_wrangler_core::origin::Origin;
use agent_wrangler_core::payload::directory_name;
use agent_wrangler_core::registry::Registry;
use agent_wrangler_core::session_facts::{self, SessionFacts};

use crate::proto::{DeliveryTarget, Hook};

/// Where a session's own account of itself is kept. The daemon can read it
/// again without a new message from the agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionFiles {
    /// Which agent this is. The agent kind decides how the daemon reads its
    /// files.
    pub agent: String,
    /// The file the agent writes the conversation to. For a child this is the
    /// child's own file, and not the file of its lead.
    pub transcript: String,
    /// The file that names a child, beside that child's transcript, or `None`
    /// for a session. A child transcript carries no name and no title, so the
    /// daemon reads both from this file.
    #[serde(default)]
    pub child_meta_file: Option<String>,
    /// The modification time last read from it. If a file did not move, it
    /// needs no second look. A look at every session then costs one stat for
    /// each session rather than one scan for each session.
    pub mtime: Option<u64>,
}

/// The reading, the clock and the process table, behind one seam.
pub trait World {
    /// What an agent's own files say about this session: what it is called by,
    /// and what it works with. `child_meta_file` is set only for a child, whose
    /// transcript names it not at all.
    fn read_session_files(
        &self,
        agent: &str,
        transcript: &str,
        child_meta_file: Option<&str>,
        session: &str,
    ) -> SessionFacts;
    /// When a file last changed, or `None` for one that is not there.
    fn mtime(&self, path: &str) -> Option<u64>;
    /// Whether a process still runs, and is still the intended one.
    fn alive(&self, process: &Process) -> bool;
}

/// The real one: an agent's files, the filesystem, and this machine's processes.
pub struct Real;

impl World for Real {
    fn read_session_files(
        &self,
        agent: &str,
        transcript: &str,
        child_meta_file: Option<&str>,
        session: &str,
    ) -> SessionFacts {
        match (agent, child_meta_file) {
            ("claude", Some(meta_file)) => {
                session_facts::read_claude_child(&session_facts::ChildPaths {
                    transcript: transcript.to_string(),
                    meta_file: meta_file.to_string(),
                })
            }
            ("claude", None) => session_facts::read_claude_session(transcript),
            ("copilot", _) => match crate::paths::home() {
                Some(home) => session_facts::read_copilot_session(&home, session),
                None => SessionFacts::default(),
            },
            _ => SessionFacts::default(),
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
    /// A child of the session is over.
    ///
    /// This word ends a child alone, and it ends nothing when a hook names no
    /// child.
    ///
    /// The two events do not share one word, because they fail at different
    /// prices. The `end` word ends the session that a hook names, so a
    /// `SubagentStop` body with no agent id takes a lead that still runs. This
    /// word takes nothing in the same case, and it costs one stale row.
    ChildEnd,
    Turn(Turn),
}

pub fn event(agent: &str, name: &str, recoverable: Option<bool>) -> Event {
    match name {
        "end" => Event::End,
        "childEnd" => Event::ChildEnd,
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
    pub sink: DeliveryTarget,
    pub notify: Option<Notifier>,
}

/// Every agent session the daemon holds, and every client it delivers to.
#[derive(Debug, Default)]
pub struct State {
    registry: Registry,
    /// One per session held, so the daemon can read a transcript again between
    /// events.
    session_files: BTreeMap<SessionId, SessionFiles>,
    /// Where to deliver, newest last. A client that registers twice is one
    /// client, so this list never holds the same sink twice.
    clients: Vec<Client>,
    /// When each client last said anything at all.
    ///
    /// A client that says nothing for [`SILENCE`] is a client that the daemon
    /// gives up on. This map holds one entry for each client, and never an
    /// entry for a client that left.
    spoke: BTreeMap<DeliveryTarget, Instant>,
}

/// How long a client may say nothing before the daemon gives up on it.
///
/// A client answers one question by speaking: can it still send a message? An
/// open connection does not answer that question. It says that the kernel kept
/// the connection, and says nothing about the process behind it.
///
/// This is three times [`HEARTBEAT_INTERVAL`], so two lost beats retire nobody.
/// A client that is retired goes deaf for good, because it registers once. So
/// this must also cover a sidebar restarting, and a daemon restarting with its
/// clients connecting to it again. The tmux client bounds its reconnect at
/// about two seconds and holds a test on that bound.
///
/// [`HEARTBEAT_INTERVAL`]: agent_wrangler_core::client_message::HEARTBEAT_INTERVAL
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
    facts: SessionFacts,
    mtime: Option<u64>,
    /// The files that the daemon watches for this agent, already worked out
    /// from what the hook named. A child names its own pair, and not the file
    /// of its lead.
    files: SessionFiles,
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
    /// Session, and the files to read for it.
    pub watch: Vec<(SessionId, SessionFiles)>,
    /// Session, and the process that is said to run it.
    pub processes: Vec<(SessionId, Process)>,
}

/// What the look found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// Session, the transcript's new modification time, and what it now says.
    pub moved: Vec<(SessionId, u64, SessionFacts)>,
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
            .filter_map(|(session, files)| {
                let mtime = world.mtime(&files.transcript)?;
                if since.get(session).copied().flatten() == Some(mtime) {
                    return None;
                }
                Some((
                    session.clone(),
                    mtime,
                    world.read_session_files(
                        &files.agent,
                        &files.transcript,
                        files.child_meta_file.as_deref(),
                        session.as_str(),
                    ),
                ))
            })
            .collect(),
    }
}

/// Which agent a hook speaks for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReportedAgent {
    /// The session that the daemon files this hook under.
    session: SessionId,
    /// The session that started it, for a hook that fired inside a child.
    lead: Option<SessionId>,
}

/// The agent id that a hook names, for a hook that fired inside a child.
///
/// Only Claude is read this way. Copilot has subagent events of its own, and
/// refinement measured none of them, so a Copilot hook always speaks for its
/// own session. The daemon already reads an agent's files by the kind of agent,
/// and this is the same decision in the same place.
fn claude_child_of(hook: &Hook) -> Option<&str> {
    match hook.agent.as_str() {
        "claude" => hook.agent_id.as_deref().filter(|id| !id.is_empty()),
        _ => None,
    }
}

/// Which agent a hook speaks for, worked out from what it named.
///
/// Claude gives a subagent and a teammate no session id of their own, so a hook
/// inside either carries the id of the lead and names the child beside it. The
/// daemon composes an id for the child out of the pair.
fn reported_agent(hook: &Hook) -> Option<ReportedAgent> {
    let lead = SessionId::new(&hook.session_id)?;
    match claude_child_of(hook).and_then(|id| SessionId::child_of(&lead, id)) {
        Some(child) => Some(ReportedAgent {
            session: child,
            lead: Some(lead),
        }),
        None => Some(ReportedAgent {
            session: lead,
            lead: None,
        }),
    }
}

/// The files that a hook names, for the agent that the hook speaks for.
///
/// A hook inside a child names the transcript of the lead, and never the file of
/// the child. The daemon therefore builds the child's own pair of files from
/// that path and the agent id.
fn files_named_by(hook: &Hook) -> SessionFiles {
    let child = claude_child_of(hook)
        .and_then(|agent_id| session_facts::claude_child_paths(&hook.transcript, agent_id));
    match child {
        Some(paths) => SessionFiles {
            agent: hook.agent.clone(),
            transcript: paths.transcript,
            child_meta_file: Some(paths.meta_file),
            mtime: None,
        },
        None => SessionFiles {
            agent: hook.agent.clone(),
            transcript: hook.transcript.clone(),
            child_meta_file: None,
            mtime: None,
        },
    }
}

/// This function reads what a hook named. It touches the filesystem and
/// nothing else.
pub fn read_hook(hook: &Hook, world: &dyn World) -> Reading {
    let files = files_named_by(hook);
    Reading {
        facts: world.read_session_files(
            &files.agent,
            &files.transcript,
            files.child_meta_file.as_deref(),
            &hook.session_id,
        ),
        mtime: world.mtime(&files.transcript),
        files,
    }
}

impl State {
    /// This method takes in what a hook reported, with what its files already
    /// said.
    pub fn apply_hook(&mut self, hook: &Hook, reading: Reading) -> Applied {
        let Some(reported) = reported_agent(hook) else {
            return Applied::Nothing;
        };
        let session = reported.session.clone();
        let event = event(&hook.agent, &hook.event, hook.recoverable);
        match event {
            Event::End => return Applied::told(self.end_session(&session)),
            // A hook that names no child ends nothing here. A lead is therefore
            // safe from the stop event of its own child.
            Event::ChildEnd if reported.lead.is_none() => return Applied::Nothing,
            Event::ChildEnd => return Applied::told(self.end_session(&session)),
            _ => {}
        }

        let meta = LabelFacts {
            dir: directory_name(&hook.cwd),
            ..reading.facts.label
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
            lead: reported.lead,
            ..Agent::new(
                session.clone(),
                &hook.agent,
                meta,
                Origin::decode(&hook.origin),
            )
        }
        .with_status(reading.facts.status)
        .with_records(reading.facts.records);

        self.session_files.insert(
            session.clone(),
            SessionFiles {
                mtime: reading.mtime,
                ..reading.files
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
    pub fn spoke(&mut self, sink: &DeliveryTarget, now: Instant) {
        if self.clients.iter().any(|held| &held.sink == sink) {
            self.spoke.insert(sink.clone(), now);
        }
    }

    /// Every client that has said nothing for [`SILENCE`].
    pub fn silent(&self, now: Instant) -> Vec<DeliveryTarget> {
        self.spoke
            .iter()
            .filter(|(_, spoke)| now.duration_since(**spoke) >= SILENCE)
            .map(|(sink, _)| sink.clone())
            .collect()
    }

    /// This method gives up on one client, whatever the daemon knew about it.
    /// It returns whether there was one to give up on.
    pub fn retire(&mut self, sink: &DeliveryTarget) -> bool {
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
        agent::build_state_message(&self.registry.encode())
    }

    /// Every session held, for a report of what is there rather than for a
    /// change to it.
    #[cfg(test)]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Drop a session, and drop the files of every child that goes with it.
    ///
    /// A child runs inside the process of its lead, so a lead that leaves takes
    /// its children with it. [`Registry::end`] drops the records. This method
    /// drops the files that the daemon watches for them, which the registry
    /// knows nothing about.
    fn end_session(&mut self, session: &SessionId) -> bool {
        for child in self.registry.children_of(session) {
            self.session_files.remove(&child);
        }
        self.session_files.remove(session);
        self.registry.end(session)
    }

    /// What the next look must cover: the transcripts held, and the processes
    /// to ask after. This method reads nothing.
    pub fn plan(&self) -> Plan {
        Plan {
            watch: self
                .session_files
                .iter()
                .map(|(session, files)| (session.clone(), files.clone()))
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

        // An agent whose process went away is gone, whatever it last said. A
        // child shares the process of its lead, so both go at once.
        for session in look.dead {
            changed |= self.end_session(&session);
        }

        for (session, mtime, found) in look.moved {
            // A session that ended during the look is not one to bring back.
            let Some(files) = self.session_files.get_mut(&session) else {
                continue;
            };
            files.mtime = Some(mtime);
            let Some(held) = self.registry.get(&session).cloned() else {
                continue;
            };
            // The directory is not in the transcript. The daemon keeps it from
            // what the last hook said, because a scan never looks for it. Every
            // other fact is in the transcript, so the scan states all of them
            // afresh.
            //
            // The lead comes from the hook and from no file at all, so it rides
            // along with the rest of the held record and a scan never touches
            // it.
            let record = Agent {
                meta: LabelFacts {
                    dir: held.meta.dir.clone(),
                    ..found.label
                },
                ..held
            }
            .with_status(found.status)
            .with_records(found.records);
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
        self.session_files
            .iter()
            .map(|(session, files)| (session.clone(), files.mtime))
            .collect()
    }

    /// The state as it is kept between runs: every session, with the file that
    /// the daemon reads its account of itself from.
    pub fn snapshot(&self) -> Vec<(String, SessionFiles)> {
        self.registry
            .iter()
            .map(|agent| {
                let files = self
                    .session_files
                    .get(&agent.session)
                    .cloned()
                    .unwrap_or_default();
                (agent.encode(), files)
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
    pub fn restore(&mut self, saved: Vec<(String, SessionFiles)>, world: &dyn World) {
        for (line, files) in saved {
            let agent_wrangler_core::agent::Record::Known(agent) = Agent::decode(&line) else {
                continue;
            };
            match agent.process {
                Some(process) if world.alive(&process) => {}
                _ => continue,
            }
            self.session_files.insert(agent.session.clone(), files);
            self.registry.report(agent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::client_message::HEARTBEAT_INTERVAL;
    use std::cell::RefCell;

    use agent_wrangler_core::agent::{ProcessStartStamp, TranscriptRecords};

    /// A world with no files and no processes. A test states three things
    /// outright: what a transcript says, the time it last changed, and what
    /// runs under each pid.
    ///
    /// A pid maps to the process that holds it now. A test can therefore state
    /// that one number went to another process.
    #[derive(Default)]
    struct Fake {
        facts: RefCell<BTreeMap<String, SessionFacts>>,
        mtime: RefCell<BTreeMap<String, u64>>,
        alive: RefCell<BTreeMap<u32, Option<ProcessStartStamp>>>,
    }

    impl Fake {
        fn says(&self, transcript: &str, label: LabelFacts, mtime: u64) {
            self.reads(
                transcript,
                SessionFacts {
                    label,
                    ..SessionFacts::default()
                },
                mtime,
            );
        }

        fn reads(&self, transcript: &str, facts: SessionFacts, mtime: u64) {
            self.facts
                .borrow_mut()
                .insert(transcript.to_string(), facts);
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
        fn read_session_files(
            &self,
            _agent: &str,
            transcript: &str,
            _child_meta_file: Option<&str>,
            _session: &str,
        ) -> SessionFacts {
            self.facts
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
        started: Some(ProcessStartStamp(918_273)),
    };

    fn titled(title: &str) -> LabelFacts {
        LabelFacts {
            title: title.to_string(),
            ..LabelFacts::default()
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
            agent_id: None,
            agent_type: None,
            origin: Origin::from_lookup(|name| match name {
                "ZELLIJ_SESSION_NAME" => Some("proto".to_string()),
                "ZELLIJ_PANE_ID" => Some("7".to_string()),
                _ => None,
            })
            .encode(),
            process: Some(AGENT),
            at: 1000,
        }
    }

    /// The same hook, fired inside a child of the session.
    ///
    /// Measured on Claude Code 2.1.258. A subagent and a teammate both carry
    /// these two fields, and session_id still names the lead.
    fn child_hook(event: &str, agent_id: &str) -> Hook {
        Hook {
            agent_id: Some(agent_id.to_string()),
            agent_type: Some("Explore".to_string()),
            ..hook(event)
        }
    }

    /// The id that the daemon files a child of session `one` under.
    fn child(agent_id: &str) -> SessionId {
        SessionId::child_of(&session("one"), agent_id).unwrap()
    }

    fn session(text: &str) -> SessionId {
        SessionId::new(text).unwrap()
    }

    fn notifier(program: &str) -> Option<Notifier> {
        Notifier::new(vec![program.to_string()])
    }

    fn client(session: &str, notify: Option<Notifier>) -> Client {
        Client {
            sink: DeliveryTarget::Zellij {
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
    fn the_child_end_word_is_its_own_event() {
        assert_eq!(event("claude", "childEnd", None), Event::ChildEnd);
    }

    #[test]
    fn a_hook_that_names_a_child_files_the_child_under_the_lead() {
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("working"), &world);
        state.on_hook(&child_hook("working", "a9a352"), &world);

        let filed = state.registry().get(&child("a9a352")).unwrap();
        assert_eq!(filed.session, child("a9a352"));
        assert_eq!(filed.lead, Some(session("one")));
        // The lead is still there, and it leads nobody itself.
        assert_eq!(state.registry().get(&session("one")).unwrap().lead, None);
    }

    #[test]
    fn a_hook_that_names_no_child_files_the_session_as_it_does_today() {
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("working"), &world);
        assert_eq!(state.registry().get(&session("one")).unwrap().lead, None);
    }

    #[test]
    fn a_lead_keeps_its_own_turn_while_a_child_works() {
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        // The user submits a prompt, so the lead is working.
        state.on_hook(&hook("working"), &world);
        // The child then raises a call. The lead must not take that turn.
        state.on_hook(&child_hook("needsAttention", "a9a352"), &world);

        assert_eq!(
            state.registry().get(&session("one")).unwrap().turn,
            Turn::Working
        );
        assert_eq!(
            state.registry().get(&child("a9a352")).unwrap().turn,
            Turn::Attention
        );
    }

    #[test]
    fn the_child_end_word_ends_a_child() {
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("working"), &world);
        state.on_hook(&child_hook("working", "a9a352"), &world);
        assert!(state.registry().get(&child("a9a352")).is_some());

        state.on_hook(&child_hook("childEnd", "a9a352"), &world);
        assert_eq!(state.registry().get(&child("a9a352")), None);
        // The lead runs on.
        assert!(state.registry().get(&session("one")).is_some());
    }

    #[test]
    fn the_child_end_word_ends_nothing_when_a_hook_names_no_child() {
        // A SubagentStop body that carried no agent id must never end the lead.
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("working"), &world);
        assert_eq!(state.on_hook(&hook("childEnd"), &world), Applied::Nothing);
        assert!(state.registry().get(&session("one")).is_some());
    }

    #[test]
    fn a_session_that_ends_takes_every_child_under_it() {
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("working"), &world);
        state.on_hook(&child_hook("working", "a9a352"), &world);
        state.on_hook(&child_hook("working", "ab1234"), &world);

        state.on_hook(&hook("end"), &world);
        assert_eq!(state.registry().get(&session("one")), None);
        assert_eq!(state.registry().get(&child("a9a352")), None);
        assert_eq!(state.registry().get(&child("ab1234")), None);
        // Nothing is watched for an agent that left.
        assert!(state.plan().watch.is_empty());
    }

    #[test]
    fn a_dead_lead_takes_every_child_under_it() {
        // A child runs inside the process of its lead, so one process check
        // reaps both.
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("working"), &world);
        state.on_hook(&child_hook("working", "a9a352"), &world);

        world.killed(AGENT.pid);
        assert!(state.poll(&world));
        assert_eq!(state.registry().get(&session("one")), None);
        assert_eq!(state.registry().get(&child("a9a352")), None);
    }

    #[test]
    fn a_child_reads_its_facts_from_its_own_transcript() {
        // The hook names the transcript of the lead. The daemon reads the file
        // of the child, which sits under a directory named for the lead.
        let world = Fake::default();
        world.running(AGENT);
        world.says("/t/one.jsonl", titled("the lead"), 1);
        world.says(
            "/t/one/subagents/agent-a9a352.jsonl",
            titled("the child"),
            1,
        );

        let mut state = State::default();
        state.on_hook(&child_hook("working", "a9a352"), &world);
        assert_eq!(
            state.registry().get(&child("a9a352")).unwrap().meta.title,
            "the child"
        );
    }

    #[test]
    fn a_child_whose_transcript_appears_later_is_read_on_the_next_tick() {
        // A child writes no transcript until it answers, so SubagentStart finds
        // no file at all.
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&child_hook("working", "a9a352"), &world);
        assert_eq!(
            state.registry().get(&child("a9a352")).unwrap().meta.title,
            ""
        );

        world.says("/t/one/subagents/agent-a9a352.jsonl", titled("at last"), 1);
        assert!(state.poll(&world));
        assert_eq!(
            state.registry().get(&child("a9a352")).unwrap().meta.title,
            "at last"
        );
    }

    #[test]
    fn a_copilot_hook_that_names_an_agent_files_the_session() {
        // Claude is the priority. Copilot has subagent events of its own, and
        // refinement measured none of them.
        let world = Fake::default();
        world.running(AGENT);
        let mut state = State::default();
        let copilot = Hook {
            agent: "copilot".to_string(),
            ..child_hook("working", "a9a352")
        };
        state.on_hook(&copilot, &world);
        assert!(state.registry().get(&session("one")).is_some());
        assert_eq!(state.registry().get(&child("a9a352")), None);
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
        assert!(SILENCE >= HEARTBEAT_INTERVAL * 3);
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
        world.says("/t/one.jsonl", LabelFacts::default(), 2);
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
        world.says("/t/one.jsonl", LabelFacts::default(), 1);
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

    fn works_with(branch: &str, model: &str, context_tokens: u64) -> SessionFacts {
        SessionFacts {
            status: agent_wrangler_core::agent::StatusFacts {
                branch: branch.to_string(),
                model: model.to_string(),
                context_tokens,
            },
            ..SessionFacts::default()
        }
    }

    /// Facts holding the two transcript records alone: what the session last
    /// said, and the tool that runs now.
    fn facts_with_records(last_message: &str, running_tool: &str) -> SessionFacts {
        SessionFacts {
            records: TranscriptRecords {
                last_message: last_message.to_string(),
                running_tool: running_tool.to_string(),
            },
            ..SessionFacts::default()
        }
    }

    #[test]
    fn a_hook_files_what_the_session_works_with() {
        let world = Fake::default();
        world.reads("/t/one.jsonl", works_with("main", "claude-opus-5", 4200), 1);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        let held = state.registry().get(&session("one")).unwrap();
        assert_eq!(held.status.branch, "main");
        assert_eq!(held.status.model, "claude-opus-5");
        assert_eq!(held.status.context_tokens, 4200);
    }

    #[test]
    fn watching_states_afresh_what_the_session_works_with() {
        // A branch changes, and a turn spends more of the window, with no hook
        // between. The watch carries both to a row.
        let world = Fake::default();
        world.reads("/t/one.jsonl", works_with("main", "claude-opus-5", 4200), 1);
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);

        world.reads(
            "/t/one.jsonl",
            works_with("daemon", "claude-opus-5", 91_000),
            2,
        );
        assert!(state.poll(&world));
        let held = state.registry().get(&session("one")).unwrap();
        assert_eq!(held.status.branch, "daemon");
        assert_eq!(held.status.context_tokens, 91_000);
    }

    #[test]
    fn a_hook_files_what_the_session_last_said_and_what_it_runs() {
        let world = Fake::default();
        world.reads("/t/one.jsonl", facts_with_records("{said}", "{running}"), 1);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);
        let held = state.registry().get(&session("one")).unwrap();
        assert_eq!(held.records.last_message, "{said}");
        assert_eq!(held.records.running_tool, "{running}");
    }

    #[test]
    fn watching_states_afresh_what_the_session_last_said() {
        // An agent answers with no hook between, and the watch carries the new
        // answer to a client.
        let world = Fake::default();
        world.reads("/t/one.jsonl", facts_with_records("{first}", ""), 1);
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);

        world.reads("/t/one.jsonl", facts_with_records("{second}", ""), 2);
        assert!(state.poll(&world));
        let held = state.registry().get(&session("one")).unwrap();
        assert_eq!(held.records.last_message, "{second}");
    }

    #[test]
    fn a_tool_that_finished_leaves_the_record_that_named_it() {
        // The scan states every transcript fact afresh. A record that kept the
        // tool call would draw a tool that stopped running.
        let world = Fake::default();
        world.reads("/t/one.jsonl", facts_with_records("{said}", "{running}"), 1);
        world.running(AGENT);
        let mut state = State::default();
        state.on_hook(&hook("start"), &world);

        world.reads("/t/one.jsonl", facts_with_records("{said}", ""), 2);
        assert!(state.poll(&world));
        let held = state.registry().get(&session("one")).unwrap();
        assert_eq!(held.records.running_tool, "");
        assert_eq!(held.records.last_message, "{said}");
    }

    #[test]
    fn watching_keeps_the_directory_no_transcript_mentions() {
        let world = Fake::default();
        world.says("/t/one.jsonl", LabelFacts::default(), 1);
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
            started: Some(ProcessStartStamp(999_999)),
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
