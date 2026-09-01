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

use agent_wrangler_core::agent::{Agent, Turn};
use agent_wrangler_core::label::label;
use agent_wrangler_core::status_line::{short_model_name, short_token_count};

use crate::model::{CellAlignment, NamedColor, Row, RowContent, RowKey, TableCell};
use crate::options::DrawingOptions;
use crate::render::{
    cut_to_columns, DASHBOARD_CELL_GAP, DASHBOARD_MARKER_GAP, DASHBOARD_MARKER_INSET,
    DASHBOARD_NAME_COLUMN,
};
use crate::tree::{indicator, pane_placement, Pane, Tab};

/// The word that the TURN column draws for each turn state. The user reads
/// these, so they say what the agent does rather than name a variant.
const WANTS_YOU: &str = "wants you";
const WORKING: &str = "working";
const IDLE: &str = "idle";

/// The fewest columns that the AGENT column is drawn in.
///
/// A name shorter than this says nothing, so the pane draws one line about
/// itself instead of a table that no one can read.
const MINIMUM_NAME_COLUMNS: usize = 12;

/// One column of the dashboard table, after the AGENT column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Turn,
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
    const IN_DRAW_ORDER: [Column; 6] = [
        Column::Turn,
        Column::Tab,
        Column::Pane,
        Column::Branch,
        Column::Model,
        Column::ContextTokens,
    ];

    /// What the heading row calls this column.
    fn heading(self) -> &'static str {
        match self {
            Column::Turn => "TURN",
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
            Column::Turn => WANTS_YOU.chars().count(),
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
            Column::Turn => match agent.turn {
                Turn::Attention => WANTS_YOU.to_string(),
                Turn::Working => WORKING.to_string(),
                Turn::Idle => IDLE.to_string(),
            },
            Column::Tab => format!("{} {}", place.tab.displayed_index, place.tab.name),
            Column::Pane => place.pane.title.clone(),
            Column::Branch => agent.status.branch.clone(),
            Column::Model => short_model_name(&agent.status.model),
            Column::ContextTokens => short_token_count(agent.status.context_tokens),
        }
    }
}

/// One agent as the table lists it: the record, and where it runs.
struct AgentPlace<'a> {
    tab: &'a Tab,
    pane: &'a Pane,
    agent: &'a Agent,
}

/// Every agent of the session, in the order the tree draws them.
fn agents_in_tree_order(tabs: &[Tab]) -> Vec<AgentPlace<'_>> {
    tabs.iter()
        .flat_map(|tab| {
            tab.panes.iter().flat_map(move |pane| {
                pane.agents
                    .iter()
                    .map(move |agent| AgentPlace { tab, pane, agent })
            })
        })
        .collect()
}

