//! How the rows of the dashboard are built from the tabs of the session and
//! their panes.
//!
//! The input is the same session that [`build_tree`] takes. What comes out is
//! one row for every agent, and no row for a tab or a pane. The rows order by
//! urgency rather than by place, and every fact takes a column of its own.
//!
//! The builder takes the width, exactly as [`build_frame`] takes it, and for
//! the same reason: a column either fits or it does not. The whole frame is
//! composed again every draw, so a width-dependent build costs nothing.
//!
//! [`build_tree`]: crate::tree::build_tree
//! [`build_frame`]: crate::frame::build_frame

use std::collections::{BTreeMap, BTreeSet};

use agent_wrangler_core::agent::{Agent, SessionId, Turn};
use agent_wrangler_core::label::label;
use agent_wrangler_core::preview::{Preview, ToolCall};
use agent_wrangler_core::status_line::{short_model_name, short_token_count};

use crate::markdown::clipped_message_lines;
use crate::model::{
    Branch, CellAlignment, NamedColor, OpenPreviews, Placement, Row, RowContent, RowKey,
    RowPreview, RowStem, TableCell, TextRun,
};
use crate::options::DrawingOptions;
use crate::render::{
    cut_to_columns, DASHBOARD_CELL_GAP, DASHBOARD_NAME_COLUMN, PREVIEW_TEXT_COLUMN, STATUS_COLUMNS,
};
use crate::tree::{pane_placement, Pane, Tab};

/// The word that the STATUS column draws for each turn state. The user reads
/// these, so they say what the agent does rather than name a variant.
///
/// The STATUS column is held at one fixed width, so every word here must fit in
/// it. `the_status_column_is_as_wide_as_its_longest_word` holds the pair in
/// step.
const WANTS_YOU: &str = "needs you";
const WORKING: &str = "working";
const IDLE: &str = "idle";

/// The heading that the STATUS column draws.
const STATUS_HEADING: &str = "STATUS";

/// What the block says for an agent that reports no message.
///
/// Every row opens. A block that drew nothing would leave the user to guess
/// whether the agent said nothing or the sidebar failed to read it.
const NO_MESSAGE: &str = "this agent reports no message";

/// The widest that a message line is drawn, whatever the pane can hold.
///
/// A line of prose that spans a whole wide pane is hard to read, because the
/// eye loses the start of the next line. The table is dense and prose is not,
/// so the measure is held here and does not grow with the pane.
///
/// A modern pane is wide. The measure is therefore set well above the width of
/// a printed page, and it holds only the widest panes back.
const PREVIEW_MEASURE: usize = 120;

/// The widest that a tool's argument is drawn in.
///
/// An argument can hold a path, a URL or a secret. The name alone says that the
/// agent is busy, and a short argument says what it is busy with. Neither needs
/// the whole value, so a long one is cut and the cut is marked.
const TOOL_ARGUMENT_COLUMNS: usize = 40;

/// The fewest columns that the AGENT column is drawn in.
///
/// A name shorter than this says nothing, so the pane draws one line about
/// itself instead of a table that no one can read.
const MINIMUM_NAME_COLUMNS: usize = 12;

/// One column of the dashboard table, after the AGENT column.
///
/// STATUS is not here. That column leads the table and never drops, so the
/// builder draws it from the turn state of the row rather than from this list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Tab,
    Pane,
    Branch,
    Model,
    ContextTokens,
}

impl Column {
    /// The columns after AGENT, in the order they draw.
    ///
    /// A narrow pane takes them away from the end of this list. The table
    /// therefore shortens from its right edge, and a column never disappears
    /// from the middle.
    const IN_DRAW_ORDER: [Column; 5] = [
        Column::Tab,
        Column::Pane,
        Column::Branch,
        Column::Model,
        Column::ContextTokens,
    ];

    /// What the heading row calls this column.
    fn heading(self) -> &'static str {
        match self {
            Column::Tab => "TAB",
            Column::Pane => "PANE",
            Column::Branch => "BRANCH",
            Column::Model => "MODEL",
            Column::ContextTokens => "CTX",
        }
    }

    /// The most columns that this one takes, however long its values are.
    ///
    /// Without a cap, one long branch name takes the room that four other
    /// columns need. A value longer than the cap is cut, and the cut carries
    /// its mark.
    fn widest(self) -> usize {
        match self {
            Column::Tab => 16,
            Column::Pane => 16,
            Column::Branch => 18,
            Column::Model => 12,
            Column::ContextTokens => 6,
        }
    }

    /// Which edge of its columns this one sits against. A count reads against
    /// the right, so the thousands of two counts line up.
    fn alignment(self) -> CellAlignment {
        match self {
            Column::ContextTokens => CellAlignment::Right,
            _ => CellAlignment::Left,
        }
    }

    /// What this column says about one agent. An empty answer draws nothing:
    /// no dash and no zero.
    fn spell(self, place: &AgentPlace<'_>) -> String {
        let agent = place.agent;
        match self {
            Column::Tab => format!("{} {}", place.tab.displayed_index, place.tab.name),
            Column::Pane => place.pane.title.clone(),
            Column::Branch => agent.status.branch.clone(),
            Column::Model => short_model_name(&agent.status.model),
            Column::ContextTokens => short_token_count(agent.status.context_tokens),
        }
    }
}

/// What the STATUS column says about one turn state.
fn status_word(turn: Turn) -> &'static str {
    match turn {
        Turn::Attention => WANTS_YOU,
        Turn::Working => WORKING,
        Turn::Idle => IDLE,
    }
}

/// One agent as the table lists it: the record, where it runs, and where it
/// sits in the tree of its own group.
struct AgentPlace<'a> {
    tab: &'a Tab,
    pane: &'a Pane,
    agent: &'a Agent,
    /// How far under its lead this agent sits, and what the tree draws at each
    /// level of that depth.
    stem: RowStem,
    /// Whether an agent hangs under this one. The block of such a row carries
    /// the tree past it, so the children below stay attached to the row.
    has_children: bool,
}

/// One lead and everything under it.
///
/// A group draws as one thing and moves as one thing. It takes the urgency of
/// the most urgent row in it, so a call from a child lifts the whole group.
struct AgentGroup<'a> {
    urgency: Urgency,
    rows: Vec<AgentPlace<'a>>,
}

/// How urgent one row is: the group it sits in, and how long it waited inside
/// that group.
type Urgency = (u8, u64);

