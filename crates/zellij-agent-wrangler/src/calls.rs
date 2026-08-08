//! The calls agents make for the user, and which of them the user has already
//! turned up to.
//!
//! A call is a fact about an agent the user has not got to yet, and the sidebar
//! is told that fact rather than deciding it. What follows from being told
//! rather than knowing is that an answer given here takes a round trip to come
//! back, so the state that arrives in the meantime still says the agent is
//! asking.

use std::collections::BTreeSet;

use agent_wrangler_core::agent::{Agent, SessionId};
use agent_wrangler_core::registry::Registry;

/// The calls the user has already turned up to, each by which call it was.
///
/// The state a sidebar draws is handed to it whole, and the answer travels the
/// other way, so the two cross: the state written before the answer arrived
/// still says the agent is asking, and drawing it as it reads would put the row
/// back for as long as the round trip takes. Holding the answered calls is what
/// keeps that row down until the answer is reflected in what arrives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answered(BTreeSet<(SessionId, u64)>);

impl Answered {
    /// Record that the user has turned up to this call.
    pub fn answer(&mut self, agent: &Agent) {
        self.0.insert((agent.session.clone(), agent.raised));
    }

    /// Every session in `registry` whose call has already been answered.
    ///
    /// A session calling again is calling with a moment of its own, so it is not
    /// one of these: only the very call that was answered is.
    pub fn settled(&self, registry: &Registry) -> Vec<SessionId> {
        registry
            .calling()
            .into_iter()
            .filter(|agent| self.0.contains(&(agent.session.clone(), agent.raised)))
            .map(|agent| agent.session.clone())
            .collect()
    }

    /// Forget every answer to a session `registry` no longer holds.
    ///
    /// An answer is only ever worth holding against the state that might still
    /// contradict it, and a session that has ended will not be described again.
    /// Without this the set would grow for as long as the sidebar runs.
    pub fn prune(&mut self, registry: &Registry) {
        self.0
            .retain(|(session, _)| registry.get(session).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::agent::Turn;

    use crate::agents::tests::{agent, at_pane, session};

    /// A registry holding one agent per call described: whose it is, whether it
    /// is asking, and when it asked.
    fn registry(calls: &[(&str, Turn, u64)]) -> Registry {
        let mut registry = Registry::default();
        for (id, turn, raised) in calls {
            registry.report(Agent {
                turn: *turn,
                raised: *raised,
                ..agent(id, at_pane(1))
            });
        }
        registry
    }

    fn calling(registry: &Registry) -> Vec<SessionId> {
        registry
            .calling()
            .into_iter()
            .map(|agent| agent.session.clone())
            .collect()
    }

    #[test]
    fn an_answered_call_is_settled_however_often_it_is_described_again() {
        let mut answered = Answered::default();
        let registry = registry(&[("one", Turn::Attention, 5), ("two", Turn::Attention, 5)]);
        answered.answer(registry.get(&session("one")).unwrap());
        assert_eq!(answered.settled(&registry), vec![session("one")]);
        // The state arriving again says exactly what it said before, and is
        // still not news.
        assert_eq!(answered.settled(&registry), vec![session("one")]);
    }

    #[test]
    fn a_session_that_calls_again_is_calling_unanswered() {
        let mut answered = Answered::default();
        let held = registry(&[("one", Turn::Attention, 5)]);
        answered.answer(held.get(&session("one")).unwrap());
        let again = registry(&[("one", Turn::Attention, 20)]);
        assert!(answered.settled(&again).is_empty());
    }

    #[test]
    fn a_settled_call_is_what_puts_the_row_back_down() {
        // What the answer does to the state it is drawn from, since holding the
        // answer is only worth anything if it is applied.
        let mut answered = Answered::default();
        let mut registry = registry(&[("one", Turn::Attention, 5), ("two", Turn::Attention, 7)]);
        answered.answer(registry.get(&session("one")).unwrap());
        for settled in answered.settled(&registry) {
            registry.seen(&settled);
        }
        assert_eq!(calling(&registry), vec![session("two")]);
    }

    #[test]
    fn an_answer_to_a_session_that_has_ended_is_forgotten() {
        let mut answered = Answered::default();
        let registry = registry(&[("one", Turn::Attention, 5)]);
        answered.answer(registry.get(&session("one")).unwrap());
        // A session still running is a session that can still contradict the
        // answer, so the answer is kept.
        answered.prune(&registry);
        assert_eq!(answered.settled(&registry), vec![session("one")]);
        answered.prune(&Registry::default());
        assert_eq!(answered, Answered::default());
    }
}
