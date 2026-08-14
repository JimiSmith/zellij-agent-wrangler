use std::collections::BTreeSet;

use agent_wrangler_core::agent::{Agent, SessionId};
use agent_wrangler_core::registry::Registry;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answered(BTreeSet<(SessionId, u64)>);

impl Answered {
    pub fn answer(&mut self, agent: &Agent) {
        self.0.insert((agent.session.clone(), agent.raised));
    }

    pub fn settled(&self, registry: &Registry) -> Vec<SessionId> {
        registry
            .calling()
            .into_iter()
            .filter(|agent| self.0.contains(&(agent.session.clone(), agent.raised)))
            .map(|agent| agent.session.clone())
            .collect()
    }

    pub fn prune(&mut self, registry: &Registry) {
        self.0
            .retain(|(session, _)| registry.get(session).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_wrangler_core::agent::{Meta, Turn};
    use agent_wrangler_core::origin::Origin;

    fn calling(id: &str, raised: u64) -> Agent {
        let mut agent = Agent::new(
            SessionId::new(id).unwrap(),
            "claude",
            Meta::default(),
            Origin::default(),
        );
        agent.turn = Turn::Attention;
        agent.raised = raised;
        agent
    }

    #[test]
    fn the_exact_answered_call_is_suppressed() {
        let mut answered = Answered::default();
        let agent = calling("one", 5);
        answered.answer(&agent);
        let mut registry = Registry::default();
        registry.report(agent);
        assert_eq!(
            answered.settled(&registry),
            [SessionId::new("one").unwrap()]
        );
    }

    #[test]
    fn a_later_call_from_the_same_session_is_not_suppressed() {
        let mut answered = Answered::default();
        answered.answer(&calling("one", 5));
        let mut registry = Registry::default();
        registry.report(calling("one", 6));
        assert!(answered.settled(&registry).is_empty());
    }

    #[test]
    fn an_answer_to_a_session_that_ended_is_forgotten() {
        let mut answered = Answered::default();
        let agent = calling("one", 5);
        answered.answer(&agent);
        let mut registry = Registry::default();
        registry.report(agent);
        answered.prune(&registry);
        assert_eq!(answered.settled(&registry).len(), 1);

        answered.prune(&Registry::default());
        assert_eq!(answered, Answered::default());
    }
}
