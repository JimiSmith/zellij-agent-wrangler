//! The agent sessions the sidebar knows about, and where in the tree they sit.
//!
//! A record arrives from the agent's own lifecycle hooks, which report the pane
//! they were invoked in. The sidebar files them by session id and hands each one
//! to the pane it named; a record whose pane is not on screen is held but drawn
//! nowhere, so an agent in a pane that has gone leaves no row behind.

use std::collections::BTreeMap;

use crate::model::SessionId;
use crate::tree::Tab;

/// The character between a record's fields, and the one between records. Both
/// are excluded from every field, so a run of records splits exactly.
const FIELD: char = '\t';
const RECORD: char = '\n';

/// The pipe an agent's hooks report a session on, carrying one record.
pub const START_MESSAGE: &str = "wrangler:agent-start";

/// The pipes they report a session's end and its turn on, each carrying the
/// session id alone. What the agent is doing is a fact about a session already
/// filed, so these name it rather than describing it again.
pub const END_MESSAGE: &str = "wrangler:agent-end";
pub const WORKING_MESSAGE: &str = "wrangler:agent-working";
pub const ATTENTION_MESSAGE: &str = "wrangler:agent-attention";

/// What a sidebar with no records of its own asks, and what any sidebar that
/// has some answers with.
pub const SYNC_REQUEST_MESSAGE: &str = "wrangler:agents?";
pub const SYNC_MESSAGE: &str = "wrangler:agents";

/// Whose turn it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Turn {
    /// The agent is waiting, and wants nothing.
    #[default]
    Idle,
    /// The agent is mid-turn.
    Working,
    /// The agent wants the user.
    Attention,
}

impl Turn {
    fn encode(self) -> &'static str {
        match self {
            Turn::Idle => "idle",
            Turn::Working => "working",
            Turn::Attention => "attention",
        }
    }

    fn decode(text: &str) -> Option<Self> {
        match text {
            "idle" => Some(Turn::Idle),
            "working" => Some(Turn::Working),
            "attention" => Some(Turn::Attention),
            _ => None,
        }
    }
}

/// One agent session: who it is, what to call it, the pane it reported itself
/// from, and whose turn it is.
///
/// The two strings are trimmed of anything that could split the wire format,
/// which is why the fields are built rather than assigned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub session: SessionId,
    pub agent: String,
    pub label: String,
    pub pane: Option<u32>,
    pub turn: Turn,
}

/// Replace every character that would split a record or a field, and every
/// control character, with a space.
fn field(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Agent {
    pub fn new(session: SessionId, agent: &str, label: &str, pane: Option<u32>) -> Self {
        Agent {
            session,
            agent: field(agent),
            label: field(label),
            pane,
            turn: Turn::default(),
        }
    }

    /// The record as one line: session, agent, pane, turn, label. The pane is
    /// written as nothing at all when the agent reported none.
    pub fn encode(&self) -> String {
        let pane = self.pane.map(|id| id.to_string()).unwrap_or_default();
        format!(
            "{}{FIELD}{}{FIELD}{pane}{FIELD}{}{FIELD}{}",
            self.session.as_str(),
            self.agent,
            self.turn.encode(),
            self.label
        )
    }

    /// The record `encode` wrote, or `None` for a line that is not one.
    ///
    /// The label is the whole remainder of the line, so a label carrying the
    /// field character would still parse; it cannot, because the constructor
    /// takes that character out.
    pub fn decode(line: &str) -> Option<Self> {
        let mut fields = line.splitn(5, FIELD);
        let session = SessionId::new(fields.next()?)?;
        let agent = fields.next()?;
        let pane = fields.next()?;
        let turn = Turn::decode(fields.next()?)?;
        let label = fields.next()?;
        let pane = if pane.is_empty() {
            None
        } else {
            Some(pane.parse().ok()?)
        };
        Some(Agent {
            turn,
            ..Agent::new(session, agent, label, pane)
        })
    }
}

/// Every agent session the sidebar has been told about, newest state per
/// session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    sessions: BTreeMap<SessionId, Agent>,
}

impl Registry {
    /// File an agent, keeping whose turn it already was.
    ///
    /// An agent announces itself again whenever its own session restarts under
    /// it, which says nothing new about the turn it is in the middle of. `true`
    /// when this changed anything.
    pub fn start(&mut self, mut agent: Agent) -> bool {
        if let Some(known) = self.sessions.get(&agent.session) {
            agent.turn = known.turn;
        }
        self.adopt(agent)
    }

    /// File an agent exactly as given, turn included.
    fn adopt(&mut self, agent: Agent) -> bool {
        self.sessions.insert(agent.session.clone(), agent.clone()) != Some(agent)
    }

    /// Drop an agent's session. `true` when there was one to drop.
    pub fn end(&mut self, session: &SessionId) -> bool {
        self.sessions.remove(session).is_some()
    }

    /// Say whose turn it is in a session already filed. An agent the sidebar
    /// has never been told about has no row to mark, so this says nothing.
    pub fn mark(&mut self, session: &SessionId, turn: Turn) -> bool {
        match self.sessions.get_mut(session) {
            Some(agent) if agent.turn != turn => {
                agent.turn = turn;
                true
            }
            _ => false,
        }
    }

    /// Answer the agents in `pane` that were asking for the user.
    ///
    /// Attention is a fact about an agent the user has not got to yet, so
    /// arriving at its pane is what settles it. Every sidebar reads this off
    /// the same focus and reaches the same answer, so none has to tell the
    /// others.
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
        for agent in text.split(RECORD).filter_map(Agent::decode) {
            changed |= self.adopt(agent);
        }
        changed
    }
}

