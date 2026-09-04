//! The row vocabulary a client is drawn from.
//!
//! The content of a row names *what* the row is. The `Branch` and the
//! `Placement` follow from the place of the row. The `Indicator` follows from
//! the turn state of the thing the row points at. The strings of a row are the
//! literal names of things, and the client chooses every glyph around them when
//! it draws the row.
//!
//! Nothing here knows what a terminal is. A row says what to draw, and never how
//! to draw it.

use std::collections::BTreeSet;

use agent_wrangler_core::agent::{Agent, SessionId, Turn};
use agent_wrangler_core::registry::Registry;

/// The stable name a multiplexer gives a pane.
///
/// Multiplexers do not agree on the shape of a pane id. The native id stays
/// opaque text, so every adapter can keep it without a second identity of its
/// own.
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

/// The stable name a multiplexer gives a tab.
///
/// If another tab opens or closes, the position of a tab can change. This id
/// names the tab itself. [`TabPosition`] is the metadata that orders and labels
/// the tab.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TabId(String);

impl TabId {
    pub fn new(id: impl Into<String>) -> Self {
        TabId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The place of a tab in the tab bar, counted from zero.
///
/// A position is not a name. If a tab closes, every tab after it moves along.
/// The tab at a position is therefore only the tab that was there while nothing
/// before it opened or closed. A position orders the tabs, labels their rows,
/// and goes to host APIs that can address tabs only by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabPosition(usize);

impl TabPosition {
    /// The tab at this place in the bar.
    pub const fn at(position: usize) -> Self {
        TabPosition(position)
    }

    /// The place itself, for whoever else counts tabs from zero.
    pub const fn zero_based(self) -> usize {
        self.0
    }

    /// The number the user calls this tab by, which counts from one. Tabs are
    /// the one thing the user sees numbered that way.
    pub const fn one_based(self) -> usize {
        self.0 + 1
    }
}

/// The position of a child in its tab. The last child closes the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    More,
    Last,
}

/// The columns that one level of a [`RowStem`] takes.
///
/// The builder narrows the AGENT cell by the width of the stem, and the drawing
/// spends exactly that width. Both read this constant, so the two cannot drift
/// apart and every column after AGENT stays aligned.
pub const STEM_COLUMNS_PER_LEVEL: usize = 2;

/// How far under its lead a dashboard row sits, and what the tree draws at each
/// level of that depth.
///
/// The last entry is the branch of the row itself. Every entry before it says
/// whether that ancestor has a later sibling, which decides whether the line
/// carries on down past this row. An agent that no agent started carries no
/// entry at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowStem(Vec<Branch>);

impl RowStem {
    /// The stem of a row at this depth, outermost level first.
    pub fn new(levels: Vec<Branch>) -> Self {
        RowStem(levels)
    }

    /// The levels, outermost first. The drawing reads them in this order.
    pub fn levels(&self) -> &[Branch] {
        &self.0
    }

    /// The columns that this stem takes.
    pub fn columns(&self) -> usize {
        self.0.len() * STEM_COLUMNS_PER_LEVEL
    }

    /// The same stem, held to `columns`.
    ///
    /// The stem and the AGENT cell sum to one width on every row. A stem deeper
    /// than the whole AGENT column therefore pushes every column after AGENT out
    /// of place, and the table widens because a child appeared.
    ///
    /// The outermost levels are the ones that go. The last level is the branch
    /// of the row itself, and that branch says that the row hangs under
    /// something.
    pub fn held_to(&self, columns: usize) -> RowStem {
        let room = columns / STEM_COLUMNS_PER_LEVEL;
        let dropped = self.0.len().saturating_sub(room);
        RowStem(self.0[dropped..].to_vec())
    }
}

/// Whether the block under a dashboard row is drawn. The row marks which it is,
/// so a user knows that a closed row has something to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPreview {
    Open,
    Closed,
}

