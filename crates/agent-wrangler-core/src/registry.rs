//! Every agent session known about, with the newest state of each session.
//!
//! A record says everything there is to say about a session. The registry can
//! pick a session up from whichever of its events it hears first. A blank field
//! withdraws nothing. An empty field keeps whatever the registry already knows,
//! so a report can carry only what it found at the time.

use std::collections::BTreeMap;

use crate::agent::{Agent, AgentId, SessionId, Turn, RECORD};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    sessions: BTreeMap<SessionId, Agent>,
}

impl Registry {
    /// Store an agent that announces itself, and keep whose turn it already was.
    ///
    /// An agent announces itself again whenever its own session restarts under
    /// it. That announcement says nothing new about the turn in progress. If
    /// this changed anything, the result is `true`.
    pub fn start(&mut self, mut agent: Agent) -> bool {
        if let Some(known) = self.sessions.get(&agent.session) {
            agent.turn = known.turn;
            agent.raised = known.raised;
        }
        self.merge(agent)
    }

    /// Store what an agent reported of itself mid-session, whose turn it is
    /// included.
    pub fn report(&mut self, agent: Agent) -> bool {
        self.merge(agent)
    }

    /// Store a record, and keep every blank field that is already known.
    ///
    /// A title that the scan did not find this time is not a withdrawn title. A
    /// color written once, and since scrolled out of reach, is still the color
    /// of that session. If this changed anything, the result is `true`.
    fn merge(&mut self, mut agent: Agent) -> bool {
        if let Some(known) = self.sessions.get(&agent.session) {
            for (fresh, held) in [
                (&mut agent.meta.dir, &known.meta.dir),
                (&mut agent.meta.name, &known.meta.name),
                (&mut agent.meta.color, &known.meta.color),
                (&mut agent.meta.title, &known.meta.title),
            ] {
                if fresh.is_empty() {
                    *fresh = held.clone();
                }
            }
            // Where an agent is, and what process it is, are found the same way
            // every time. A record that says neither came from something that
            // failed to look, not from an agent that moved.
            if agent.origin.is_empty() {
                agent.origin = known.origin.clone();
            }
            if agent.process.is_none() {
                agent.process = known.process;
            }
            // The lead arrives on a hook and is read off no file. A report that
            // names none came from a look that found none, and not from a child
            // that changed hands.
            if agent.lead.is_none() {
                agent.lead = known.lead.clone();
            }
        }
        self.sessions.insert(agent.session.clone(), agent.clone()) != Some(agent)
    }

    /// Drop an agent's session, and every agent under it at any depth.
    ///
    /// A child runs inside the process of its lead, so it cannot outlive that
    /// process. A lead that leaves therefore takes its whole subtree with it,
    /// and no row is left behind that names a session which is gone.
    ///
    /// If this dropped anything, the result is `true`.
    pub fn end(&mut self, root: &SessionId) -> bool {
        let dropped = self.sessions.remove(root).is_some();
        let under = self.agents_under(root);
        for agent in &under {
            self.sessions.remove(agent);
        }
        dropped || !under.is_empty()
    }

    /// Every agent under `root`, at any depth. `root` itself is not listed.
    ///
    /// A composed id holds the session and then one agent id for each level, so
    /// the id is the path. One pass over the sessions therefore returns the
    /// whole subtree. The daemon calls this to drop what it watches for a child,
    /// which the registry knows nothing about.
    pub fn agents_under(&self, root: &SessionId) -> Vec<SessionId> {
        self.sessions
            .keys()
            .filter(|session| session.is_under(root))
            .cloned()
            .collect()
    }

