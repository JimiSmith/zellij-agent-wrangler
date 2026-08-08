//! The agent sessions the sidebar knows about, and where in the tree they sit.
//!
//! A record arrives from the agent's own lifecycle hooks, which report the pane
//! they were invoked in. The sidebar files them by session id and hands each one
//! to the pane it named; a record whose pane is not on screen is held but drawn
//! nowhere, so an agent in a pane that has gone leaves no row behind.

use std::collections::BTreeMap;

use crate::model::{NamedColor, SessionId};
use crate::options::Label;
use crate::tree::Tab;

/// The character between a record's fields, and the one between records. Both
/// are excluded from every field, so a run of records splits exactly.
const FIELD: char = '\t';
const RECORD: char = '\n';

/// The shape of a record, which every one of them leads with.
///
/// The two halves are installed separately and updated separately, so one of
/// them can be older than the other. Saying which shape a record is written in
/// is what turns that into something the sidebar can report rather than a run of
/// records it silently makes nothing of.
pub const FORMAT: u32 = 2;

/// The pipes an agent's hooks report a session on. Each carries one whole
/// record: what a session calls itself changes under it, so every event is a
/// chance to say so, and only the message name says which event it was.
///
/// The one exception is the end of a session, which names it and nothing else:
/// there is no state left to describe.
pub const START_MESSAGE: &str = "wrangler:agent-start";
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

/// What a session is called by, as the client found it. Every field is empty
/// until something says otherwise: an agent that has not titled itself yet, and
/// one that never will, look the same from here.
///
/// These are what the row is *composed from* rather than the row's text, so the
/// sidebar can be told to spell a label differently without the agents having
/// to report themselves again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// The name of the directory the agent is working in.
    pub dir: String,
    /// The agent's own name when it is a teammate of another session, which is
    /// what tells the two apart.
    pub name: String,
    /// The color the agent shows for this session, by the agent's own name for
    /// it. Empty for a session with none, and for an agent that assigns none.
    pub color: String,
    /// The title the session gave itself.
    pub title: String,
}

/// One agent session: who it is, what it calls itself, the pane it reported
/// itself from, and whose turn it is.
///
/// Every string is trimmed of anything that could split the wire format, which
/// is why the fields are built rather than assigned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub session: SessionId,
    pub agent: String,
    pub meta: Meta,
    pub pane: Option<u32>,
    pub turn: Turn,
    /// When the agent last called for the user, as the client read the clock.
    /// It comes from the one process that sees each call once, so every sidebar
    /// orders the calls the same way without comparing clocks of its own.
    pub raised: u64,
}

