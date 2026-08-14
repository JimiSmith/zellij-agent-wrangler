//! The row vocabulary a client is drawn from.
//!
//! A row's content names *what* the row is; its `Branch` and `Placement` follow
//! from where it sits, and its `Indicator` from the turn state of the thing it
//! points at. A row's strings are the literal names of things, and every glyph
//! drawn around them is chosen when the row is painted.
//!
//! Nothing here knows what a terminal is: a row says what is to be drawn, and
//! never how.

use agent_wrangler_core::agent::{Agent, SessionId};

/// A multiplexer pane's own stable name.
///
/// Multiplexers do not agree on the shape of pane ids. Keeping the native id
/// as opaque text lets every adapter preserve it without allocating a second
/// identity of its own.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PaneId(String);

impl PaneId {
    pub fn new(id: impl Into<String>) -> Self {
        PaneId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<u32> for PaneId {
    fn from(id: u32) -> Self {
        PaneId::new(id.to_string())
    }
}

/// Where a tab sits in the tab bar, counted from zero.
///
/// A position is not a name: closing a tab moves every tab after it along, so
/// the tab at a position is only the tab that was there while nothing before it
/// has opened or closed. It is what orders the tabs, what labels their rows, and
/// what a client means when it points at one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabPosition(usize);

impl TabPosition {
    /// The tab sitting at this place in the bar.
    pub const fn at(position: usize) -> Self {
        TabPosition(position)
    }

    /// The place itself, for whoever else counts tabs from zero.
    pub const fn zero_based(self) -> usize {
        self.0
    }

    /// The number this tab is called by, which counts from one: tabs are the one
    /// thing the user is shown numbered that way.
    pub const fn one_based(self) -> usize {
        self.0 + 1
    }
}

/// A child's position in its tab: the last one closes the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    More,
    Last,
}

/// Where a row sits relative to the user: the one channel a client reads
/// both the gutter and the row's intensity off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The focused pane of the focused tab, or that tab itself.
    Here,
    /// Elsewhere in the tab you are in.
    Focused,
    /// Under a tab you are not in.
    Unfocused,
}

impl Placement {
    pub fn here(self) -> bool {
        matches!(self, Placement::Here)
    }
}

/// The palette a thing's identity is drawn from. Terminal-named rather than
/// RGB, so the user's own theme decides what the colors look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedColor {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    BrightYellow,
    BrightMagenta,
}

impl NamedColor {
    /// The terminal color an agent's own color name is drawn in, or `None` for
    /// a session with no color of its own.
    ///
    /// An agent names eight colors and a terminal has six it can count on, so
    /// the two with no name of their own are drawn in the bright form of their
    /// nearest neighbour. That is what keeps eight sessions eight colors apart,
    /// which is the whole of what the color is for.
    pub fn agent(name: &str) -> Option<Self> {
        Some(match name {
            "red" => NamedColor::Red,
            "green" => NamedColor::Green,
            "yellow" => NamedColor::Yellow,
            "blue" => NamedColor::Blue,
            "purple" => NamedColor::Magenta,
            "cyan" => NamedColor::Cyan,
            "orange" => NamedColor::BrightYellow,
            "pink" => NamedColor::BrightMagenta,
            _ => return None,
        })
    }

    /// The color one session is drawn in, which is the color the agent gave it
    /// and nothing the client chose.
    pub fn of(agent: &Agent) -> Option<Self> {
        NamedColor::agent(&agent.meta.color)
    }
}

/// The right-edge turn-state marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indicator {
    None,
    /// The agent wants you.
    Attention,
    /// The agent is mid-turn.
    Working,
}

impl Indicator {
    /// The glyph this marker draws, and the color it carries of its own. A
    /// `None` color leaves the row's own style standing.
    ///
    /// Both glyphs are one column wide and share a shape, so the two states read
    /// as one channel: filled for the agent that wants you, hollow for the one
    /// still going.
    pub fn resolve(self) -> Option<(char, Option<NamedColor>)> {
        match self {
            Indicator::None => None,
            Indicator::Attention => Some(('●', Some(NamedColor::Yellow))),
            Indicator::Working => Some(('○', None)),
        }
    }
}

/// What a selectable row points at.
///
/// The selection is carried as one of these rather than as a position, so a
/// pane opening or closing above the selected row moves the row without moving
/// the selection off the thing it was on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKey {
    Tab(TabPosition),
    Pane(PaneId),
    Agent(SessionId),
    /// The same session as [`RowKey::Agent`], drawn a second time under the
    /// agent it belongs to rather than under the tab it is in. It is a kind of
    /// its own because a key has to name one row: two rows sharing one would
    /// both be drawn selected, and the selection could never reach the second.
    Section(SessionId),
    /// An entry in the notification area. It names the same session an agent
    /// row does, and is a separate kind so that opening the entry can do more
    /// than selecting the agent does.
    Notification(SessionId),
}