    /// The composed id of the agent that `agent` names, somewhere under
    /// `session`.
    ///
    /// `SubagentStop` names an agent id and never the chain, so the daemon
    /// cannot compute the key of the child that stopped. It matches instead.
    /// The match is against the whole last level of an id, and never against a
    /// substring of one. Claude gives one agent id to one child of a session, so
    /// at most one agent answers.
    pub fn session_of_agent_id(&self, session: &SessionId, agent: &AgentId) -> Option<&SessionId> {
        self.sessions
            .keys()
            .find(|held| held.is_under(session) && held.last_segment() == agent.as_str())
    }

    /// Every agent calling for the user, the most recent call first.
    pub fn calling(&self) -> Vec<&Agent> {
        let mut calling: Vec<&Agent> = self
            .sessions
            .values()
            .filter(|agent| agent.turn == Turn::Attention)
            .collect();
        calling.sort_by(|a, b| b.raised.cmp(&a.raised).then(a.session.cmp(&b.session)));
        calling
    }

    /// Answer one session that asked for the user.
    ///
    /// Attention is a fact about an agent that the user did not reach yet. The
    /// arrival of the user settles it. Which arrival counts is not decided here.
    /// Only the code that draws the session knows where the user is.
    pub fn seen(&mut self, session: &SessionId) -> bool {
        match self.sessions.get_mut(session) {
            Some(agent) if agent.turn == Turn::Attention => {
                agent.turn = Turn::Idle;
                true
            }
            _ => false,
        }
    }

    pub fn get(&self, session: &SessionId) -> Option<&Agent> {
        self.sessions.get(session)
    }