/// Hand each pane the agents that reported themselves from it, in session
/// order.
///
/// A pane holding agents is drawn as them rather than as itself, so this is what
/// decides whether a pane appears in the tree under its own title.
pub fn place(tabs: &mut [Tab], registry: &Registry) {
    for tab in tabs.iter_mut() {
        for pane in tab.panes.iter_mut() {
            pane.agents = registry
                .sessions
                .values()
                .filter(|agent| agent.pane == Some(pane.id))
                .cloned()
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Pane;

    fn session(text: &str) -> SessionId {
        SessionId::new(text).unwrap()
    }

    fn agent(id: &str, pane: Option<u32>) -> Agent {
        Agent::new(session(id), "claude", "wrangler", pane)
    }

    #[test]
    fn a_record_survives_the_round_trip() {
        for record in [agent("one", Some(3)), agent("two", None)] {
            assert_eq!(Agent::decode(&record.encode()), Some(record));
        }
    }

    #[test]
    fn a_label_cannot_split_the_record_it_sits_at_the_end_of() {
        let record = Agent::new(session("one"), "claude", "a\tb\nc", Some(1));
        assert_eq!(record.label, "a b c");
        assert_eq!(Agent::decode(&record.encode()), Some(record));
    }

    #[test]
    fn a_line_that_is_not_a_record_decodes_to_nothing() {
        for line in ["", "one", "one\tclaude", "one\tclaude\tx\tlabel"] {
            assert_eq!(Agent::decode(line), None, "{line}");
        }
    }

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
    fn a_turn_survives_the_round_trip() {
        for turn in [Turn::Idle, Turn::Working, Turn::Attention] {
            let record = Agent {
                turn,
                ..agent("one", Some(3))
            };
            assert_eq!(Agent::decode(&record.encode()), Some(record));
        }
    }

    #[test]
    fn a_record_with_no_turn_it_recognises_decodes_to_nothing() {
        assert_eq!(Agent::decode("one\tclaude\t3\tdozing\tlabel"), None);
    }

    #[test]
    fn a_turn_is_only_marked_on_a_session_already_filed() {
        let mut registry = Registry::default();
        assert!(!registry.mark(&session("one"), Turn::Working));
        registry.start(agent("one", Some(3)));
        assert!(registry.mark(&session("one"), Turn::Working));
        // Saying the same thing twice is not a change.
        assert!(!registry.mark(&session("one"), Turn::Working));
    }

    #[test]
    fn announcing_a_session_again_leaves_its_turn_alone() {
        // An agent re-registers whenever its own session restarts under it,
        // which says nothing about the turn it is in the middle of.
        let mut registry = Registry::default();
        registry.start(agent("one", Some(3)));
        registry.mark(&session("one"), Turn::Working);
        registry.start(agent("one", Some(3)));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Working);
    }

    #[test]
    fn absorbing_takes_the_turn_it_is_told() {
        let mut mine = Registry::default();
        mine.start(agent("one", Some(3)));
        let mut theirs = Registry::default();
        theirs.start(agent("one", Some(3)));
        theirs.mark(&session("one"), Turn::Attention);
        assert!(mine.absorb(&theirs.encode()));
        assert_eq!(mine.get(&session("one")).unwrap().turn, Turn::Attention);
    }

    #[test]
    fn arriving_at_a_pane_answers_the_agents_asking_from_it() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(3)));
        registry.start(agent("two", Some(9)));
        registry.mark(&session("one"), Turn::Attention);
        registry.mark(&session("two"), Turn::Attention);
        assert!(registry.seen(3));
        assert_eq!(registry.get(&session("one")).unwrap().turn, Turn::Idle);
        // Another pane's agent is still asking.
        assert_eq!(registry.get(&session("two")).unwrap().turn, Turn::Attention);
    }

    #[test]
    fn arriving_at_a_pane_leaves_an_agent_working_in_it_alone() {
        // Only a call for the user is answered by turning up; work carries on.
        let mut registry = Registry::default();
        registry.start(agent("one", Some(3)));
        registry.mark(&session("one"), Turn::Working);
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

    fn tabs() -> Vec<Tab> {
        vec![Tab {
            position: 0,
            name: "one".to_string(),
            active: true,
            panes: vec![Pane::new(1, "bash", false), Pane::new(2, "nvim", true)],
        }]
    }

    #[test]
    fn an_agent_lands_on_the_pane_it_reported_itself_from() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(2)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert!(tabs[0].panes[0].agents.is_empty());
        assert_eq!(tabs[0].panes[1].agents.len(), 1);
    }

    #[test]
    fn an_agent_whose_pane_is_gone_lands_nowhere() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(99)));
        registry.start(agent("two", None));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert!(tabs
            .iter()
            .all(|tab| tab.panes.iter().all(|pane| pane.agents.is_empty())));
    }

    #[test]
    fn two_agents_in_one_pane_both_land_on_it() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(1)));
        registry.start(agent("two", Some(1)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        assert_eq!(tabs[0].panes[0].agents.len(), 2);
    }

    #[test]
    fn placing_again_replaces_what_was_placed_before() {
        let mut registry = Registry::default();
        registry.start(agent("one", Some(1)));
        let mut tabs = tabs();
        place(&mut tabs, &registry);
        registry.end(&session("one"));
        place(&mut tabs, &registry);
        assert!(tabs[0].panes[0].agents.is_empty());
    }
}
