//! The row vocabulary the sidebar is drawn from.
//!
//! A row's content names *what* the row is; its `Branch` and `Placement` follow
//! from where it sits, and its `Indicator` from the turn state of the thing it
//! points at. A row's strings are the literal names of things, and every glyph
//! drawn around them is chosen when the row is painted.

/// A child's position in its tab: the last one closes the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    More,
    Last,
}

/// Where a row sits relative to the user: the one channel the sidebar reads
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
#[allow(dead_code)]
pub enum NamedColor {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

impl NamedColor {
    /// The SGR foreground parameter for this color.
    pub fn sgr(self) -> u8 {
        match self {
            NamedColor::Red => 31,
            NamedColor::Green => 32,
            NamedColor::Yellow => 33,
            NamedColor::Blue => 34,
            NamedColor::Magenta => 35,
            NamedColor::Cyan => 36,
        }
    }
}

/// The right-edge turn-state marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indicator {
    None,
    /// The agent wants you.
    Attention,
    /// The agent is mid-turn.
    #[allow(dead_code)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKey {
    Tab(usize),
    Pane(u32),
    Notification(usize),
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
    #[allow(dead_code)]
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