/// How urgent one row is: the group it sits in, and how long it has waited
/// inside that group.
///
/// A call carries the clock reading of the moment it was raised, and nothing
/// else carries one. An older reading is a longer wait, so the first group
/// sorts by the reading itself. Every other agent reads zero and ties there,
/// which leaves the tree order in place under a stable sort.
fn urgency(agent: &Agent) -> (u8, u64) {
    match agent.turn {
        Turn::Attention => (0, agent.raised),
        Turn::Working => (1, 0),
        Turn::Idle => (2, 0),
    }
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
    // The turn marker takes one column, and the columns on each side of it stay
    // clear. A table has an edge of its own, so the marker sits inside the pane
    // rather than against its edge.
    let field = width.saturating_sub(DASHBOARD_MARKER_GAP + 1 + DASHBOARD_MARKER_INSET);
    let room = field.saturating_sub(DASHBOARD_NAME_COLUMN);
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
pub fn build_dashboard(tabs: &[Tab], width: usize, options: &DrawingOptions) -> Vec<Row> {
    let mut places = agents_in_tree_order(tabs);
    if places.is_empty() {
        return vec![Row::new(RowContent::DashboardNoAgents)];
    }
    let Some((columns, name_width)) = fit(&places, width) else {
        return vec![Row::new(RowContent::DashboardPaneTooNarrow)];
    };
    // A stable sort. Two agents that report the same facts therefore keep the
    // order that the tree gives them, and no row moves under the cursor.
    places.sort_by_key(|place| urgency(place.agent));

    let mut rows = vec![Row::new(RowContent::DashboardHeading {
        name: cell("AGENT", name_width, CellAlignment::Left),
        cells: columns
            .iter()
            .map(|(column, width)| cell(column.heading(), *width, column.alignment()))
            .collect(),
    })];
    rows.extend(places.iter().map(|place| {
        Row::new(RowContent::DashboardAgent {
            placement: pane_placement(place.tab.active, place.pane.focused),
            turn: place.agent.turn,
            color: NamedColor::for_agent(place.agent),
            name: cell(
                &label(place.agent, options.label),
                name_width,
                CellAlignment::Left,
            ),
            cells: columns
                .iter()
                .map(|(column, width)| cell(&column.spell(place), *width, column.alignment()))
                .collect(),
        })
        .with_indicator(indicator(place.agent, options))
        .with_key(RowKey::Agent(place.agent.session.clone()))
    }));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    use agent_wrangler_core::agent::{LabelFacts, SessionId, StatusFacts};
    use agent_wrangler_core::label::Label;
    use agent_wrangler_core::origin::Origin;
    use agent_wrangler_core::status_line::StatusTemplate;

    use crate::model::{Indicator, Placement, TabId, TabPosition};
    use crate::render::row_text;

    /// A pane 118 columns wide, the width that the design draws. Every column
    /// fits there.
    const WIDE: usize = 118;

    /// The narrowest pane that draws a table: the columns before the AGENT
    /// cell, the smallest AGENT column, the gap before the marker, the marker
    /// itself and the gap after it. Every column after AGENT is already gone by
    /// then.
    const NARROWEST: usize = DASHBOARD_NAME_COLUMN
        + MINIMUM_NAME_COLUMNS
        + DASHBOARD_MARKER_GAP
        + 1
        + DASHBOARD_MARKER_INSET;

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
        build_dashboard(tabs, width, &DrawingOptions::default())
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
            RowContent::DashboardHeading { name, cells } => [name.text.trim_end().to_string()]
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

    #[test]
    fn one_agent_draws_one_row_and_a_tab_or_a_pane_draws_none() {
        // The session holds three agents, four panes and three tabs. Only the
        // agents reach the table, under one heading row.
        let rows = dashboard(&session(), WIDE);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            names(&rows),
            ["migrate the runner", "the zellij port", "docs"]
        );
    }

    #[test]
    fn the_table_names_its_columns_in_the_order_they_draw() {
        assert_eq!(
            headings(&dashboard(&session(), WIDE)),
            ["AGENT", "TURN", "TAB", "PANE", "BRANCH", "MODEL", "CTX"]
        );
    }

    #[test]
    fn the_turn_column_holds_the_word_for_the_group_the_row_sits_in() {
        let rows = dashboard(&session(), WIDE);
        for (row, word) in [(1, WANTS_YOU), (2, WORKING), (3, IDLE)] {
            assert_eq!(cell_text(&rows, row, 0), word);
        }
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
        for column in [3, 4, 5] {
            assert_eq!(cell_text(&rows, 1, column), "", "column {column}");
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
        assert_eq!(cell_text(&rows, 1, 1), "3 infra");
        assert_eq!(cell_text(&rows, 1, 2), "ssh prod-1");
        assert_eq!(cell_text(&rows, 1, 3), "infra/ci");
        assert_eq!(cell_text(&rows, 1, 4), "opus-5");
        assert_eq!(cell_text(&rows, 1, 5), "122k");
    }

    #[test]
    fn columns_drop_from_the_right_and_each_one_drops_whole() {
        // The pane shortens the table from its right edge, so a column never
        // disappears from the middle. AGENT never drops.
        let tabs = session();
        let mut seen: Vec<Vec<String>> = Vec::new();
        for width in (NARROWEST..=WIDE).rev() {
            let columns = headings(&dashboard(&tabs, width));
            if seen.last() != Some(&columns) {
                seen.push(columns);
            }
        }
        let want: Vec<Vec<String>> = (0..=6)
            .rev()
            .map(|kept| {
                ["AGENT", "TURN", "TAB", "PANE", "BRANCH", "MODEL", "CTX"][..=kept]
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
            rows[1].key,
            Some(RowKey::Agent(SessionId::new("one").unwrap()))
        );
    }

    #[test]
    fn the_marker_says_whose_turn_it_is_and_the_turn_column_stays_without_it() {
        let rows = dashboard(&session(), WIDE);
        let markers: Vec<Indicator> = rows[1..].iter().map(|row| row.indicator).collect();
        assert_eq!(
            markers,
            [Indicator::Attention, Indicator::Working, Indicator::None]
        );
        // Turning the marker off leaves the column of words behind.
        let quiet = DrawingOptions {
            turn_state: false,
            ..DrawingOptions::default()
        };
        let rows = build_dashboard(&session(), WIDE, &quiet);
        assert!(rows[1..].iter().all(|row| row.indicator == Indicator::None));
        assert_eq!(cell_text(&rows, 1, 0), WANTS_YOU);
    }

    #[test]
    fn the_gutter_marks_the_agent_in_the_pane_you_are_in() {
        let rows = dashboard(&session(), WIDE);
        // The zellij port runs in the focused pane of the active tab.
        assert!(row_text(&rows[2].content).starts_with('▌'));
        assert!(row_text(&rows[1].content).starts_with(' '));
    }

    #[test]
    fn the_row_of_an_agent_you_are_with_takes_the_focused_placement() {
        let rows = dashboard(&session(), WIDE);
        let placements: Vec<Placement> = rows[1..]
            .iter()
            .map(|row| match &row.content {
                RowContent::DashboardAgent { placement, .. } => *placement,
                other => panic!("unexpected row: {other:?}"),
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
        assert_eq!(names(&build_dashboard(&tabs, WIDE, &named)), ["wrangler"]);
    }

    #[test]
    fn the_status_line_template_draws_no_second_row_in_the_table() {
        // The table gives the branch, the model and the count a column each,
        // and one row per agent is the whole point of the view.
        let lined = DrawingOptions {
            status_line: StatusTemplate::new("{branch} · {model}"),
            ..DrawingOptions::default()
        };
        assert_eq!(build_dashboard(&session(), WIDE, &lined).len(), 4);
    }
}