/// Replace every character that would split a record or a field, and every
/// control character, with a space.
fn field(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl Agent {
    pub fn new(session: SessionId, agent: &str, meta: Meta, pane: Option<u32>) -> Self {
        Agent {
            session,
            agent: field(agent),
            meta: Meta {
                dir: field(&meta.dir),
                name: field(&meta.name),
                color: field(&meta.color),
                title: field(&meta.title),
            },
            pane,
            turn: Turn::default(),
            raised: 0,
        }
    }

    /// What this session's row says, spelled the way the sidebar was asked to
    /// spell it.
    ///
    /// A teammate leads with its own name, so it is never mistaken for a
    /// session of its own. An untitled session falls back to where it is
    /// working, and one that cannot say even that falls back to what it is.
    pub fn label(&self, mode: Label) -> String {
        let dir = match self.meta.dir.is_empty() {
            true => self.agent.as_str(),
            false => self.meta.dir.as_str(),
        };
        let title = self.meta.title.as_str();
        match (mode, self.meta.name.as_str()) {
            (Label::Name, "") if !title.is_empty() => title.to_string(),
            (_, "") => dir.to_string(),
            (Label::Name, name) if title.is_empty() => format!("@{name}"),
            (Label::Name, name) => format!("@{name} - {title}"),
            (Label::Dir, name) => format!("@{name} - {dir}"),
        }
    }

    /// The color this session's icon is drawn in, or `None` for one the agent
    /// gives no color to.
    pub fn color(&self) -> Option<NamedColor> {
        NamedColor::agent(&self.meta.color)
    }

    /// The record as one line: the format, then session, agent, pane, turn,
    /// raised, and the four things it is known by. The pane is written as
    /// nothing at all when the agent reported none.
    pub fn encode(&self) -> String {
        let pane = self.pane.map(|id| id.to_string()).unwrap_or_default();
        format!(
            "{FORMAT}{FIELD}{}{FIELD}{}{FIELD}{pane}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}{FIELD}{}",
            self.session.as_str(),
            self.agent,
            self.turn.encode(),
            self.raised,
            self.meta.dir,
            self.meta.name,
            self.meta.color,
            self.meta.title,
        )
    }

    /// What a line on one of the agent pipes turned out to be.
    ///
    /// The title is the whole remainder of the line, so a title carrying the
    /// field character would still parse; it cannot, because the constructor
    /// takes that character out.
    pub fn decode(line: &str) -> Record {
        let mut fields = line.splitn(10, FIELD);
        match fields.next().and_then(|format| format.parse::<u32>().ok()) {
            Some(FORMAT) => {}
            Some(other) => return Record::Foreign(other),
            None => return Record::None,
        }
        match Agent::read(fields) {
            Some(agent) => Record::Known(agent),
            None => Record::None,
        }
    }

    /// A record's fields, after the one saying which format they are in.
    fn read<'a>(mut fields: impl Iterator<Item = &'a str>) -> Option<Self> {
        let session = SessionId::new(fields.next()?)?;
        let agent = fields.next()?;
        let pane = fields.next()?;
        let turn = Turn::decode(fields.next()?)?;
        let raised = fields.next()?.parse().ok()?;
        let meta = Meta {
            dir: fields.next()?.to_string(),
            name: fields.next()?.to_string(),
            color: fields.next()?.to_string(),
            title: fields.next()?.to_string(),
        };
        let pane = match pane.is_empty() {
            true => None,
            false => Some(pane.parse().ok()?),
        };
        Some(Agent {
            turn,
            raised,
            ..Agent::new(session, agent, meta, pane)
        })
    }
}

/// What a line arriving on one of the agent pipes turned out to be.
///
/// A record written in another format is told from a line that is not a record
/// at all, because the two mean different things: the first says the halves of
/// this plugin are out of step with each other, and the second says nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Known(Agent),
    Foreign(u32),
    None,
}