    /// Every session held, in session order.
    pub fn iter(&self) -> impl Iterator<Item = &Agent> {
        self.sessions.values()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Every record, one per line, in session order.
    pub fn encode(&self) -> String {
        self.sessions
            .values()
            .map(Agent::encode)
            .collect::<Vec<String>>()
            .join(&RECORD.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::{agent, at_pane, colored, meta, reporting, session};
    use crate::agent::{Record, FORMAT};

    /// The id of one child, for a test that states the chain outright.
    fn agent_id(text: &str) -> AgentId {
        AgentId::new(text).unwrap()
    }

    #[test]
    fn re_filing_the_same_record_changes_nothing() {
        let mut registry = Registry::default();
        assert!(registry.start(agent("one", 3)));
        assert!(!registry.start(agent("one", 3)));
        assert!(registry.start(agent("one", 4)));
    }

    /// One agent that names another as the agent which started it.
    fn child(id: &str, lead: &str, pane: u32) -> Agent {
        agent(id, pane).with_lead(session(lead))
    }

    #[test]
    fn ending_a_lead_ends_every_child_under_it() {
        let mut registry = Registry::default();
        registry.start(agent("lead", 3));
        registry.start(child("lead.a1", "lead", 3));
        registry.start(child("lead.a2", "lead", 3));
        registry.start(agent("other", 4));
        assert!(registry.end(&session("lead")));
        assert_eq!(registry.get(&session("lead.a1")), None);
        assert_eq!(registry.get(&session("lead.a2")), None);
        // An agent that the ended session does not lead stays.
        assert!(registry.get(&session("other")).is_some());
    }

    #[test]
    fn ending_a_lead_ends_a_grandchild() {
        // A teammate starts a subagent of its own. Both go with the lead.
        let mut registry = Registry::default();
        registry.start(agent("lead", 3));
        registry.start(child("lead.mate", "lead", 3));
        registry.start(child("lead.mate.probe", "lead.mate", 3));
        assert!(registry.end(&session("lead")));
        assert!(registry.is_empty());
    }

    #[test]
    fn ending_a_teammate_ends_its_own_children_and_leaves_the_lead() {
        let mut registry = Registry::default();
        registry.start(agent("lead", 3));
        registry.start(child("lead.mate", "lead", 3));
        registry.start(child("lead.mate.probe", "lead.mate", 3));
        registry.start(child("lead.other", "lead", 3));
        assert!(registry.end(&session("lead.mate")));
        assert_eq!(registry.get(&session("lead.mate.probe")), None);
        assert!(registry.get(&session("lead")).is_some());
        assert!(registry.get(&session("lead.other")).is_some());
    }

    #[test]
    fn agents_under_a_lead_lists_every_depth() {
        let mut registry = Registry::default();
        registry.start(agent("lead", 3));
        registry.start(child("lead.mate", "lead", 3));
        registry.start(child("lead.mate.probe", "lead.mate", 3));
        assert_eq!(
            registry.agents_under(&session("lead")),
            vec![session("lead.mate"), session("lead.mate.probe")]
        );
        // A leaf leads nothing.
        assert!(registry
            .agents_under(&session("lead.mate.probe"))
            .is_empty());
    }

    #[test]
    fn an_agent_id_finds_the_child_that_carries_it_at_any_depth() {
        let mut registry = Registry::default();
        registry.start(agent("lead", 3));
        registry.start(child("lead.mate", "lead", 3));
        registry.start(child("lead.mate.probe", "lead.mate", 3));
        assert_eq!(
            registry.session_of_agent_id(&session("lead"), &agent_id("probe")),
            Some(&session("lead.mate.probe"))
        );
        // A session leads itself nowhere, so its own id answers nothing.
        assert_eq!(
            registry.session_of_agent_id(&session("lead"), &agent_id("lead")),
            None
        );
    }

    #[test]
    fn an_agent_id_matches_a_whole_level_and_never_a_substring() {
        // A teammate agent id carries the name that the user chose. A substring
        // match ends an agent that nothing stopped.
        let mut registry = Registry::default();
        registry.start(agent("lead", 3));
        registry.start(child("lead.mate-probe", "lead", 3));
        assert_eq!(
            registry.session_of_agent_id(&session("lead"), &agent_id("probe")),
            None
        );
        // Another session's child is not this session's child either.
        registry.start(agent("other", 4));
        registry.start(child("other.probe", "other", 4));
        assert_eq!(
            registry.session_of_agent_id(&session("lead"), &agent_id("probe")),
            None
        );
    }

    #[test]
    fn ending_a_child_leaves_its_lead() {
        let mut registry = Registry::default();
        registry.start(agent("lead", 3));
        registry.start(child("lead.a1", "lead", 3));
        assert!(registry.end(&session("lead.a1")));
        assert!(registry.get(&session("lead")).is_some());
    }

    #[test]
    fn a_report_that_names_no_lead_withdraws_none() {
        let mut registry = Registry::default();
        registry.start(child("lead.a1", "lead", 3));
        // A later report of the same child that says nothing about its lead.
        registry.report(agent("lead.a1", 3));
        assert_eq!(
            registry.get(&session("lead.a1")).unwrap().lead,
            Some(session("lead"))
        );
    }

    #[test]
    fn a_lead_survives_the_round_trip_through_a_record() {
        let mut registry = Registry::default();
        registry.start(child("lead.a1", "lead", 3));
        let line = registry.encode();
        let Record::Known(read) = Agent::decode(&line) else {
            panic!("not a record");
        };
        assert_eq!(read.lead, Some(session("lead")));
    }

    #[test]
    fn a_record_written_in_another_format_says_which_one() {
        // The two ends of the wire are installed separately, so one can be
        // older than the other. A record that the older end wrote is not a line
        // to pass over in silence.
        let record = agent("one", 3).encode();
        let older = record.replacen(&FORMAT.to_string(), "0", 1);
        assert_eq!(Agent::decode(&older), Record::Foreign(0));
    }

    #[test]
    fn a_report_files_a_session_nobody_saw_start() {
        let mut registry = Registry::default();
        assert!(registry.report(reporting("one", 3, Turn::Working, 0)));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Working);
        // The same report twice is not a change.
        assert!(!registry.report(reporting("one", 3, Turn::Working, 0)));
    }

    #[test]
    fn announcing_a_session_again_leaves_its_turn_alone() {
        // An agent registers again whenever its own session restarts under it.
        // That says nothing about the turn in progress.
        let mut registry = Registry::default();
        registry.start(agent("one", 3));
        registry.report(reporting("one", 3, Turn::Working, 0));
        registry.start(agent("one", 3));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Working);
    }

    #[test]
    fn a_record_that_says_nothing_new_about_a_session_unsays_nothing() {
        let mut registry = Registry::default();
        registry.start(Agent::new(
            session("one"),
            "claude",
            meta("wrangler", "scout", "the port"),
            at_pane(1),
        ));
        registry.report(reporting("one", 1, Turn::Working, 0));
        let held = registry.get(&session("one")).unwrap();
        assert_eq!(held.meta, meta("wrangler", "scout", "the port"));
    }

    #[test]
    fn a_color_found_once_outlives_the_window_it_was_found_in() {
        // A session records its color as it begins. Only a fixed window over
        // the end of the transcript is read, so the color scrolls out of sight
        // as the session runs. The registry holds the color, and nothing looks
        // for it again. The session keeps the color it was given, and only a
        // record that names a different color changes it.
        let mut registry = Registry::default();
        registry.start(colored("one", "red"));
        registry.report(reporting("one", 1, Turn::Working, 0));
        assert_eq!(registry.get(&session("one")).unwrap().meta.color, "red");
        registry.start(colored("one", "blue"));
        assert_eq!(registry.get(&session("one")).unwrap().meta.color, "blue");
    }

    #[test]
    fn a_record_that_does_say_something_new_replaces_it() {
        let mut registry = Registry::default();
        registry.start(agent("one", 1));
        registry.start(Agent::new(
            session("one"),
            "claude",
            meta("wrangler", "", "the port"),
            at_pane(1),
        ));
        assert_eq!(
            registry.get(&session("one")).unwrap().meta.title,
            "the port"
        );
    }

    #[test]
    fn arriving_at_a_session_answers_it() {
        let mut registry = Registry::default();
        registry.report(reporting("one", 3, Turn::Attention, 0));
        registry.report(reporting("two", 9, Turn::Attention, 0));
        assert!(registry.seen(&session("one")));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Idle);
        // Every other session still asks.
        assert_eq!(registry.get(&session("two")).unwrap().turn, Turn::Attention);
    }

