use std::collections::BTreeMap;

use agent_wrangler_core::agent::{Agent, SessionId};
use agent_wrangler_core::registry::Registry;

/// The last call that each session answered.
///
/// A session raises attention once at a time. Only its latest answered
/// `raised` decides anything. An older `raised` can never come back. The next
/// call carries a `raised` that this map does not hold, so nothing suppresses
/// that call. One entry per session limits the size to the number of sessions
/// in the registry, not to the number of calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnsweredCalls(BTreeMap<SessionId, u64>);

impl AnsweredCalls {
    /// Remember that the user reached this session while it called.
    pub fn record_answer(&mut self, agent: &Agent) {
        self.0.insert(agent.session.clone(), agent.raised);
    }

    /// The sessions that call now, and whose current call the user already
    /// answered. The sidebar draws no attention for these.
    pub fn already_answered_sessions(&self, registry: &Registry) -> Vec<SessionId> {
        registry
            .calling()
            .into_iter()
            .filter(|agent| self.0.get(&agent.session) == Some(&agent.raised))
            .map(|agent| agent.session.clone())
            .collect()
    }

    /// Forget every session that the registry no longer holds.
    pub fn drop_gone_sessions(&mut self, registry: &Registry) {
        self.0.retain(|session, _| registry.get(session).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::agent::{LabelFacts, Turn};
    use agent_wrangler_core::origin::Origin;

    fn calling(id: &str, raised: u64) -> Agent {
        let mut agent = Agent::new(
            SessionId::new(id).unwrap(),
            "claude",
            LabelFacts::default(),
            Origin::default(),
        );
        agent.turn = Turn::Attention;
        agent.raised = raised;
        agent
    }

    #[test]
    fn the_exact_answered_call_is_suppressed() {
        let mut answered = AnsweredCalls::default();
        let agent = calling("one", 5);
        answered.record_answer(&agent);
        let mut registry = Registry::default();
        registry.report(agent);
        assert_eq!(
            answered.already_answered_sessions(&registry),
            [SessionId::new("one").unwrap()]
        );
    }

    #[test]
    fn a_later_call_from_the_same_session_is_not_suppressed() {
        let mut answered = AnsweredCalls::default();
        answered.record_answer(&calling("one", 5));
        let mut registry = Registry::default();
        registry.report(calling("one", 6));
        assert!(answered.already_answered_sessions(&registry).is_empty());
    }

    #[test]
    fn a_session_that_keeps_calling_is_remembered_once() {
        let mut answered = AnsweredCalls::default();
        let mut registry = Registry::default();
        let mut held = Vec::new();
        for raised in 0..1_000 {
            let agent = calling("one", raised);
            answered.record_answer(&agent);
            registry.report(agent);
            answered.drop_gone_sessions(&registry);
            held.push(answered.0.len());
        }
        assert_eq!(held.iter().max(), Some(&1));
        assert_eq!(
            answered.already_answered_sessions(&registry),
            [SessionId::new("one").unwrap()]
        );

        registry.report(calling("one", 1_000));
        assert!(answered.already_answered_sessions(&registry).is_empty());
    }

    #[test]
    fn an_answer_to_a_session_that_ended_is_forgotten() {
        let mut answered = AnsweredCalls::default();
        let agent = calling("one", 5);
        answered.record_answer(&agent);
        let mut registry = Registry::default();
        registry.report(agent);
        answered.drop_gone_sessions(&registry);
        assert_eq!(answered.already_answered_sessions(&registry).len(), 1);

        answered.drop_gone_sessions(&Registry::default());
        assert_eq!(answered, AnsweredCalls::default());
    }
}