/// The urgency group of an agent that wants the user, which leads the table.
const URGENCY_ATTENTION: u8 = 0;

/// How urgent one row is.
///
/// A call carries the clock reading of the moment it was raised, and nothing
/// else carries one. An older reading is a longer wait, so the first group
/// sorts by the reading itself. Every other agent reads zero and ties there,
/// which leaves the tree order in place under a stable sort.
fn urgency(agent: &Agent) -> Urgency {
    match agent.turn {
        Turn::Attention => (URGENCY_ATTENTION, agent.raised),
        Turn::Working => (1, 0),
        Turn::Idle => (2, 0),
    }
}

/// Every group of one pane: each agent that no agent of the pane started, with
/// everything under that agent.
fn groups_of_pane<'a>(tab: &'a Tab, pane: &'a Pane) -> Vec<AgentGroup<'a>> {
    let held: BTreeSet<&SessionId> = pane.agents.iter().map(|agent| &agent.session).collect();
    let mut children: BTreeMap<&SessionId, Vec<&Agent>> = BTreeMap::new();
    let mut roots: Vec<&Agent> = Vec::new();
    for agent in &pane.agents {
        // An agent whose lead is not in this pane leads a group of its own. The
        // daemon files no such record, and a row must not vanish because two
        // reports arrived out of order.
        match agent.lead.as_ref().filter(|lead| held.contains(lead)) {
            Some(lead) => children.entry(lead).or_default().push(agent),
            None => roots.push(agent),
        }
    }
    // The children of one agent draw in the order of their own ids, so one set
    // of records always draws in one order. A Claude agent id is random, so the
    // order says nothing about which child began first.
    for under in children.values_mut() {
        under.sort_by(|one, other| one.session.cmp(&other.session));
    }
    roots.sort_by(|one, other| one.session.cmp(&other.session));
    roots
        .into_iter()
        .map(|root| {
            let mut rows = Vec::new();
            let urgency = walk_group(tab, pane, root, &children, Vec::new(), &mut rows);
            AgentGroup { urgency, rows }
        })
        .collect()
}

/// One agent and everything under it, in the order they draw, appended to
/// `into`. The result is the urgency of the most urgent row of the lot.
///
/// A child follows its parent, so the walk is depth first. Nothing sorts inside
/// a group by urgency. A child that raises a call keeps its place, and no row
/// moves under the cursor while children work.
fn walk_group<'a>(
    tab: &'a Tab,
    pane: &'a Pane,
    agent: &'a Agent,
    children: &BTreeMap<&SessionId, Vec<&'a Agent>>,
    stem: Vec<Branch>,
    into: &mut Vec<AgentPlace<'a>>,
) -> Urgency {
    let under: &[&Agent] = match children.get(&agent.session) {
        Some(under) => under,
        None => &[],
    };
    into.push(AgentPlace {
        tab,
        pane,
        agent,
        stem: RowStem::new(stem.clone()),
        has_children: !under.is_empty(),
    });
    let mut worst = urgency(agent);
    let last = under.len().saturating_sub(1);
    for (position, child) in under.iter().enumerate() {
        let mut deeper = stem.clone();
        deeper.push(match position == last {
            true => Branch::Last,
            false => Branch::More,
        });
        worst = worst.min(walk_group(tab, pane, child, children, deeper, into));
    }
    worst
}