/// The place of a row relative to the user. A client reads both the gutter and
/// the intensity of the row off this one channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The focused pane of the focused tab, or that tab itself.
    FocusedPane,
    /// Another pane of the tab that the user is looking at. This pane is not
    /// the focused one.
    SameTab,
    /// Under a tab that the user is not looking at.
    OtherTab,
}

impl Placement {
    pub fn is_focused_pane(self) -> bool {
        matches!(self, Placement::FocusedPane)
    }
}

/// The emphasis one run of text draws with.
///
/// The fields carry terminal effects rather than markdown roles, because
/// several roles take one effect. A heading and a bold word are both bold, and
/// a quote and a code span are both dim. The client decides what each effect
/// looks like, the way it decides what a color name looks like.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextEmphasis {
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub underlined: bool,
    pub crossed_out: bool,
}

/// One run of text in a row, and the emphasis it draws with. A row carries runs
/// rather than one string when markdown emphasises part of its text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextRun {
    pub text: String,
    pub emphasis: TextEmphasis,
}

impl TextRun {
    /// A run that nothing emphasises.
    pub fn plain(text: impl Into<String>) -> TextRun {
        TextRun {
            text: text.into(),
            emphasis: TextEmphasis::default(),
        }
    }
}

/// The palette the identity of a thing is drawn from. The colors carry terminal
/// names rather than RGB values, so the theme of the user decides how they look.
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
    /// The terminal color a color name from an agent is drawn in, or `None` for
    /// a session with no color of its own.
    ///
    /// An agent names eight colors, and a terminal has six colors it can count
    /// on. The two colors with no name of their own are drawn in the bright form
    /// of their nearest neighbor. That keeps eight sessions eight colors apart,
    /// which is the whole purpose of the color.
    pub fn from_color_word(name: &str) -> Option<Self> {
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
    pub fn for_agent(agent: &Agent) -> Option<Self> {
        NamedColor::from_color_word(&agent.meta.color)
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
    /// The glyph this marker draws, and the color the marker carries of its own.
    /// A `None` color leaves the style of the row unchanged.
    ///
    /// Both glyphs are one column wide and share a shape, so the two states read
    /// as one channel. The glyph is filled for the agent that wants you, and
    /// hollow for the agent that still works.
    pub fn glyph_and_color(self) -> Option<(char, Option<NamedColor>)> {
        match self {
            Indicator::None => None,
            Indicator::Attention => Some(('●', Some(NamedColor::Yellow))),
            Indicator::Working => Some(('○', None)),
        }
    }
}

/// What a selectable row points at.
///
/// The selection travels as one of these rather than as a position. If a pane
/// opens or closes above the selected row, the row moves and the selection stays
/// on the thing it was on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKey {
    Tab(TabId),
    Pane(PaneId),
    Agent(SessionId),
    /// The same session as [`RowKey::Agent`], drawn a second time under the
    /// agent it belongs to rather than under the tab it is in. It is a kind of
    /// its own because a key must name one row. Two rows with one key both draw
    /// as selected, and the selection never reaches the second row.
    Section(SessionId),
    /// An entry in the notification area. It names the same session that an
    /// agent row names. It is a separate kind. When the user opens this entry,
    /// the client can do more than the selection of an agent row does.
    Notification(SessionId),
}

impl RowKey {
    /// The key as one line of text, which is how it travels between the
    /// clients that share a selection.
    pub fn encode(&self) -> String {
        match self {
            RowKey::Tab(id) => format!("tab-id:{}", id.as_str()),
            RowKey::Pane(id) => format!("pane:{}", id.as_str()),
            RowKey::Agent(session) => format!("agent:{}", session.as_str()),
            RowKey::Section(session) => format!("section:{}", session.as_str()),
            RowKey::Notification(session) => format!("notification:{}", session.as_str()),
        }
    }

