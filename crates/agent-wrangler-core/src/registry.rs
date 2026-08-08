//! Every agent session known about, newest state per session.
//!
//! A record says everything there is to say about a session, so one can be
//! picked up from whichever of its events is heard first. What a record leaves
//! blank is not a withdrawal: an empty field keeps whatever is already known,
//! which is what lets a report carry only what it could find at the time.

use std::collections::BTreeMap;

use crate::agent::{Agent, Record, SessionId, Turn, RECORD};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    sessions: BTreeMap<SessionId, Agent>,
}

impl Registry {
    /// File an agent announcing itself, keeping whose turn it already was.
    ///
    /// An agent announces itself again whenever its own session restarts under
    /// it, which says nothing new about the turn it is in the middle of. `true`
    /// when this changed anything.
    pub fn start(&mut self, mut agent: Agent) -> bool {
        if let Some(known) = self.sessions.get(&agent.session) {
            agent.turn = known.turn;
            agent.raised = known.raised;
        }
        self.merge(agent)
    }

    /// File what an agent reported of itself mid-session, whose turn it is
    /// included.
    pub fn report(&mut self, agent: Agent) -> bool {
        self.merge(agent)
    }

    /// File a record, keeping whatever it leaves blank that is already known.
    ///
    /// A title that could not be found this time is not a title withdrawn, and
    /// a color written once and since scrolled out of reach is still the color
    /// of that session. `true` when this changed anything.
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
        }
        self.sessions.insert(agent.session.clone(), agent.clone()) != Some(agent)
    }

    /// Drop an agent's session. `true` when there was one to drop.
    pub fn end(&mut self, session: &SessionId) -> bool {
        self.sessions.remove(session).is_some()
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

    /// Answer the agents in `pane` that were asking for the user.
    ///
    /// Attention is a fact about an agent the user has not got to yet, so
    /// arriving at its pane is what settles it.
    pub fn seen(&mut self, pane: u32) -> bool {
        let mut changed = false;
        for agent in self.sessions.values_mut() {
            if agent.pane == Some(pane) && agent.turn == Turn::Attention {
                agent.turn = Turn::Idle;
                changed = true;
            }
        }
        changed
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

    /// Take in every record of a run `encode` wrote, turns included, keeping
    /// what is already known about a session the run does not mention. `true`
    /// when this changed anything.
    pub fn absorb(&mut self, text: &str) -> bool {
        let mut changed = false;
        for line in text.split(RECORD) {
            if let Record::Known(agent) = Agent::decode(line) {
                changed |= self.merge(agent);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::{agent, colored, meta, reporting, session};
    use crate::agent::FORMAT;

    #[test]
    fn a_registry_survives_the_round_trip() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(3)));
        registry.start(agent("two", None));
        let mut copy = Registry::default();
        assert!(copy.absorb(&registry.encode()));
        assert_eq!(copy, registry);
    }

    #[test]
    fn an_empty_registry_absorbs_to_nothing() {
        let mut registry = Registry::default();
        assert!(!registry.absorb(&Registry::default().encode()));
        assert_eq!(registry, Registry::default());
    }

    #[test]
    fn absorbing_keeps_a_session_the_text_does_not_mention() {
        let mut registry = Registry::default();
        registry.start(agent("mine", Some(1)));
        let mut other = Registry::default();
        other.start(agent("theirs", Some(2)));
        registry.absorb(&other.encode());
        assert!(registry.get(&session("mine")).is_some());
        assert!(registry.get(&session("theirs")).is_some());
    }

    #[test]
    fn re_filing_the_same_record_changes_nothing() {
        let mut registry = Registry::default();
        assert!(registry.start(agent("one", Some(3))));
        assert!(!registry.start(agent("one", Some(3))));
        assert!(registry.start(agent("one", Some(4))));
    }

    #[test]
    fn a_record_written_in_another_format_says_which_one() {
        // The two ends of the wire are installed separately, so one can be
        // older than the other, and a record it wrote is not a line to pass
        // over in silence.
        let record = agent("one", Some(3)).encode();
        let older = record.replacen(&FORMAT.to_string(), "0", 1);
        assert_eq!(Agent::decode(&older), Record::Foreign(0));
        // Absorbing takes nothing from a format it does not know.
        let mut registry = Registry::default();
        assert!(!registry.absorb(&older));
    }

    #[test]
    fn a_report_files_a_session_nobody_saw_start() {
        let mut registry = Registry::default();
        assert!(registry.report(reporting("one", Some(3), Turn::Working, 0)));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Working);
        // Saying the same thing twice is not a change.
        assert!(!registry.report(reporting("one", Some(3), Turn::Working, 0)));
    }

    #[test]
    fn announcing_a_session_again_leaves_its_turn_alone() {
        // An agent re-registers whenever its own session restarts under it,
        // which says nothing about the turn it is in the middle of.
        let mut registry = Registry::default();
        registry.start(agent("one", Some(3)));
        registry.report(reporting("one", Some(3), Turn::Working, 0));
        registry.start(agent("one", Some(3)));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Working);
    }

    #[test]
    fn a_record_that_says_nothing_new_about_a_session_unsays_nothing() {
        let mut registry = Registry::default();
        registry.start(Agent::new(
            session("one"),
            "claude",
            meta("wrangler", "scout", "the port"),
            Some(1),
        ));
        registry.report(reporting("one", Some(1), Turn::Working, 0));
        let held = registry.get(&session("one")).unwrap();
        assert_eq!(held.meta, meta("wrangler", "scout", "the port"));
    }

    #[test]
    fn a_color_found_once_outlives_the_window_it_was_found_in() {
        // A session records its color as it begins, and only a fixed window
        // over the end of the transcript is read, so the color scrolls out of
        // sight as the session runs. It is held here rather than looked for
        // again: the session keeps the color it was given, and only a record
        // that names a different one changes it.
        let mut registry = Registry::default();
        registry.start(colored("one", "red"));
        registry.report(reporting("one", Some(1), Turn::Working, 0));
        assert_eq!(registry.get(&session("one")).unwrap().meta.color, "red");
        registry.start(colored("one", "blue"));
        assert_eq!(registry.get(&session("one")).unwrap().meta.color, "blue");
    }

    #[test]
    fn a_record_that_does_say_something_new_replaces_it() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(1)));
        registry.start(Agent::new(
            session("one"),
            "claude",
            meta("wrangler", "", "the port"),
            Some(1),
        ));
        assert_eq!(
            registry.get(&session("one")).unwrap().meta.title,
            "the port"
        );
    }

    #[test]
    fn absorbing_takes_the_turn_it_is_told() {
        let mut mine = Registry::default();
        mine.start(agent("one", Some(3)));
        let mut theirs = Registry::default();
        theirs.report(reporting("one", Some(3), Turn::Attention, 5));
        assert!(mine.absorb(&theirs.encode()));
        assert_eq!(mine.get(&session("one")).unwrap().turn, Turn::Attention);
    }

    #[test]
    fn arriving_at_a_pane_answers_the_agents_asking_from_it() {
        let mut registry = Registry::default();
        registry.report(reporting("one", Some(3), Turn::Attention, 0));
        registry.report(reporting("two", Some(9), Turn::Attention, 0));
        assert!(registry.seen(3));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Idle);
        // Another pane's agent is still asking.
        assert_eq!(registry.get(&session("two")).unwrap().turn, Turn::Attention);
    }

    #[test]
    fn calls_for_the_user_are_listed_newest_first() {
        let mut registry = Registry::default();
        for (id, at) in [("one", 10), ("two", 30), ("three", 20)] {
            registry.report(reporting(id, Some(1), Turn::Attention, at));
        }
        // An agent that is not calling is not listed at all.
        registry.start(agent("quiet", Some(1)));
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
        registry.report(reporting("one", Some(1), Turn::Attention, 10));
        registry.report(reporting("two", Some(1), Turn::Attention, 20));
        assert!(registry.report(reporting("one", Some(1), Turn::Attention, 30)));
        assert_eq!(registry.calling()[0].session.as_str(), "one");
    }

    #[test]
    fn arriving_at_a_pane_leaves_an_agent_working_in_it_alone() {
        // Only a call for the user is answered by turning up; work carries on.
        let mut registry = Registry::default();
        registry.report(reporting("one", Some(3), Turn::Working, 0));
        assert!(!registry.seen(3));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Working);
    }

    #[test]
    fn ending_a_session_that_was_never_filed_changes_nothing() {
        let mut registry = Registry::default();
        assert!(!registry.end(&session("one")));
        registry.start(agent("one", Some(3)));
        assert!(registry.end(&session("one")));
    }
}