/// Every agent session the sidebar has been told about, newest state per
/// session.
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
    ///
    /// A session nobody saw start is filed here rather than dropped: a record
    /// carries everything needed to draw a row, so the sidebar can pick an
    /// agent up from any event it happens to hear first.
    pub fn report(&mut self, agent: Agent) -> bool {
        self.merge(agent)
    }

    /// File a record, keeping whatever it leaves blank that is already known.
    ///
    /// A client reports what it can see at the time, and a title it could not
    /// find this time is not a title withdrawn. `true` when this changed
    /// anything.
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
        for line in text.split(RECORD) {
            if let Record::Known(agent) = Agent::decode(line) {
                changed |= self.merge(agent);
            }
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

    fn meta(dir: &str, name: &str, title: &str) -> Meta {
        Meta {
            dir: dir.to_string(),
            name: name.to_string(),
            color: String::new(),
            title: title.to_string(),
        }
    }

    fn colored(id: &str, color: &str) -> Agent {
        Agent::new(
            session(id),
            "claude",
            Meta {
                color: color.to_string(),
                ..meta("wrangler", "", "")
            },
            Some(1),
        )
    }

    fn agent(id: &str, pane: Option<u32>) -> Agent {
        Agent::new(session(id), "claude", meta("wrangler", "", ""), pane)
    }

    /// The record an agent's hooks send when the turn changes: everything the
    /// session is, plus whose turn it now is.
    fn reporting(id: &str, pane: Option<u32>, turn: Turn, raised: u64) -> Agent {
        Agent {
            turn,
            raised,
            ..agent(id, pane)
        }
    }

    #[test]
    fn a_record_survives_the_round_trip() {
        for record in [agent("one", Some(3)), agent("two", None)] {
            assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
        }
    }

    #[test]
    fn a_title_cannot_split_the_record_it_sits_at_the_end_of() {
        let record = Agent::new(session("one"), "claude", meta("d", "", "a\tb\nc"), Some(1));
        assert_eq!(record.meta.title, "a b c");
        assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
    }

    #[test]
    fn a_line_that_is_not_a_record_decodes_to_nothing() {
        for line in [
            "",
            "one",
            "2\tone\tclaude",
            "2\tone\tclaude\t1\tidle\t0\tdir",
            "2\tone\tclaude\tx\tidle\t0\tdir\t\t\ttitle",
        ] {
            assert_eq!(Agent::decode(line), Record::None, "{line}");
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
            let record = reporting("one", Some(3), turn, 7);
            assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
        }
    }

    #[test]
    fn a_record_with_no_turn_it_recognises_decodes_to_nothing() {
        assert_eq!(
            Agent::decode("2\tone\tclaude\t3\tdozing\t0\tdir\t\t\t"),
            Record::None
        );
    }

    #[test]
    fn a_record_written_in_another_format_says_which_one() {
        // The two halves are installed separately, so one can be older than the
        // other, and a record it wrote is not a line to pass over in silence.
        let record = agent("one", Some(3)).encode();
        let older = record.replacen(&FORMAT.to_string(), "0", 1);
        assert_eq!(Agent::decode(&older), Record::Foreign(0));
        // Absorbing takes nothing from a format it does not know.
        let mut registry = Registry::default();
        assert!(!registry.absorb(&older));
    }

    #[test]
    fn a_report_files_a_session_nobody_saw_start() {
        // A record says everything a row needs, so the sidebar can pick an
        // agent up from whichever of its events it happens to hear first.
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
        // The client reports what it can find at the time, and a title it could
        // not find this time is not a title withdrawn.
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

    fn labelled(dir: &str, name: &str, title: &str) -> Agent {
        Agent::new(session("one"), "claude", meta(dir, name, title), Some(1))
    }

    #[test]
    fn a_session_is_drawn_in_the_color_the_agent_gives_it() {
        // The two an agent names that a terminal does not are drawn in the
        // bright form of their neighbour, so all eight stay apart.
        for (name, color) in [
            ("red", NamedColor::Red),
            ("green", NamedColor::Green),
            ("yellow", NamedColor::Yellow),
            ("blue", NamedColor::Blue),
            ("purple", NamedColor::Magenta),
            ("cyan", NamedColor::Cyan),
            ("orange", NamedColor::BrightYellow),
            ("pink", NamedColor::BrightMagenta),
        ] {
            assert_eq!(colored("one", name).color(), Some(color), "{name}");
        }
    }

    #[test]
    fn a_session_with_no_color_of_its_own_is_drawn_in_none() {
        assert_eq!(colored("one", "").color(), None);
        // A name this sidebar does not know is not a color to guess at.
        assert_eq!(colored("one", "chartreuse").color(), None);
    }

    #[test]
    fn a_color_survives_the_round_trip() {
        let record = colored("one", "purple");
        assert_eq!(Agent::decode(&record.encode()), Record::Known(record));
    }

    #[test]
    fn a_titled_session_is_called_by_its_title_and_an_untitled_one_by_its_directory() {
        let titled = labelled("wrangler", "", "the zellij port");
        assert_eq!(titled.label(Label::Name), "the zellij port");
        assert_eq!(titled.label(Label::Dir), "wrangler");
        let untitled = labelled("wrangler", "", "");
        assert_eq!(untitled.label(Label::Name), "wrangler");
        assert_eq!(untitled.label(Label::Dir), "wrangler");
    }

    #[test]
    fn a_session_that_can_say_neither_is_called_by_what_it_is() {
        let anonymous = labelled("", "", "");
        assert_eq!(anonymous.label(Label::Name), "claude");
        assert_eq!(anonymous.label(Label::Dir), "claude");
    }

    #[test]
    fn a_teammate_leads_with_its_own_name_whatever_it_is_called_by() {
        let teammate = labelled("wrangler", "scout", "reading the source");
        assert_eq!(teammate.label(Label::Name), "@scout - reading the source");
        assert_eq!(teammate.label(Label::Dir), "@scout - wrangler");
        // A teammate with nothing to say for itself is still told apart from a
        // session of its own.
        assert_eq!(
            labelled("wrangler", "scout", "").label(Label::Name),
            "@scout"
        );
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