impl RowKey {
    /// The key as one line of text, which is how it travels between the
    /// clients sharing a selection.
    pub fn encode(&self) -> String {
        match self {
            RowKey::Tab(position) => format!("tab:{}", position.zero_based()),
            RowKey::Pane(id) => format!("pane:{}", id.as_str()),
            RowKey::Agent(session) => format!("agent:{}", session.as_str()),
            RowKey::Section(session) => format!("section:{}", session.as_str()),
            RowKey::Notification(session) => format!("notification:{}", session.as_str()),
        }
    }

    /// The key `encode` wrote, or `None` for anything else. A client running
    /// older code than the one that sent this says nothing rather than guessing.
    pub fn decode(text: &str) -> Option<Self> {
        let (kind, value) = text.split_once(':')?;
        match kind {
            "tab" => value
                .parse()
                .ok()
                .map(|position| RowKey::Tab(TabPosition::at(position))),
            "pane" if !value.is_empty() => Some(RowKey::Pane(PaneId::new(value))),
            "agent" => SessionId::new(value).map(RowKey::Agent),
            "section" => SessionId::new(value).map(RowKey::Section),
            "notification" => SessionId::new(value).map(RowKey::Notification),
            _ => None,
        }
    }
}

/// What a row is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowContent {
    Header {
        text: String,
    },
    Blank,
    Window {
        index: String,
        name: String,
        placement: Placement,
        color: Option<NamedColor>,
    },
    Pane {
        index: String,
        title: String,
        branch: Branch,
        placement: Placement,
        color: Option<NamedColor>,
    },
    Agent {
        index: String,
        label: String,
        branch: Branch,
        placement: Placement,
        color: Option<NamedColor>,
    },
    NotificationTitle {
        title: String,
        color: Option<NamedColor>,
    },
    NotificationBody {
        text: String,
    },
}

/// An agent's call for attention, as the notification area lists it: the agent
/// that raised it, and the message describing where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The agent session this entry points at, which is what opening it goes
    /// to and what stops the same session being listed twice.
    pub session: SessionId,
    pub agent: String,
    pub color: Option<NamedColor>,
    pub message: String,
}

/// A drawn line: its content, the marker pinned to its right edge, and the thing
/// the selection lands on when it is here. A row with no key cannot be selected
/// or clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub content: RowContent,
    pub indicator: Indicator,
    pub key: Option<RowKey>,
}

impl Row {
    /// An inert row: a heading, a blank, or anything else that labels rather
    /// than points.
    pub fn new(content: RowContent) -> Self {
        Row {
            content,
            indicator: Indicator::None,
            key: None,
        }
    }

    pub fn at(mut self, key: RowKey) -> Self {
        self.key = Some(key);
        self
    }

    pub fn with(mut self, indicator: Indicator) -> Self {
        self.indicator = indicator;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use agent_wrangler_core::agent::Meta;
    use agent_wrangler_core::origin::Origin;

    fn colored(color: &str) -> Agent {
        Agent::new(
            SessionId::new("one").unwrap(),
            "claude",
            Meta {
                color: color.to_string(),
                ..Meta::default()
            },
            Origin::default(),
        )
    }

    #[test]
    fn a_session_is_drawn_in_the_color_the_agent_gives_it() {
        // The two an agent names that a terminal does not are drawn in the
        // bright form of their neighbour, so all eight stay apart.
        for (name, want) in [
            ("red", NamedColor::Red),
            ("green", NamedColor::Green),
            ("yellow", NamedColor::Yellow),
            ("blue", NamedColor::Blue),
            ("purple", NamedColor::Magenta),
            ("cyan", NamedColor::Cyan),
            ("orange", NamedColor::BrightYellow),
            ("pink", NamedColor::BrightMagenta),
        ] {
            assert_eq!(NamedColor::of(&colored(name)), Some(want), "{name}");
        }
    }

    #[test]
    fn a_session_with_no_color_of_its_own_is_drawn_in_none() {
        assert_eq!(NamedColor::of(&colored("")), None);
        // A name this palette does not hold is not a color to guess at.
        assert_eq!(NamedColor::of(&colored("chartreuse")), None);
    }

    #[test]
    fn a_key_survives_the_round_trip() {
        for key in [
            RowKey::Tab(TabPosition::at(0)),
            RowKey::Tab(TabPosition::at(12)),
            RowKey::Pane(7.into()),
            RowKey::Pane(PaneId::new("%7")),
            RowKey::Agent(SessionId::new("9f3c-1a").unwrap()),
            RowKey::Section(SessionId::new("9f3c-1a").unwrap()),
            RowKey::Notification(SessionId::new("9f3c-1a").unwrap()),
        ] {
            assert_eq!(RowKey::decode(&key.encode()), Some(key));
        }
    }

    #[test]
    fn anything_else_decodes_to_nothing() {
        for text in [
            "", "tab", "tab:", "tab:x", "pane:", "agent:", "window:1", "1",
        ] {
            assert_eq!(RowKey::decode(text), None, "{text}");
        }
    }

    #[test]
    fn a_sanitized_id_is_the_one_that_comes_back() {
        // Sanitizing on the way in is what makes the round trip total: an id
        // decoded from the wire is already the shape the constructor allows.
        let key = RowKey::Agent(SessionId::new("a/b").unwrap());
        assert_eq!(RowKey::decode(&key.encode()), Some(key));
    }
}