/// The columns that one table column takes: the widest of its heading and its
/// values, held to the cap that the column carries.
fn column_width(column: Column, places: &[AgentPlace<'_>]) -> usize {
    places
        .iter()
        .map(|place| column.spell(place).chars().count())
        .chain([column.heading().chars().count()])
        .max()
        .unwrap_or(0)
        .min(column.widest())
}

/// The table that a pane `width` columns wide has room for: the columns after
/// AGENT that are drawn, and the columns that AGENT itself takes.
///
/// `None` says that the pane is too narrow for the AGENT column, whatever else
/// is dropped.
fn fit(places: &[AgentPlace<'_>], width: usize) -> Option<(Vec<(Column, usize)>, usize)> {
    let room = width.saturating_sub(DASHBOARD_NAME_COLUMN);
    let mut kept: Vec<(Column, usize)> = Column::IN_DRAW_ORDER
        .iter()
        .map(|column| (*column, column_width(*column, places)))
        .collect();
    loop {
        let spent: usize = kept
            .iter()
            .map(|(_, width)| width + DASHBOARD_CELL_GAP)
            .sum();
        let name = room.saturating_sub(spent);
        if name >= MINIMUM_NAME_COLUMNS {
            return Some((kept, name));
        }
        kept.pop()?;
    }
}

/// A cell holding `text`, cut to `width` and marked where it was cut.
fn cell(text: &str, width: usize, alignment: CellAlignment) -> TableCell {
    TableCell {
        text: cut_to_columns(text, width),
        width,
        alignment,
    }
}

/// The columns that one line of a block has for its text in a pane `width`
/// columns wide.
///
/// The text starts inside the preview panel. The measure limits the line
/// length so a wide pane does not draw one long line of prose.
fn preview_field(width: usize) -> usize {
    width
        .saturating_sub(PREVIEW_TEXT_COLUMN)
        .clamp(1, PREVIEW_MEASURE)
}

/// The tool name and a bounded argument for the preview footer.
fn tool_line(call: &ToolCall, field: usize) -> String {
    if call.argument.is_empty() {
        return cut_to_columns(&call.name, field);
    }
    let argument = cut_to_columns(&call.argument, TOOL_ARGUMENT_COLUMNS);
    cut_to_columns(&format!("{}: {argument}", call.name), field)
}

/// One line of a block, before the tree glyph of that line is decided. The
/// glyph depends on how many lines follow, which is known only once every line
/// is built.
enum PreviewLine {
    /// One line of what the agent said, in the runs that its markdown divided
    /// the line into.
    Message(Vec<TextRun>),
    Time(String),
}

impl PreviewLine {
    /// The row content of this line. The line hangs from `branch`, under a row
    /// whose own place in the tree is `stem`.
    fn content(self, placement: Placement, stem: RowStem, branch: Branch) -> RowContent {
        match self {
            PreviewLine::Message(runs) => RowContent::PreviewMessage {
                placement,
                stem,
                branch,
                runs,
            },
            PreviewLine::Time(text) => RowContent::PreviewTime {
                placement,
                stem,
                branch,
                text,
            },
        }
    }
}

/// The block that hangs under one open row: what the agent last said, when it
/// said so, and the tool it runs now.
///
/// Every line carries the key of the agent it describes, the way a notification
/// entry does. A click anywhere in the block therefore reaches the same pane,
/// and the keys step over the block as one thing.
fn preview_rows(place: &AgentPlace<'_>, stem: &RowStem, width: usize) -> Vec<Row> {
    let field = preview_field(width);
    let preview = Preview::from_records(&place.agent.records);
    let placement = pane_placement(place.tab.active, place.pane.focused);
    let key = RowKey::Agent(place.agent.session.clone());

    let mut message = clipped_message_lines(&preview.message, field);
    if message.is_empty() {
        message = vec![vec![TextRun::plain(NO_MESSAGE)]];
    }
    let truncated = message.len() > 8;
    let mut lines: Vec<PreviewLine> = message
        .into_iter()
        .take(8)
        .map(PreviewLine::Message)
        .collect();
    if truncated {
        lines.push(PreviewLine::Time("… more".to_string()));
    }
    let mut footer = Vec::new();
    if let Some(clock) = preview.timestamp.get(11..16) {
        footer.push(format!("{clock} UTC"));
    }
    if let Some(call) = &preview.running_tool {
        footer.push(tool_line(call, field));
    }
    if !footer.is_empty() {
        lines.push(PreviewLine::Time(cut_to_columns(
            &footer.join(" · "),
            field,
        )));
    }

    // The last line closes the tree, and nothing is drawn below it. A row with
    // children of its own is the one exception: the last line of that block
    // carries the tree on, and the children hang below it.
    let closes = match place.has_children {
        true => Branch::More,
        false => Branch::Last,
    };
    let count = lines.len();
    lines
        .into_iter()
        .enumerate()
        .map(|(at, line)| {
            let branch = match at + 1 == count {
                true => closes,
                false => Branch::More,
            };
            Row::new(line.content(placement, stem.clone(), branch)).with_key(key.clone())
        })
        .collect()
}

/// The rows that a client draws for the dashboard, in the order they are drawn
/// and navigated.
///
/// The order is by urgency. Agents that want you lead, agents mid-turn follow,
/// and idle agents go last. Inside the first group the longest wait leads. This
/// reverses [`Registry::calling`], which puts the most recent call first. A
/// list of calls answers "what just happened". A dashboard answers "who has
/// waited longest".
///
/// [`Registry::calling`]: agent_wrangler_core::registry::Registry::calling
pub fn build_dashboard(
    tabs: &[Tab],
    width: usize,
    open: &OpenPreviews,
    options: &DrawingOptions,
) -> Vec<Row> {
    let mut groups: Vec<AgentGroup> = tabs
        .iter()
        .flat_map(|tab| tab.panes.iter().map(move |pane| (tab, pane)))
        .flat_map(|(tab, pane)| groups_of_pane(tab, pane))
        .collect();
    // A stable sort. Two groups that report the same facts therefore keep the
    // order that the tree gives them, and no row moves under the cursor.
    groups.sort_by_key(|group| group.urgency);
    let mut queue_starts = BTreeMap::new();
    let mut offset = 0;
    for urgency in 0..=2 {
        let matching: Vec<_> = groups
            .iter()
            .filter(|group| group.urgency.0 == urgency)
            .collect();
        if !matching.is_empty() {
            queue_starts.insert(offset, (urgency, matching.len()));
            offset += matching.iter().map(|group| group.rows.len()).sum::<usize>();
        }
    }
    let places: Vec<AgentPlace> = groups.into_iter().flat_map(|group| group.rows).collect();
    if places.is_empty() {
        return vec![Row::new(RowContent::DashboardNoAgents)];
    }
    let Some((columns, name_width)) = fit(&places, width) else {
        return vec![Row::new(RowContent::DashboardPaneTooNarrow)];
    };

    let mut rows = vec![Row::new(RowContent::DashboardHeading {
        status: cell(STATUS_HEADING, STATUS_COLUMNS, CellAlignment::Left),
        name: cell("AGENT", name_width, CellAlignment::Left),
        cells: columns
            .iter()
            .map(|(column, width)| cell(column.heading(), *width, column.alignment()))
            .collect(),
    })];
    for (position, place) in places.iter().enumerate() {
        if let Some((urgency, count)) = queue_starts.get(&position) {
            rows.push(Row::new(RowContent::DashboardGroup {
                title: ["Needs you", "Working", "Idle"][*urgency as usize].to_string(),
                count: *count,
            }));
        }
        let showing = match open.holds(&place.agent.session) {
            true => RowPreview::Open,
            false => RowPreview::Closed,
        };
        // The stem and the AGENT cell sum to one width on every row, so every
        // column after AGENT stays aligned and the table never widens because a
        // child appeared.
        let stem = place.stem.held_to(name_width);
        rows.push(
            Row::new(RowContent::DashboardAgent {
                placement: pane_placement(place.tab.active, place.pane.focused),
                stem: stem.clone(),
                turn: place.agent.turn,
                color: NamedColor::for_agent(place.agent),
                preview: showing,
                status: cell(
                    status_word(place.agent.turn),
                    STATUS_COLUMNS,
                    CellAlignment::Left,
                ),
                name: cell(
                    &label(place.agent, options.label),
                    name_width - stem.columns(),
                    CellAlignment::Left,
                ),
                cells: columns
                    .iter()
                    .map(|(column, width)| cell(&column.spell(place), *width, column.alignment()))
                    .collect(),
            })
            .with_key(RowKey::Agent(place.agent.session.clone())),
        );
        if showing == RowPreview::Open {
            rows.extend(preview_rows(place, &stem, width));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    use agent_wrangler_core::agent::{LabelFacts, SessionId, StatusFacts, TranscriptRecords};
    use agent_wrangler_core::label::Label;
    use agent_wrangler_core::origin::Origin;
    use agent_wrangler_core::status_line::StatusTemplate;

    use crate::model::{Indicator, Placement, TabId, TabPosition};
    use crate::render::row_text;

    /// A pane 118 columns wide, the width that the design draws. Every column
    /// fits there.
    const WIDE: usize = 118;

    /// The narrowest pane that draws the fixed lead and a readable AGENT cell.
    /// Every optional column has been dropped at this width.
    const NARROWEST: usize = DASHBOARD_NAME_COLUMN + MINIMUM_NAME_COLUMNS;

    fn agent(id: &str, title: &str) -> Agent {
        Agent::new(
            SessionId::new(id).unwrap(),
            "claude",
            LabelFacts {
                title: title.to_string(),
                ..LabelFacts::default()
            },
            Origin::default(),
        )
    }

    /// An agent that wants you, and the reading of the clock when it called.
    fn calling(id: &str, title: &str, raised: u64) -> Agent {
        let mut agent = agent(id, title);
        agent.turn = Turn::Attention;
        agent.raised = raised;
        agent
    }

    fn working(id: &str, title: &str) -> Agent {
        let mut agent = agent(id, title);
        agent.turn = Turn::Working;
        agent
    }

    fn pane(id: u32, title: &str, focused: bool, agents: Vec<Agent>) -> Pane {
        let mut pane = Pane::new(id, title, focused);
        pane.agents = agents;
        pane
    }

    fn tab(position: usize, name: &str, active: bool, panes: Vec<Pane>) -> Tab {
        Tab {
            id: TabId::new(name),
            position: TabPosition::at(position),
            displayed_index: (position + 1).to_string(),
            name: name.to_string(),
            active,
            panes,
        }
    }

    fn session() -> Vec<Tab> {
        vec![
            tab(
                0,
                "wrangler",
                true,
                vec![
                    pane(1, "nvim", false, Vec::new()),
                    pane(2, "claude", true, vec![working("two", "the zellij port")]),
                ],
            ),
            tab(
                1,
                "notes",
                false,
                vec![pane(3, "copilot", false, vec![agent("three", "docs")])],
            ),
            tab(
                2,
                "infra",
                false,
                vec![pane(
                    4,
                    "ssh prod-1",
                    false,
                    vec![calling("one", "migrate the runner", 100)],
                )],
            ),
        ]
    }

    fn dashboard(tabs: &[Tab], width: usize) -> Vec<Row> {
        build_dashboard(
            tabs,
            width,
            &OpenPreviews::default(),
            &DrawingOptions::default(),
        )
    }

    /// The AGENT cell of every agent row, with the padding dropped.
    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .filter_map(|row| match &row.content {
                RowContent::DashboardAgent { name, .. } => Some(name.text.trim_end().to_string()),
                _ => None,
            })
            .collect()
    }

    /// The headings of the table, in the order they draw.
    fn headings(rows: &[Row]) -> Vec<String> {
        match &rows[0].content {
            RowContent::DashboardHeading {
                status,
                name,
                cells,
            } => [
                status.text.trim_end().to_string(),
                name.text.trim_end().to_string(),
            ]
            .into_iter()
            .chain(cells.iter().map(|cell| cell.text.trim().to_string()))
            .collect(),
            other => panic!("the table opens with {other:?}"),
        }
    }

    /// The cell of one column of one agent row, with the padding dropped.
    fn cell_text(rows: &[Row], row: usize, column: usize) -> String {
        match &rows[row].content {
            RowContent::DashboardAgent { cells, .. } => cells[column].text.trim().to_string(),
            other => panic!("row {row} is {other:?}"),
        }
    }

    /// The STATUS cell of one agent row, with the padding dropped.
    fn status_text(rows: &[Row], row: usize) -> String {
        match &rows[row].content {
            RowContent::DashboardAgent { status, .. } => status.text.trim().to_string(),
            other => panic!("row {row} is {other:?}"),
        }
    }

    #[test]
    fn queue_headings_count_leads_and_keep_child_attention_below_its_working_parent() {
        let mut tabs = one_group();
        tabs[0].panes[0].agents[0].turn = Turn::Working;
        tabs[0].panes[0].agents[2].turn = Turn::Attention;
        let rows = build_dashboard(
            &tabs,
            WIDE,
            &OpenPreviews::default(),
            &DrawingOptions::default(),
        );
        assert_eq!(row_text(&rows[1].content), "    NEEDS YOU · 1");
        assert_eq!(names(&rows), ["the lead", "the teammate", "the subagent"]);
        assert_eq!(status_text(&rows, 2), WORKING);
        assert_eq!(status_text(&rows, 4), "needs you");
        assert!(rows.iter().all(|row| row.indicator == Indicator::None));
    }

    #[test]
    fn queue_counts_exclude_children_and_queue_labels_are_not_selectable() {
        let mut tabs = session();
        tabs[1].panes[0]
            .agents
            .push(working("four", "another lead"));
        let mut child = under_lead("four.child", "four", "child");
        child.turn = Turn::Attention;
        tabs[1].panes[0].agents.push(child);
        let rows = dashboard(&tabs, WIDE);
        let headings: Vec<_> = rows
            .iter()
            .filter(|row| matches!(row.content, RowContent::DashboardGroup { .. }))
            .collect();
        assert_eq!(
            headings
                .iter()
                .map(|row| row_text(&row.content))
                .collect::<Vec<_>>(),
            ["    NEEDS YOU · 2", "    WORKING · 1", "    IDLE · 1"]
        );
        assert!(headings.iter().all(|row| row.key.is_none()));
    }

    #[test]
    fn one_agent_draws_one_row_and_a_tab_or_a_pane_draws_none() {
        // The session holds three agents, four panes and three tabs. Only the
        // agents reach the table, under one heading row.
        let rows = dashboard(&session(), WIDE);
        assert_eq!(rows.len(), 7);
        assert_eq!(
            names(&rows),
            ["migrate the runner", "the zellij port", "docs"]
        );
    }

    #[test]
    fn the_table_names_its_columns_in_the_order_they_draw() {
        assert_eq!(
            headings(&dashboard(&session(), WIDE)),
            ["STATUS", "AGENT", "TAB", "PANE", "BRANCH", "MODEL", "CTX"]
        );
    }

    #[test]
    fn the_status_column_holds_the_word_for_the_group_the_row_sits_in() {
        let rows = dashboard(&session(), WIDE);
        for (row, word) in [(2, WANTS_YOU), (4, WORKING), (6, IDLE)] {
            assert_eq!(status_text(&rows, row), word);
        }
    }

    #[test]
    fn the_status_column_is_as_wide_as_its_longest_word() {
        // The column is held at a fixed width, and that width decides where the
        // tree starts. A word wider than the column would be cut.
        for word in [WANTS_YOU, WORKING, IDLE, STATUS_HEADING] {
            assert!(word.chars().count() <= STATUS_COLUMNS, "{word}");
        }
        assert_eq!(
            WANTS_YOU.chars().count(),
            STATUS_COLUMNS,
            "the column spends no more than its longest word needs"
        );
    }

    /// One agent that another agent started.
    fn under_lead(id: &str, lead: &str, title: &str) -> Agent {
        agent(id, title).with_lead(SessionId::new(lead).unwrap())
    }

    /// The stem of each agent row, in the order the rows draw.
    fn stems(rows: &[Row]) -> Vec<Vec<Branch>> {
        rows.iter()
            .filter_map(|row| match &row.content {
                RowContent::DashboardAgent { stem, .. } => Some(stem.levels().to_vec()),
                _ => None,
            })
            .collect()
    }

    /// A lead, a teammate under it, and a subagent under that teammate, all in
    /// one pane.
    fn one_group() -> Vec<Tab> {
        vec![tab(
            0,
            "wrangler",
            true,
            vec![pane(
                1,
                "claude",
                true,
                vec![
                    agent("one", "the lead"),
                    under_lead("one.mate", "one", "the teammate"),
                    under_lead("one.mate.probe", "one.mate", "the subagent"),
                ],
            )],
        )]
    }

    #[test]
    fn a_child_draws_under_the_agent_that_started_it() {
        let rows = dashboard(&one_group(), WIDE);
        assert_eq!(names(&rows), ["the lead", "the teammate", "the subagent"]);
        // The stem grows one level for each level of depth, and the last child
        // of each level closes its own branch.
        assert_eq!(
            stems(&rows),
            vec![vec![], vec![Branch::Last], vec![Branch::Last, Branch::Last],]
        );
    }

    #[test]
    fn children_of_one_agent_order_by_their_id() {
        // A Claude agent id is random, so the order says nothing about which
        // child began first. The order is stable, and a stable order keeps a row
        // still.
        let tabs = vec![tab(
            0,
            "wrangler",
            true,
            vec![pane(
                1,
                "claude",
                true,
                vec![
                    agent("one", "the lead"),
                    under_lead("one.c", "one", "third"),
                    under_lead("one.a", "one", "first"),
                    under_lead("one.b", "one", "second"),
                ],
            )],
        )];
        let rows = dashboard(&tabs, WIDE);
        assert_eq!(names(&rows), ["the lead", "first", "second", "third"]);
        // Only the last child closes the branch.
        assert_eq!(
            stems(&rows),
            vec![
                vec![],
                vec![Branch::More],
                vec![Branch::More],
                vec![Branch::Last],
            ]
        );
    }

    #[test]
    fn a_group_sorts_as_one_by_the_most_urgent_row_in_it() {
        // The lead is idle and its child wants the user. The whole group leads
        // the table, and the child keeps its place inside the group.
        let tabs = vec![tab(
            0,
            "wrangler",
            true,
            vec![
                pane(1, "a", false, vec![working("busy", "a busy lead")]),
                pane(
                    2,
                    "b",
                    false,
                    vec![
                        agent("idle", "an idle lead"),
                        under_lead("idle.a", "idle", "a quiet child"),
                        {
                            let mut child = under_lead("idle.b", "idle", "a calling child");
                            child.turn = Turn::Attention;
                            child.raised = 100;
                            child
                        },
                    ],
                ),
            ],
        )];
        assert_eq!(
            names(&dashboard(&tabs, WIDE)),
            [
                "an idle lead",
                "a quiet child",
                "a calling child",
                "a busy lead"
            ]
        );
    }

    #[test]
    fn child_attention_changes_the_queue_but_not_ancestor_status() {
        let mut tabs = one_group();
        tabs[0].panes[0].agents[2].turn = Turn::Attention;
        let rows = dashboard(&tabs, WIDE);
        assert_eq!(row_text(&rows[1].content), "    NEEDS YOU · 1");
        assert!(rows.iter().all(|row| row.indicator == Indicator::None));
        assert_eq!(status_text(&rows, 2), IDLE);
        assert_eq!(status_text(&rows, 3), IDLE);
        assert_eq!(status_text(&rows, 4), WANTS_YOU);
    }

    #[test]
    fn a_stem_narrows_the_agent_cell_by_exactly_its_own_width() {
        let rows = dashboard(&one_group(), WIDE);
        let widths: Vec<usize> = rows
            .iter()
            .filter_map(|row| match &row.content {
                RowContent::DashboardAgent { stem, name, .. } => Some(stem.columns() + name.width),
                _ => None,
            })
            .collect();
        // Every row spends one width on the stem and the AGENT cell together,
        // so every column after AGENT stays aligned.
        assert_eq!(widths.len(), 3);
        assert!(widths.windows(2).all(|pair| pair[0] == pair[1]));
        // That width is the AGENT heading's own.
        match &rows[0].content {
            RowContent::DashboardHeading { name, .. } => assert_eq!(name.width, widths[0]),
            other => panic!("the table opens with {other:?}"),
        }
    }

    #[test]
    fn a_name_with_no_columns_left_keeps_its_row() {
        // A deep row can run out of the AGENT column. The name is cut, and
        // every other column still says what it says.
        let deep: Vec<Agent> = (0..12)
            .map(|level| {
                let id: String = (0..=level).map(|_| ".x").collect();
                let id = format!("one{id}");
                match level {
                    0 => under_lead(&id, "one", "a child"),
                    _ => {
                        let lead: String = (0..level).map(|_| ".x").collect();
                        under_lead(&id, &format!("one{lead}"), "a child")
                    }
                }
            })
            .collect();
        let mut agents = vec![agent("one", "the lead")];
        agents.extend(deep);
        let tabs = vec![tab(
            0,
            "wrangler",
            true,
            vec![pane(1, "claude", true, agents)],
        )];
        for width in [NARROWEST, WIDE] {
            let rows = dashboard(&tabs, width);
            // Every agent still draws a row of its own.
            assert_eq!(names(&rows).len(), 13, "{width}");
            // The stem and the AGENT cell still sum to one width, however deep
            // the row sits. The table never widens because a child appeared.
            let widths: Vec<usize> = rows
                .iter()
                .filter_map(|row| match &row.content {
                    RowContent::DashboardAgent { stem, name, .. } => {
                        Some(stem.columns() + name.width)
                    }
                    _ => None,
                })
                .collect();
            assert!(widths.windows(2).all(|pair| pair[0] == pair[1]), "{width}");
        }
    }

    #[test]
    fn a_block_under_a_row_with_children_carries_the_tree_past_it() {
        let mut open = OpenPreviews::default();
        open.open_or_close(&SessionId::new("one").unwrap());
        let rows = build_dashboard(&one_group(), WIDE, &open, &DrawingOptions::default());
        // The last line of the block continues the tree, so the teammate below
        // it stays attached to the lead.
        let branches: Vec<Branch> = rows
            .iter()
            .filter_map(|row| match &row.content {
                RowContent::PreviewMessage { branch, .. }
                | RowContent::PreviewTime { branch, .. }
                | RowContent::PreviewTool { branch, .. } => Some(*branch),
                _ => None,
            })
            .collect();
        assert!(!branches.is_empty());
        assert!(branches.iter().all(|branch| *branch == Branch::More));
    }

    #[test]
    fn a_block_under_a_row_with_no_children_closes_the_tree() {
        let mut open = OpenPreviews::default();
        open.open_or_close(&SessionId::new("one.mate.probe").unwrap());
        let rows = build_dashboard(&one_group(), WIDE, &open, &DrawingOptions::default());
        let last = rows
            .iter()
            .rev()
            .find_map(|row| match &row.content {
                RowContent::PreviewMessage { branch, .. }
                | RowContent::PreviewTime { branch, .. }
                | RowContent::PreviewTool { branch, .. } => Some(*branch),
                _ => None,
            })
            .unwrap();
        assert_eq!(last, Branch::Last);
    }

    #[test]
    fn agents_that_want_you_lead_then_mid_turn_then_idle() {
        // The tree draws these in the order wrangler, notes, infra. The table
        // draws the call first, whatever tab it is in.
        assert_eq!(
            names(&dashboard(&session(), WIDE)),
            ["migrate the runner", "the zellij port", "docs"]
        );
    }

    #[test]
    fn inside_the_first_group_the_longest_wait_leads() {
        // A call carries the clock reading of the moment it was raised, so the
        // smaller reading is the longer wait.
        let tabs = vec![tab(
            0,
            "wrangler",
            true,
            vec![
                pane(1, "a", false, vec![calling("recent", "recent", 300)]),
                pane(2, "b", false, vec![calling("oldest", "oldest", 100)]),
                pane(3, "c", false, vec![calling("middle", "middle", 200)]),
            ],
        )];
        assert_eq!(
            names(&dashboard(&tabs, WIDE)),
            ["oldest", "middle", "recent"]
        );
    }

    #[test]
    fn two_agents_that_report_the_same_facts_keep_the_order_the_tree_gives_them() {
        // The order must not move under the cursor between two draws of one
        // set of facts.
        let tabs = vec![tab(
            0,
            "wrangler",
            true,
            vec![
                pane(1, "a", false, vec![working("first", "first")]),
                pane(2, "b", false, vec![working("second", "second")]),
                pane(3, "c", false, vec![working("third", "third")]),
            ],
        )];
        assert_eq!(names(&dashboard(&tabs, WIDE)), ["first", "second", "third"]);
        assert_eq!(dashboard(&tabs, WIDE), dashboard(&tabs, WIDE));
    }

    #[test]
    fn a_value_the_agent_does_not_report_draws_nothing() {
        // No dash and no zero. A session that has answered nothing has no
        // branch, no model and no count.
        let rows = dashboard(&session(), WIDE);
        for column in [2, 3, 4] {
            assert_eq!(cell_text(&rows, 2, column), "", "column {column}");
        }
    }

    #[test]
    fn a_reported_value_is_spelled_the_way_the_status_line_spells_it() {
        let mut record = calling("one", "migrate the runner", 100);
        record = record.with_status(StatusFacts {
            branch: "infra/ci".to_string(),
            model: "claude-opus-5".to_string(),
            context_tokens: 122_000,
        });
        let tabs = vec![tab(
            2,
            "infra",
            false,
            vec![pane(4, "ssh prod-1", false, vec![record])],
        )];
        let rows = dashboard(&tabs, WIDE);
        assert_eq!(cell_text(&rows, 2, 0), "3 infra");
        assert_eq!(cell_text(&rows, 2, 1), "ssh prod-1");
        assert_eq!(cell_text(&rows, 2, 2), "infra/ci");
        assert_eq!(cell_text(&rows, 2, 3), "opus-5");
        assert_eq!(cell_text(&rows, 2, 4), "122k");
    }

    #[test]
    fn columns_drop_from_the_right_and_each_one_drops_whole() {
        // The pane shortens the table from its right edge, so a column never
        // disappears from the middle. STATUS and AGENT never drop, so the
        // shortest table the pane can hold is those two.
        let tabs = session();
        let mut seen: Vec<Vec<String>> = Vec::new();
        for width in (NARROWEST..=WIDE).rev() {
            let columns = headings(&dashboard(&tabs, width));
            if seen.last() != Some(&columns) {
                seen.push(columns);
            }
        }
        let want: Vec<Vec<String>> = (1..=6)
            .rev()
            .map(|kept| {
                ["STATUS", "AGENT", "TAB", "PANE", "BRANCH", "MODEL", "CTX"][..=kept]
                    .iter()
                    .map(|heading| heading.to_string())
                    .collect()
            })
            .collect();
        assert_eq!(seen, want);
    }

    #[test]
    fn a_name_too_long_for_its_column_is_cut_and_the_cut_carries_a_mark() {
        let tabs = vec![tab(
            0,
            "wrangler",
            true,
            vec![pane(
                1,
                "claude",
                true,
                vec![working("one", "a session label far longer than any column")],
            )],
        )];
        let rows = dashboard(&tabs, 40);
        assert!(names(&rows)[0].ends_with('…'), "{:?}", names(&rows));
    }

    #[test]
    fn a_pane_too_narrow_for_the_agent_column_says_so_and_draws_no_table() {
        for width in [1, 8, NARROWEST - 1] {
            let rows = dashboard(&session(), width);
            assert_eq!(
                rows,
                vec![Row::new(RowContent::DashboardPaneTooNarrow)],
                "{width}"
            );
        }
        assert!(matches!(
            dashboard(&session(), NARROWEST)[0].content,
            RowContent::DashboardHeading { .. }
        ));
    }

    #[test]
    fn a_session_with_no_agents_says_so() {
        let tabs = vec![tab(
            0,
            "wrangler",
            true,
            vec![pane(1, "nvim", false, vec![])],
        )];
        assert_eq!(
            dashboard(&tabs, WIDE),
            vec![Row::new(RowContent::DashboardNoAgents)]
        );
        assert_eq!(
            dashboard(&[], WIDE),
            vec![Row::new(RowContent::DashboardNoAgents)]
        );
    }

    #[test]
    fn every_agent_row_points_at_its_own_session_and_the_heading_points_at_nothing() {
        let rows = dashboard(&session(), WIDE);
        assert_eq!(rows[0].key, None);
        assert_eq!(
            rows[2].key,
            Some(RowKey::Agent(SessionId::new("one").unwrap()))
        );
    }

    #[test]
    fn dashboard_status_has_no_duplicate_turn_marker() {
        for turn_state in [false, true] {
            let options = DrawingOptions {
                turn_state,
                ..DrawingOptions::default()
            };
            let rows = build_dashboard(&session(), WIDE, &OpenPreviews::default(), &options);
            assert!(rows.iter().all(|row| row.indicator == Indicator::None));
            assert_eq!(status_text(&rows, 2), WANTS_YOU);
        }
    }

    #[test]
    fn the_gutter_marks_the_agent_in_the_pane_you_are_in() {
        let rows = dashboard(&session(), WIDE);
        // The zellij port runs in the focused pane of the active tab.
        assert!(row_text(&rows[4].content).starts_with('▌'));
        assert!(row_text(&rows[1].content).starts_with(' '));
    }

    #[test]
    fn the_row_of_an_agent_you_are_with_takes_the_focused_placement() {
        let rows = dashboard(&session(), WIDE);
        let placements: Vec<Placement> = rows[1..]
            .iter()
            .filter_map(|row| match &row.content {
                RowContent::DashboardAgent { placement, .. } => Some(*placement),
                _ => None,
            })
            .collect();
        assert_eq!(
            placements,
            [
                Placement::OtherTab,
                Placement::FocusedPane,
                Placement::OtherTab
            ]
        );
    }

    #[test]
    fn the_label_option_spells_every_agent_name() {
        let named = DrawingOptions {
            label: Label::Dir,
            ..DrawingOptions::default()
        };
        let mut record = working("one", "the zellij port");
        record.meta.dir = "wrangler".to_string();
        let tabs = vec![tab(
            0,
            "wrangler",
            true,
            vec![pane(1, "claude", true, vec![record])],
        )];
        assert_eq!(
            names(&build_dashboard(
                &tabs,
                WIDE,
                &OpenPreviews::default(),
                &named
            )),
            ["wrangler"]
        );
    }

    /// An agent that reports the two records the daemon sends. Each one is a
    /// line of JSON: the message the agent wrote, and the tool that runs now.
    fn agent_reporting_records(id: &str, message: &str, tool: &str) -> Agent {
        let mut agent = working(id, "the zellij port");
        agent = agent.with_records(TranscriptRecords {
            last_message: match message.is_empty() {
                true => String::new(),
                false => format!(
                    r#"{{"type":"assistant","timestamp":"2026-09-01T05:11:01.469Z","message":{{"content":[{{"type":"text","text":"{message}"}}]}}}}"#
                ),
            },
            running_tool: match tool.is_empty() {
                true => String::new(),
                false => format!(
                    r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_one","name":"Bash","input":{{"command":"{tool}"}}}}]}}}}"#
                ),
            },
        });
        agent
    }

    /// One tab holding one agent. A block is drawn under such an agent.
    fn session_with_one_agent(agent: Agent) -> Vec<Tab> {
        vec![tab(
            0,
            "wrangler",
            true,
            vec![pane(1, "claude", true, vec![agent])],
        )]
    }

    /// Every session whose block is drawn.
    fn open_previews(sessions: &[&str]) -> OpenPreviews {
        let mut open = OpenPreviews::default();
        for session in sessions {
            open.open_or_close(&SessionId::new(session).unwrap());
        }
        open
    }

    /// The block rows of a dashboard, as the text of each line with the
    /// trailing padding dropped.
    fn block_lines(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .filter(|row| {
                matches!(
                    row.content,
                    RowContent::PreviewMessage { .. }
                        | RowContent::PreviewTime { .. }
                        | RowContent::PreviewTool { .. }
                )
            })
            .map(|row| row_text(&row.content).trim_end().to_string())
            .collect()
    }

    #[test]
    fn narrow_preview_clips_a_long_paragraph_without_hiding_the_next_result() {
        let message = format!(
            "Result: **{}**\\n\\nnext result",
            "important ".repeat(20).trim()
        );
        let tabs = session_with_one_agent(agent_reporting_records("two", &message, "cargo test"));
        let rows = build_dashboard(
            &tabs,
            40,
            &open_previews(&["two"]),
            &DrawingOptions::default(),
        );
        let lines = block_lines(&rows);
        assert_eq!(
            lines.len(),
            3,
            "two message lines and one compact footer: {lines:?}"
        );
        assert_eq!(lines[0], "    │▌           Result: important impo…");
        assert!(lines[1].ends_with("next result"));
        assert!(lines[2].ends_with("05:11 UTC · Bash: carg…"));
        let runs = rows
            .iter()
            .find_map(|row| match &row.content {
                RowContent::PreviewMessage { runs, .. } => Some(runs),
                _ => None,
            })
            .unwrap();
        assert!(!runs[0].emphasis.bold);
        assert!(runs[1].emphasis.bold);
    }

    #[test]
    fn preview_caps_message_at_eight_lines_and_combines_utc_clock_with_tool() {
        let tabs = session_with_one_agent(agent_reporting_records(
            "two",
            "one\\n\\ntwo\\n\\nthree\\n\\nfour\\n\\nfive\\n\\nsix\\n\\nseven\\n\\neight\\n\\nnine",
            "cargo test",
        ));
        let rows = build_dashboard(
            &tabs,
            WIDE,
            &open_previews(&["two"]),
            &DrawingOptions::default(),
        );
        let lines = block_lines(&rows);
        assert_eq!(lines.len(), 10);
        assert!(lines[7].ends_with("eight"));
        assert!(lines[8].ends_with("… more"));
        assert!(lines[9].ends_with("05:11 UTC · Bash: cargo test"));
        assert!(!lines.iter().any(|line| line.contains("nine")));
    }

    #[test]
    fn minimum_dashboard_keeps_status_and_identity_tightly_paired() {
        let rows = dashboard(&session(), 28);
        assert!(matches!(
            rows[0].content,
            RowContent::DashboardHeading { .. }
        ));
        let line = row_text(&rows[2].content);
        assert_eq!(
            line.chars().skip(4).take(9).collect::<String>(),
            "needs you"
        );
        assert_eq!(line.chars().nth(14), Some('\u{f167a}'));
        assert_eq!(line.chars().count(), 28);
        assert_eq!(row_text(&rows[0].content).find("AGENT"), Some(14));
    }

    #[test]
    fn a_closed_row_draws_no_block() {
        let tabs = session_with_one_agent(agent_reporting_records(
            "two",
            "the port is done",
            "cargo test",
        ));
        assert!(block_lines(&dashboard(&tabs, WIDE)).is_empty());
    }

    #[test]
    fn an_open_row_draws_the_message_then_a_compact_footer() {
        let tabs = session_with_one_agent(agent_reporting_records(
            "two",
            "the port is done",
            "cargo test",
        ));
        let rows = build_dashboard(
            &tabs,
            WIDE,
            &open_previews(&["two"]),
            &DrawingOptions::default(),
        );
        assert_eq!(
            block_lines(&rows),
            [
                // The gutter carries down the block, because the block belongs
                // to the same pane as the row above it.
                "    │▌           the port is done",
                "    │▌           05:11 UTC · Bash: cargo test",
            ]
        );
    }

    #[test]
    fn the_preview_border_starts_at_the_panel_indent() {
        // Each preview uses the same panel indent, independent of row depth.
        let tabs = session_with_one_agent(agent_reporting_records("two", "the port is done", ""));
        let rows = build_dashboard(
            &tabs,
            WIDE,
            &open_previews(&["two"]),
            &DrawingOptions::default(),
        );
        let icon = 4;
        for line in block_lines(&rows) {
            let glyph = line.chars().nth(icon);
            assert!(
                glyph == Some('\u{2502}') || glyph == Some('\u{2514}'),
                "{line}"
            );
        }
    }

    #[test]
    fn every_line_of_a_block_carries_the_key_of_its_agent() {
        // A click anywhere in the block reaches the same pane, and the keys
        // step over the block as one thing.
        let tabs = session_with_one_agent(agent_reporting_records(
            "two",
            "the port is done",
            "cargo test",
        ));
        let rows = build_dashboard(
            &tabs,
            WIDE,
            &open_previews(&["two"]),
            &DrawingOptions::default(),
        );
        let key = Some(RowKey::Agent(SessionId::new("two").unwrap()));
        assert_eq!(rows.iter().filter(|row| row.key == key).count(), 3);
    }

    #[test]
    fn an_agent_that_reports_no_message_says_so_rather_than_drawing_nothing() {
        // This is the Copilot case. The daemon never opens a Copilot
        // transcript, so such a session reports no records at all.
        let tabs = session_with_one_agent(agent_reporting_records("two", "", ""));
        let rows = build_dashboard(
            &tabs,
            WIDE,
            &open_previews(&["two"]),
            &DrawingOptions::default(),
        );
        assert_eq!(
            block_lines(&rows),
            [format!("    │▌           {NO_MESSAGE}")]
        );
    }

    #[test]
    fn an_agent_that_runs_no_tool_draws_no_tool_line() {
        let tabs = session_with_one_agent(agent_reporting_records("two", "the port is done", ""));
        let rows = build_dashboard(
            &tabs,
            WIDE,
            &open_previews(&["two"]),
            &DrawingOptions::default(),
        );
        assert_eq!(
            block_lines(&rows).len(),
            2,
            "the message and the time alone"
        );
    }

    #[test]
    fn a_message_is_clipped_to_the_measure_and_the_measure_does_not_grow_with_the_pane() {
        let long = "word ".repeat(80);
        let tabs = session_with_one_agent(agent_reporting_records("two", long.trim(), ""));
        let widest = |width: usize| {
            let rows = build_dashboard(
                &tabs,
                width,
                &open_previews(&["two"]),
                &DrawingOptions::default(),
            );
            block_lines(&rows)
                .iter()
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0)
        };
        // A narrow pane clips the line to the available columns.
        assert!(widest(WIDE) <= WIDE, "{} drew {}", WIDE, widest(WIDE));
        // The measure keeps the clip point fixed in wider panes.
        assert_eq!(widest(400), widest(800));
        assert!(widest(400) <= PREVIEW_TEXT_COLUMN + PREVIEW_MEASURE);
        assert!(widest(400) > PREVIEW_TEXT_COLUMN + PREVIEW_MEASURE - "word ".len());
    }

    #[test]
    fn a_long_tool_argument_is_cut_and_the_cut_is_marked() {
        let tabs = session_with_one_agent(agent_reporting_records(
            "two",
            "a message",
            &"long ".repeat(40),
        ));
        let rows = build_dashboard(
            &tabs,
            WIDE,
            &open_previews(&["two"]),
            &DrawingOptions::default(),
        );
        let tool = block_lines(&rows).pop().unwrap();
        assert!(tool.contains('\u{2026}'), "{tool}");
        assert!(tool.chars().count() <= PREVIEW_TEXT_COLUMN + PREVIEW_MEASURE);
    }

    #[test]
    fn a_row_marks_whether_its_block_is_open() {
        let tabs = session_with_one_agent(agent_reporting_records("two", "the port is done", ""));
        for (open, glyph) in [
            (open_previews(&["two"]), '\u{25be}'),
            (OpenPreviews::default(), '\u{25b8}'),
        ] {
            let rows = build_dashboard(&tabs, WIDE, &open, &DrawingOptions::default());
            let row = row_text(&rows[2].content);
            // The gutter, then a blank column, then the marker, then a
            // blank column. The marker has room to breathe on each side.
            let marks: Vec<char> = row.chars().take(4).collect();
            assert_eq!(marks, ['\u{258c}', ' ', glyph, ' '], "{row}");
        }
    }

    #[test]
    fn the_status_line_template_draws_no_second_row_in_the_table() {
        // The table gives the branch, the model and the count a column each,
        // and one row per agent is the whole point of the view.
        let lined = DrawingOptions {
            status_line: StatusTemplate::new("{branch} · {model}"),
            ..DrawingOptions::default()
        };
        assert_eq!(
            build_dashboard(&session(), WIDE, &OpenPreviews::default(), &lined).len(),
            7
        );
    }
}