    /// The key `encode` wrote, or `None` for anything else. A client that runs
    /// older code than the sender gives `None` rather than a guess.
    pub fn decode(text: &str) -> Option<Self> {
        let (kind, value) = text.split_once(':')?;
        match kind {
            "tab-id" if !value.is_empty() => Some(RowKey::Tab(TabId::new(value))),
            "pane" if !value.is_empty() => Some(RowKey::Pane(PaneId::new(value))),
            "agent" => SessionId::new(value).map(RowKey::Agent),
            "section" => SessionId::new(value).map(RowKey::Section),
            "notification" => SessionId::new(value).map(RowKey::Notification),
            _ => None,
        }
    }
}

/// The agents whose preview block is drawn under their dashboard row.
///
/// One sidebar keeps its own. Nothing broadcasts this set, because a block
/// changes the height of the table. A shared set would scroll the sidebar of
/// every other tab whenever the user opened a row in one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenPreviews(BTreeSet<SessionId>);

impl OpenPreviews {
    /// Whether this agent's block is drawn.
    pub fn holds(&self, session: &SessionId) -> bool {
        self.0.contains(session)
    }

    /// Open this agent's block, or close it when it is already open.
    pub fn open_or_close(&mut self, session: &SessionId) {
        if !self.0.remove(session) {
            self.0.insert(session.clone());
        }
    }

    /// Forget every session that the registry no longer holds.
    ///
    /// An agent that the daemon stops reporting takes its open block with it.
    /// Without this, the set grows for as long as the sidebar runs.
    pub fn drop_gone_sessions(&mut self, registry: &Registry) {
        self.0.retain(|session| registry.get(session).is_some());
    }
}

/// Which edge of its columns a cell's text sits against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellAlignment {
    Left,
    Right,
}

/// One cell of the dashboard table: the text it holds, and the columns that
/// the text is padded to.
///
/// The builder fits the text to `width` and marks a cut. The drawing pads the
/// text and never shortens it. Without that rule, a long value pushes every
/// cell after it out of its column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCell {
    pub text: String,
    pub width: usize,
    pub alignment: CellAlignment,
}

/// What a row is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowContent {
    Header {
        text: String,
    },
    Blank,
    Tab {
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
    /// The status line that hangs under an agent's row.
    ///
    /// The row starts its text in the column where the label of that agent
    /// starts, and it draws under the same branch. The tree therefore reads as
    /// one structure, and the indent says that the line describes the row above
    /// it.
    ///
    /// The row carries the `index` and the `branch` of that agent row rather
    /// than a copy of its indent. Both rows then follow one rule, and neither
    /// can drift.
    AgentStatus {
        index: String,
        text: String,
        branch: Branch,
        placement: Placement,
    },
    NotificationTitle {
        title: String,
        color: Option<NamedColor>,
    },
    NotificationBody {
        text: String,
    },
    /// The row of column headings over the dashboard table.
    ///
    /// The row carries the same cell widths as every agent row under it, so a
    /// heading sits over the column it names and the two cannot drift apart.
    DashboardHeading {
        /// The AGENT heading, which sits over the kind icon and the name
        /// together.
        name: TableCell,
        /// The headings from TURN onward, in the order they draw.
        cells: Vec<TableCell>,
    },
    /// One agent as one row of the dashboard table.
    ///
    /// The row carries two channels that the tree carries as one. `placement`
    /// reaches the gutter alone, and `turn` decides how brightly the row
    /// draws. The dashboard orders by urgency, so intensity says urgency
    /// there.
    DashboardAgent {
        /// Whether the user is in this agent's pane, which the gutter marks.
        placement: Placement,
        /// How far under its lead this row sits. The stem draws before the kind
        /// icon, and the AGENT cell is already narrowed by its width.
        stem: RowStem,
        /// Whose turn it is, which decides how brightly the row draws.
        turn: Turn,
        color: Option<NamedColor>,
        /// Whether the block under this row is drawn, which the disclosure
        /// marker says.
        preview: RowPreview,
        /// The AGENT column, which the kind icon leads.
        name: TableCell,
        /// The columns from TURN onward, in the order they draw.
        cells: Vec<TableCell>,
    },
    /// One line of what an agent last told the user, in the block that the
    /// space key opens under its row.
    ///
    /// A block that reports no message draws one such line that says so. A row
    /// that opened and drew nothing leaves the user to guess why.
    PreviewMessage {
        /// The placement of the row that the block hangs from. Every line of
        /// the block carries it, so the gutter does not break in the middle of
        /// one pane.
        placement: Placement,
        /// The stem of the row that the block hangs from. Every level draws a
        /// continuation, so the tree reads as one structure through the block.
        stem: RowStem,
        /// Whether more of the block follows this line. The last line closes
        /// the tree that the block hangs from.
        branch: Branch,
        /// The line, in the runs that the markdown of the message divided it
        /// into. A line that markdown says nothing about is one plain run.
        runs: Vec<TextRun>,
    },
    /// When the agent wrote that message, in the block under its row.
    PreviewTime {
        placement: Placement,
        stem: RowStem,
        branch: Branch,
        text: String,
    },
    /// The tool that the agent runs now, in the block under its row.
    PreviewTool {
        placement: Placement,
        stem: RowStem,
        branch: Branch,
        text: String,
    },
    /// The dashboard draws no table, because no agent is running.
    DashboardNoAgents,
    /// The dashboard draws no table, because the pane is too narrow for the
    /// AGENT column.
    DashboardPaneTooNarrow,
}