    #[test]
    fn calls_for_the_user_are_listed_newest_first() {
        let mut registry = Registry::default();
        for (id, at) in [("one", 10), ("two", 30), ("three", 20)] {
            registry.report(reporting(id, 1, Turn::Attention, at));
        }
        // An agent that does not call is not listed at all.
        registry.start(agent("quiet", 1));
        let calling: Vec<&str> = registry
            .calling()
            .iter()
            .map(|agent| agent.session.as_str())
            .collect();
        assert_eq!(calling, vec!["two", "three", "one"]);
    }

    #[test]
    fn a_call_raised_again_moves_to_the_front() {
        let mut registry = Registry::default();
        registry.report(reporting("one", 1, Turn::Attention, 10));
        registry.report(reporting("two", 1, Turn::Attention, 20));
        assert!(registry.report(reporting("one", 1, Turn::Attention, 30)));
        assert_eq!(registry.calling()[0].session.as_str(), "one");
    }

    #[test]
    fn arriving_at_a_session_that_is_working_leaves_it_alone() {
        // The arrival of the user answers only a call for the user. Work
        // carries on.
        let mut registry = Registry::default();
        registry.report(reporting("one", 3, Turn::Working, 0));
        assert!(!registry.seen(&session("one")));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Working);
        // A session that nobody knows of is nothing to answer.
        assert!(!registry.seen(&session("gone")));
    }

    #[test]
    fn ending_a_session_that_was_never_filed_changes_nothing() {
        let mut registry = Registry::default();
        assert!(!registry.end(&session("one")));
        registry.start(agent("one", 3));
        assert!(registry.end(&session("one")));
    }
}