/// The call of an agent for attention, as the notification area lists it. The
/// entry holds the agent that raised the call, and the message that describes
/// where the agent is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The agent session this entry points at. When the user opens the entry,
    /// the entry goes to this session. Two entries never name one session.
    pub session: SessionId,
    pub agent_program: String,
    pub color: Option<NamedColor>,
    pub message: String,
}

/// A drawn row: the content, the marker pinned to the right edge, and the thing
/// the selection lands on at this row. A row with no key is not selectable and
/// not clickable.
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

    pub fn with_key(mut self, key: RowKey) -> Self {
        self.key = Some(key);
        self
    }

    pub fn with_indicator(mut self, indicator: Indicator) -> Self {
        self.indicator = indicator;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use agent_wrangler_core::agent::LabelFacts;
    use agent_wrangler_core::origin::Origin;

    fn colored(color: &str) -> Agent {
        Agent::new(
            SessionId::new("one").unwrap(),
            "claude",
            LabelFacts {
                color: color.to_string(),
                ..LabelFacts::default()
            },
            Origin::default(),
        )
    }

    #[test]
    fn a_session_is_drawn_in_the_color_the_agent_gives_it() {
        // An agent names two colors that a terminal does not name. Those two
        // are drawn in the bright form of their neighbor, so all eight stay
        // apart.
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
            assert_eq!(NamedColor::for_agent(&colored(name)), Some(want), "{name}");
        }
    }

    #[test]
    fn a_session_with_no_color_of_its_own_is_drawn_in_none() {
        assert_eq!(NamedColor::for_agent(&colored("")), None);
        // A name this palette does not hold is not a color to guess at.
        assert_eq!(NamedColor::for_agent(&colored("chartreuse")), None);
    }

    #[test]
    fn a_key_survives_the_round_trip() {
        for key in [
            RowKey::Tab(TabId::new("0")),
            RowKey::Tab(TabId::new("tab-12")),
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
            "", "tab", "tab:", "tab:0", "tab-id:", "pane:", "agent:", "window:1", "1",
        ] {
            assert_eq!(RowKey::decode(text), None, "{text}");
        }
    }

    #[test]
    fn a_sanitized_id_is_the_one_that_comes_back() {
        // The constructor sanitizes an id on the way in, which makes the round
        // trip total. An id decoded from the wire already has the shape the
        // constructor allows.
        let key = RowKey::Agent(SessionId::new("a/b").unwrap());
        assert_eq!(RowKey::decode(&key.encode()), Some(key));
    }
}
