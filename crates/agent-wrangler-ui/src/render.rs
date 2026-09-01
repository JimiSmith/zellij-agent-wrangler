//! How the structure of a row becomes the cells drawn for it.
//!
//! A client shows glyphs that are not the literal name of a thing. This module
//! chooses all of them:
//!
//! - the gutter that marks where you are,
//! - the icon that marks what kind of thing the row is,
//! - the tree branches,
//! - the index prefix,
//! - the space and the case of a heading,
//! - the styles.
//!
//! A row is drawn as a run of spans rather than as one styled line. The color of
//! a pane or an agent therefore sits on its icon alone. The name beside the icon
//! stays in the default of the terminal.
//!
//! What comes out is cells in a buffer rather than a finished string. A client
//! that owns its terminal hands the cells to a backend. A client that can only
//! print turns the cells into escape sequences. Neither decision belongs to a
//! row.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;

use agent_wrangler_core::agent::Turn;

use crate::model::{
    Branch, CellAlignment, NamedColor, Placement, Row, RowContent, RowKey, RowPreview, TableCell,
};

/// Column 0: a block marks "where you are", and a space does not.
///
/// The position is fixed, and no row color can imitate it. Exactly two rows
/// carry the block: the active tab, and the pane you are in inside that tab.
fn gutter(here: bool) -> char {
    if here {
        '▌'
    } else {
        ' '
    }
}

/// The glyph for the position of a child in its tab. The last child closes the
/// tree.
fn branch(branch: Branch) -> char {
    match branch {
        Branch::More => '├',
        Branch::Last => '└',
    }
}

/// The glyph that carries the tree past a row hanging under a child.
///
/// A child with siblings after it keeps the line going, so a status line does
/// not break the tree it sits inside. The last child closes the tree, and
/// nothing is drawn below it.
fn continuation(branch: Branch) -> char {
    match branch {
        Branch::More => '│',
        Branch::Last => ' ',
    }
}

/// What kind of thing the row is, drawn immediately before its name. The glyphs
/// come from Nerd Font, and each one is one column wide.
///
/// The icon is the only mark that separates an agent from a plain pane. A child
/// row draws its name in the default of the terminal, whatever color the thing
/// carries. Color therefore does not mean "this is an agent".
const ICON_PANE: char = '\u{f489}';
const ICON_AGENT: char = '\u{f167a}';

/// The gap between the kind icon and the name it labels.
///
/// Two spaces, not one: the icons overhang the single column they are declared
/// as. With one space, the name touches the glyph.
const ICON_GAP: &str = "  ";

/// The columns that the kind icon and the gap after it take together.
///
/// Every icon here is one column wide, which `an_icon_takes_the_one_column_it_is_drawn_as`
/// pins.
const ICON_AND_GAP: usize = 1 + ICON_GAP.len();

/// The glyph that says whether the block under a dashboard row is drawn. The
/// mark points down at a block that is open, and along at one that is closed.
fn open_marker(preview: RowPreview) -> char {
    match preview {
        RowPreview::Open => '▾',
        RowPreview::Closed => '▸',
    }
}

/// The glyph that a line of a preview block draws in the tree column. The last
/// line of the block closes the tree, and nothing is drawn below it.
fn preview_glyph(branch: Branch) -> char {
    match branch {
        Branch::More => '│',
        Branch::Last => '└',
    }
}

/// A description row hangs under its title. The row is indented to the column
/// where the text of the title starts, past the gutter, the icon and the gap
/// after the icon.
const BODY_INDENT: &str = "    ";

/// What a name too long for the pane ends in. The mark stands for the part of
/// the name that was not drawn.
const ELLIPSIS: char = '…';

/// What the dashboard says in place of a table.
///
/// A session with no agent gives the dashboard nothing to draw. A pane too
/// narrow for the AGENT column gives it nowhere to draw. Neither line is a
/// [`ClientProblem`], which says that the client is broken.
///
/// [`ClientProblem`]: crate::frame::ClientProblem
const NO_AGENTS: &str = "no agents";
const PANE_TOO_NARROW: &str = "widen the pane";

/// The columns a description row has for its text in a pane `width` columns
/// wide. The indent comes off the front, and the reserved right-hand column
/// comes off the end. The result is never zero, so a wrap to it always ends.
pub fn notification_body_field(width: usize) -> usize {
    width.saturating_sub(BODY_INDENT.len() + 1).max(1)
}

/// The column that a dashboard row starts its AGENT cell in.
///
/// The gutter, the open marker with a space on each side, the kind icon
/// and the gap after the icon come before that column. The builder lays the
/// table out from here, and [`parts`] draws it from here. The two therefore
/// cannot drift apart.
pub const DASHBOARD_NAME_COLUMN: usize = 4 + ICON_AND_GAP;

/// The column that a line of a preview block starts its text in.
///
/// The tree glyph sits in the column of the kind icon of the row above. The
/// block then reads as one thing with its row. The text starts one column past
/// the AGENT cell above it.
pub const PREVIEW_TEXT_COLUMN: usize = DASHBOARD_NAME_COLUMN + 1;

/// The columns between one cell of a dashboard row and the next.
pub const DASHBOARD_CELL_GAP: usize = 2;

/// The columns kept clear between the last cell of a dashboard row and the turn
/// marker.
///
/// The drawing pads every cell to the whole of its column, so the last cell
/// reaches the edge of the table on every row. Without this gap, the marker
/// touches that cell. A name in the tree reaches the edge only when it is long
/// enough to be cut, so the tree needs no such gap.
pub const DASHBOARD_MARKER_GAP: usize = 2;

/// The columns kept clear between the turn marker of a dashboard row and the
/// right edge of the pane.
///
/// A tree row draws its marker in the last column of the pane. A table has an
/// edge of its own, and a marker against the edge of the pane reads as part of
/// the frame rather than as part of the row. This gap gives the marker a space
/// on each side.
pub const DASHBOARD_MARKER_INSET: usize = 1;

/// `text` fitted to `columns`, with an ellipsis at the end of text that the
/// fit had to cut.
///
/// The dashboard cuts a value where it lays the table out. A table row must
/// lose the tail of one cell rather than the columns at its right edge.
/// [`elide`] cuts a whole line and stays the last resort.
///
/// Columns count as characters, which is the measure the table is composed
/// with.
pub fn cut_to_columns(text: &str, columns: usize) -> String {
    if text.chars().count() <= columns {
        return text.to_string();
    }
    let Some(room) = columns.checked_sub(1) else {
        return String::new();
    };
    let mut cut: String = text.chars().take(room).collect();
    cut.push(ELLIPSIS);
    cut
}

/// One piece of a table row.
enum Field {
    /// Text drawn as written, in the style of the row: the gutter, a gap, or a
    /// cell already padded to its columns.
    Text(String),
    /// The kind icon, drawn in the color of the session.
    Icon {
        glyph: char,
        color: Option<NamedColor>,
    },
}

/// The text of one cell, padded to the columns of that cell.
fn padded(cell: &TableCell) -> String {
    let room = cell.width.saturating_sub(cell.text.chars().count());
    match cell.alignment {
        CellAlignment::Left => format!("{}{:room$}", cell.text, ""),
        CellAlignment::Right => format!("{:room$}{}", "", cell.text),
    }
}

/// The fields of a run of cells: a gap before each one, then the cell itself.
fn cell_fields(cells: &[TableCell]) -> Vec<Field> {
    cells
        .iter()
        .flat_map(|cell| {
            [
                Field::Text(format!("{:DASHBOARD_CELL_GAP$}", "")),
                Field::Text(padded(cell)),
            ]
        })
        .collect()
}

/// The text of a row, split so that a color can land on the kind icon alone.
enum Parts {
    /// One undivided run of text: a heading, a blank, or a tab row. A tab row
    /// has no icon column of its own.
    Whole(String),
    /// A child row, with the color its icon is drawn in.
    Split {
        head: String,
        icon: char,
        tail: String,
        color: Option<NamedColor>,
    },
    /// A dashboard row, as a run of fields at fixed offsets.
    Columns(Vec<Field>),
}

/// The pieces of a child row: the gutter, the branch and the index, then the
/// kind icon and the name. The icon sits with the name it labels rather than out
/// at the margin. The tree the row hangs off therefore reads as one unbroken
/// structure.
fn child_parts(
    placement: Placement,
    icon: char,
    position: Branch,
    index: &str,
    name: &str,
    color: Option<NamedColor>,
) -> Parts {
    Parts::Split {
        head: child_head(placement, position, index),
        icon,
        tail: format!("{ICON_GAP}{name}"),
        color,
    }
}

/// Everything a child row draws before its kind icon: the gutter, the branch
/// and the index.
fn child_head(placement: Placement, position: Branch, index: &str) -> String {
    format!(
        "{} {}─ {index}: ",
        gutter(placement.is_focused_pane()),
        branch(position)
    )
}

/// The column that a child row starts its name in.
///
/// A status row pads to this column, so its text sits directly under the label
/// it describes. Both rows read the column from here, so the two cannot drift
/// apart.
fn child_name_column(placement: Placement, position: Branch, index: &str) -> usize {
    child_head(placement, position, index).chars().count() + ICON_AND_GAP
}

/// The pieces of the status row under an agent. The row draws the gutter, then
/// the line that carries the tree past it, then spaces. The spaces reach the
/// column where the label above starts.
///
/// The row draws no kind icon. It describes the row above rather than pointing
/// at a thing of its own, so it takes no color and needs nothing to carry one.
fn status_parts(placement: Placement, position: Branch, index: &str, text: &str) -> Parts {
    let lead = format!(
        "{} {}",
        gutter(placement.is_focused_pane()),
        continuation(position)
    );
    let indent = child_name_column(placement, position, index).saturating_sub(lead.chars().count());
    Parts::Whole(format!("{lead}{:indent$}{text}", ""))
}

/// The pieces of one line of the block under a dashboard row: the gutter, the
/// tree glyph that the block hangs from, and the text.
///
/// The line draws no kind icon and no open marker. It describes the row
/// above rather than pointing at a thing of its own, so it takes no color and
/// needs nothing to carry one. It keeps the gutter. The block belongs to the
/// same pane as its row, and the mark must not break between the two.
fn preview_parts(placement: Placement, branch: Branch, text: &str) -> Parts {
    let lead = format!("{}", gutter(placement.is_focused_pane()));
    let indent = (DASHBOARD_NAME_COLUMN - ICON_AND_GAP).saturating_sub(lead.chars().count());
    let gap = PREVIEW_TEXT_COLUMN - (DASHBOARD_NAME_COLUMN - ICON_AND_GAP) - 1;
    Parts::Whole(format!(
        "{lead}{:indent$}{}{:gap$}{text}",
        "",
        preview_glyph(branch),
        ""
    ))
}

/// The pieces one row is drawn as.
fn parts(content: &RowContent) -> Parts {
    match content {
        // The single leading space is necessary. It aligns the underline.
        RowContent::Header { text } => Parts::Whole(format!(" {}", text.to_uppercase())),
        RowContent::Blank => Parts::Whole(String::new()),
        RowContent::Tab {
            index,
            name,
            placement,
            ..
        } => Parts::Whole(format!(
            "{} {index}: {name}",
            gutter(placement.is_focused_pane())
        )),
        RowContent::Pane {
            index,
            title,
            branch,
            placement,
            color,
        } => child_parts(*placement, ICON_PANE, *branch, index, title, *color),
        RowContent::Agent {
            index,
            label,
            branch,
            placement,
            color,
        } => child_parts(*placement, ICON_AGENT, *branch, index, label, *color),
        RowContent::AgentStatus {
            index,
            text,
            branch,
            placement,
        } => status_parts(*placement, *branch, index, text),
        // No gutter and no branch: the entry hangs off nothing, and the area it
        // sits in is never where you are.
        RowContent::NotificationTitle { title, color } => Parts::Split {
            head: " ".to_string(),
            icon: ICON_AGENT,
            tail: format!("  {title}"),
            color: *color,
        },
        RowContent::NotificationBody { text } => Parts::Whole(format!("{BODY_INDENT}{text}")),
        // The heading row draws spaces where an agent row draws its kind icon
        // and the gap after it. Both rows therefore start the AGENT column in
        // the same place.
        RowContent::DashboardHeading { name, cells } => {
            let mut fields = vec![
                Field::Text(format!("{:DASHBOARD_NAME_COLUMN$}", "")),
                Field::Text(padded(name)),
            ];
            fields.extend(cell_fields(cells));
            Parts::Columns(fields)
        }
        RowContent::DashboardAgent {
            placement,
            name,
            cells,
            color,
            preview,
            ..
        } => {
            let mut fields = vec![
                Field::Text(format!(
                    "{} {} ",
                    gutter(placement.is_focused_pane()),
                    open_marker(*preview)
                )),
                Field::Icon {
                    glyph: ICON_AGENT,
                    color: *color,
                },
                Field::Text(ICON_GAP.to_string()),
                Field::Text(padded(name)),
            ];
            fields.extend(cell_fields(cells));
            Parts::Columns(fields)
        }
        RowContent::PreviewMessage {
            placement,
            branch,
            text,
        }
        | RowContent::PreviewTime {
            placement,
            branch,
            text,
        }
        | RowContent::PreviewTool {
            placement,
            branch,
            text,
        } => preview_parts(*placement, *branch, text),
        RowContent::DashboardNoAgents => Parts::Whole(format!("  {NO_AGENTS}")),
        RowContent::DashboardPaneTooNarrow => Parts::Whole(format!("  {PANE_TOO_NARROW}")),
    }
}

/// The line drawn for a row, before it is fitted to the width of the pane. The
/// drawing composes the same line out of styled spans. A test of the glyphs
/// around a name therefore asserts on this line.
pub fn row_text(content: &RowContent) -> String {
    match parts(content) {
        Parts::Whole(text) => text,
        Parts::Split {
            head, icon, tail, ..
        } => format!("{head}{icon}{tail}"),
        Parts::Columns(fields) => fields
            .iter()
            .map(|field| match field {
                Field::Text(text) => text.clone(),
                Field::Icon { glyph, .. } => glyph.to_string(),
            })
            .collect(),
    }
}

/// The styled spans a row is drawn as, before they are fitted to the width of
/// the pane.
///
/// The color of a child rides on its icon alone. A whole row in the color of an
/// agent drowns the list once more than a couple of agents run. The icon is
/// enough to tie the row to the thing it points at.
pub fn row_line(content: &RowContent) -> Line<'static> {
    let base = base_style(content);
    match parts(content) {
        Parts::Whole(text) => Line::from(Span::styled(text, base)),
        Parts::Split {
            head,
            icon,
            tail,
            color,
        } => Line::from(vec![
            Span::styled(head, base),
            Span::styled(icon.to_string(), own_color(base, color)),
            Span::styled(tail, base),
        ]),
        Parts::Columns(fields) => Line::from(
            fields
                .into_iter()
                .map(|field| match field {
                    Field::Text(text) => Span::styled(text, base),
                    Field::Icon { glyph, color } => {
                        Span::styled(glyph.to_string(), own_color(base, color))
                    }
                })
                .collect::<Vec<Span<'static>>>(),
        ),
    }
}

/// The style the text of a row draws in. If the right-edge indicator carries no
/// state color of its own, it inherits this style.
///
/// The channels stay apart: intensity says where you are, and the kind icon says
/// what the row is. The color of a child belongs to the icon rather than to the
/// name. Only a tab row draws its whole width in a color. Nothing here varies
/// with the turn state of a row, which the indicator carries on its own.
pub fn base_style(content: &RowContent) -> Style {
    match content {
        RowContent::Header { .. } => Style::new()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED),
        RowContent::Blank => Style::new().add_modifier(Modifier::DIM),
        RowContent::Tab {
            placement, color, ..
        } => own_color(intensity(*placement), *color),
        RowContent::Pane { placement, .. }
        | RowContent::Agent { placement, .. }
        // The status row takes the placement of the agent it hangs under. The
        // intensity channel then keeps one meaning across every row, and the
        // indent alone says that this line is detail.
        | RowContent::AgentStatus { placement, .. } => intensity(*placement),
        RowContent::NotificationTitle { .. } => Style::new(),
        // Dimmed, so the title leads and the description reads as its detail.
        // Intensity is not available for that: it says where you are.
        RowContent::NotificationBody { .. } => Style::new().add_modifier(Modifier::DIM),
        RowContent::DashboardHeading { .. } => Style::new()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED),
        RowContent::DashboardAgent { turn, .. } => urgency(*turn),
        // The block is what the user asked to read, so the message draws at
        // full strength whatever the row above it does.
        RowContent::PreviewMessage { .. } => Style::new(),
        // Dimmed, so the message leads and the two facts under it read as its
        // detail. A description row already draws that way.
        RowContent::PreviewTime { .. } | RowContent::PreviewTool { .. } => {
            Style::new().add_modifier(Modifier::DIM)
        }
        // What the dashboard says about itself instead of a table. The line
        // must read plainly, so it takes neither channel.
        RowContent::DashboardNoAgents | RowContent::DashboardPaneTooNarrow => Style::new(),
    }
}

/// How brightly a row draws, which is the one channel that says where you are.
/// The row you are on is bold. A tab you are not in is dim. The rest of the tab
/// you are in is plain.
///
/// An unfocused tab is dim as a whole, its icons and its inherited indicators
/// included. The tab therefore sits behind the current tab as a block, and the
/// user finds the current tab at a glance in a long list.
fn intensity(placement: Placement) -> Style {
    match placement {
        Placement::FocusedPane => Style::new().add_modifier(Modifier::BOLD),
        Placement::SameTab => Style::new(),
        Placement::OtherTab => Style::new().add_modifier(Modifier::DIM),
    }
}

/// How brightly a dashboard row draws, which is the one channel that says how
/// urgent the row is.
///
/// The dashboard orders by urgency, so the agent that wants you leads the pane
/// and draws brightest. The tree orders by place, so intensity says placement
/// there. The gutter says where you are in both views.
fn urgency(turn: Turn) -> Style {
    match turn {
        Turn::Attention => Style::new().add_modifier(Modifier::BOLD),
        Turn::Working => Style::new(),
        Turn::Idle => Style::new().add_modifier(Modifier::DIM),
    }
}

/// The style with the color of a thing added. If the thing has no color of its
/// own, the style stays as it is.
fn own_color(style: Style, color: Option<NamedColor>) -> Style {
    match color {
        Some(c) => style.fg(color_of(c)),
        None => style,
    }
}

/// The terminal color a name in the palette is drawn in.
///
/// The colors carry names rather than numbers. A cyan session is drawn in the
/// color the theme of the user calls cyan.
fn color_of(color: NamedColor) -> Color {
    match color {
        NamedColor::Red => Color::Red,
        NamedColor::Green => Color::Green,
        NamedColor::Yellow => Color::Yellow,
        NamedColor::Blue => Color::Blue,
        NamedColor::Magenta => Color::Magenta,
        NamedColor::Cyan => Color::Cyan,
        NamedColor::BrightYellow => Color::LightYellow,
        NamedColor::BrightMagenta => Color::LightMagenta,
    }
}

/// What the cells of a selected row take on: reverse video, with the color and
/// the dimming of whatever it covers dropped.
///
/// Under reverse video both land on what is now the background. A colored icon
/// there draws a block of color across the selected row, and a dimmed one washes
/// the row out. The selection points at somewhere you are not, so the selected
/// row is usually a dimmed one.
///
/// The style goes over the finished row rather than into each span, which is
/// what makes the bar span the full width. The same patch covers the text of a
/// short row and the padding after it.
fn selection() -> Style {
    Style::new()
        .fg(Color::Reset)
        .add_modifier(Modifier::REVERSED)
        .remove_modifier(Modifier::DIM)
}

/// A line fitted to `field` columns, with an ellipsis at the end of a line the
/// fit had to cut.
///
/// A name that stops with no mark reads as the whole name of something else. A
/// list of names must never do that, because two panes in the same directory
/// differ in their tails. The ellipsis takes the last column of the
/// field rather than the column after it. The column kept for the turn-state
/// marker therefore stays clear, whatever a name does.
///
/// The cut goes span by span, so the styling survives it. The ellipsis draws in
/// the style of the span the cut fell in, because it stands for the text it
/// replaced. A field too narrow to hold even the tree in front of a name cuts
/// into the tree instead. That is the same order the width takes them away in.
///
/// Columns count as characters, which is the measure the tree in front of a name
/// is composed with.
fn elide(line: Line<'static>, field: usize) -> Line<'static> {
    let drawn: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if drawn <= field {
        return line;
    }
    let Some(mut room) = field.checked_sub(1) else {
        return Line::default();
    };
    let mut kept: Vec<Span<'static>> = Vec::new();
    for span in line.spans {
        let head: String = span.content.chars().take(room).collect();
        room -= head.chars().count();
        // The room is certain to run out inside a span. The line is longer than
        // the field, and the field is one column wider than the room its text
        // was given.
        if room == 0 {
            kept.push(Span::styled(format!("{head}{ELLIPSIS}"), span.style));
            break;
        }
        kept.push(Span::styled(head, span.style));
    }
    Line::from(kept)
}

/// The columns that a row keeps clear at the right edge of the pane, after its
/// turn marker.
///
/// A tree row keeps none, and its marker sits in the last column. Every
/// dashboard row keeps the same number, the heading included, so that a heading
/// and the rows under it stop in the same column.
fn marker_inset(content: &RowContent) -> u16 {
    match content {
        RowContent::DashboardHeading { .. }
        | RowContent::DashboardAgent { .. }
        | RowContent::PreviewMessage { .. }
        | RowContent::PreviewTime { .. }
        | RowContent::PreviewTool { .. }
        | RowContent::DashboardNoAgents
        | RowContent::DashboardPaneTooNarrow => DASHBOARD_MARKER_INSET as u16,
        _ => 0,
    }
}

/// The finished pane: every row drawn in order, with the selected row in reverse
/// video.
///
/// The rightmost column stays free for the turn-state indicator. A long name is
/// cut before it can reach that column, and an ellipsis marks the cut.
pub struct Sidebar<'a> {
    pub lines: &'a [Row],
    /// What the selection is on. If the keys do not come to this client, the
    /// selection is nothing at all.
    pub selected: Option<&'a RowKey>,
}

impl Sidebar<'_> {
    /// Whether this row is the row the selection is on. A row that points at
    /// nothing is never that row, whatever the selection holds.
    fn is_selected(&self, row: &Row) -> bool {
        row.key.is_some() && row.key.as_ref() == self.selected
    }

    /// The cells of one row: the text fitted to everything but the last column,
    /// then the marker in the column kept back for it.
    fn draw(&self, row: &Row, area: Rect, buf: &mut Buffer) {
        // The marker takes one column, and the text takes every column before
        // it. One number therefore says both where the marker goes and how much
        // room the text has.
        let marker = area.width.saturating_sub(1 + marker_inset(&row.content));
        let base = base_style(&row.content);
        let line = elide(row_line(&row.content), marker as usize);
        buf.set_line(area.x, area.y, &line, marker);
        if let Some((glyph, color)) = row.indicator.glyph_and_color() {
            if let Some(cell) = buf.cell_mut((area.x + marker, area.y)) {
                cell.set_char(glyph).set_style(own_color(base, color));
            }
        }
        if self.is_selected(row) {
            buf.set_style(area, selection());
        }
    }
}

impl Widget for Sidebar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        for (offset, row) in self.lines.iter().enumerate() {
            let Ok(offset) = u16::try_from(offset) else {
                break;
            };
            if offset >= area.height {
                break;
            }
            self.draw(row, Rect::new(area.x, area.y + offset, area.width, 1), buf);
        }
    }
}

/// `text` wrapped to `field` columns. The break falls on whitespace where a word
/// fits, and inside a word that is longer than the field.
pub fn wrap(text: &str, field: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > field {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let head: String = word.chars().take(field).collect();
            word = word.chars().skip(field).collect();
            lines.push(head);
        }
        let joined = if current.is_empty() {
            word.clone()
        } else {
            format!("{current} {word}")
        };
        if joined.chars().count() > field {
            lines.push(std::mem::take(&mut current));
            current = word;
        } else {
            current = joined;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::Indicator;

    fn tab(index: &str, name: &str, placement: Placement) -> RowContent {
        RowContent::Tab {
            index: index.to_string(),
            name: name.to_string(),
            placement,
            color: None,
        }
    }

    fn pane(index: &str, title: &str, position: Branch, placement: Placement) -> RowContent {
        RowContent::Pane {
            index: index.to_string(),
            title: title.to_string(),
            branch: position,
            placement,
            color: None,
        }
    }

    fn agent(index: &str, label: &str, position: Branch, placement: Placement) -> RowContent {
        RowContent::Agent {
            index: index.to_string(),
            label: label.to_string(),
            branch: position,
            placement,
            color: None,
        }
    }

    fn status(index: &str, text: &str, position: Branch, placement: Placement) -> RowContent {
        RowContent::AgentStatus {
            index: index.to_string(),
            text: text.to_string(),
            branch: position,
            placement,
        }
    }

    /// The cells of one row, as a client `width` columns wide draws them.
    fn drawn(row: &Row, width: u16, selected: bool) -> Buffer {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        let key = row.key.clone();
        Sidebar {
            lines: std::slice::from_ref(row),
            selected: match selected {
                true => key.as_ref(),
                false => None,
            },
        }
        .render(area, &mut buf);
        buf
    }

    /// The text of one drawn row, with the styling dropped.
    fn text(buf: &Buffer, y: u16) -> String {
        (buf.area().left()..buf.area().right())
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn a_tab_row_leads_with_its_gutter() {
        assert_eq!(
            row_text(&tab("1", "editor", Placement::FocusedPane)),
            "▌ 1: editor"
        );
        assert_eq!(
            row_text(&tab("2", "shell", Placement::OtherTab)),
            "  2: shell"
        );
    }

    #[test]
    fn a_child_row_is_indented_under_its_tab() {
        assert_eq!(
            row_text(&pane("0", "nvim", Branch::More, Placement::SameTab)),
            "  ├─ 0: \u{f489}  nvim"
        );
        assert_eq!(
            row_text(&pane("1", "bash", Branch::Last, Placement::FocusedPane)),
            "▌ └─ 1: \u{f489}  bash"
        );
    }

    #[test]
    fn a_pane_and_an_agent_land_in_the_same_columns() {
        // A change in which pane of a tab runs an agent must not shift the
        // tree. The two forms therefore differ only in the icon and the name.
        let pane_row = row_text(&pane("0", "name", Branch::Last, Placement::FocusedPane));
        let agent_row = row_text(&agent("0", "name", Branch::Last, Placement::FocusedPane));
        assert_ne!(pane_row, agent_row);
        assert_eq!(pane_row.chars().count(), agent_row.chars().count());
        assert_eq!(
            pane_row.replace(ICON_PANE, ""),
            agent_row.replace(ICON_AGENT, "")
        );
    }

    #[test]
    fn an_icon_takes_the_one_column_it_is_drawn_as() {
        // The tree is composed with a count of characters and drawn with a
        // measure of columns. The two agree only while these glyphs measure one
        // column. A glyph of two columns takes the cell after it and shifts
        // every name in the pane one place right.
        for (content, icon) in [
            (
                pane("1", "nvim", Branch::Last, Placement::SameTab),
                ICON_PANE,
            ),
            (
                agent("1", "nvim", Branch::Last, Placement::SameTab),
                ICON_AGENT,
            ),
        ] {
            let buf = drawn(&Row::new(content), 20, false);
            let cells: Vec<&str> = (0..20).map(|x| buf[(x, 0)].symbol()).collect();
            assert_eq!(cells[8], icon.to_string(), "the icon is one cell");
            assert_eq!(cells[9..11].concat(), "  ", "and does not eat the gap");
            assert_eq!(cells[11..15].concat(), "nvim");
        }
    }

    #[test]
    fn a_childs_color_lands_on_its_icon_and_nowhere_else() {
        let content = RowContent::Agent {
            index: "0".to_string(),
            label: "a".to_string(),
            branch: Branch::Last,
            placement: Placement::OtherTab,
            color: Some(NamedColor::Cyan),
        };
        let buf = drawn(&Row::new(content), 20, false);
        assert_eq!(text(&buf, 0).trim_end(), "  └─ 0: \u{f167a}  a");
        assert_eq!(buf[(8, 0)].fg, Color::Cyan, "the icon carries the color");
        assert_eq!(buf[(7, 0)].fg, Color::Reset, "the tree stays default");
        assert_eq!(buf[(11, 0)].fg, Color::Reset, "the name stays default");
        // The dimming is the placement channel and the color is the identity
        // channel. An unfocused agent therefore keeps its icon color, and the
        // dimming does not replace it.
        for x in 0..12 {
            assert!(buf[(x, 0)].modifier.contains(Modifier::DIM), "{x}");
        }
    }

    /// The column that `needle` starts in, counted the way the drawing counts.
    /// A byte offset is not a column: the kind icons take four bytes each.
    fn column_of(line: &str, needle: &str) -> Option<usize> {
        line.find(needle).map(|byte| line[..byte].chars().count())
    }

    #[test]
    fn a_status_row_starts_its_text_under_the_label_above_it() {
        // The pair reads as one thing only while the two columns agree. A wider
        // index moves both of them together.
        for index in ["1", "12", "345"] {
            let above = row_text(&agent(index, "wrangler", Branch::Last, Placement::SameTab));
            let below = row_text(&status(index, "main", Branch::Last, Placement::SameTab));
            assert_eq!(
                column_of(&above, "wrangler"),
                column_of(&below, "main"),
                "index {index}: {above:?} against {below:?}"
            );
        }
    }

    #[test]
    fn a_status_row_carries_the_tree_past_itself_only_where_the_tree_goes_on() {
        // A status line under a middle child must not break the branch that
        // runs down to the children after it.
        assert_eq!(
            row_text(&status("1", "main", Branch::More, Placement::SameTab)),
            "  │        main"
        );
        assert_eq!(
            row_text(&status("1", "main", Branch::Last, Placement::SameTab)),
            "           main"
        );
    }

    #[test]
    fn a_status_row_takes_the_gutter_and_the_intensity_of_its_agent() {
        let here = status("1", "main", Branch::Last, Placement::FocusedPane);
        assert!(row_text(&here).starts_with('▌'));
        assert!(base_style(&here).add_modifier.contains(Modifier::BOLD));
        let elsewhere = status("1", "main", Branch::More, Placement::OtherTab);
        assert!(base_style(&elsewhere).add_modifier.contains(Modifier::DIM));
        assert!(
            !base_style(&status("1", "main", Branch::More, Placement::SameTab))
                .add_modifier
                .intersects(Modifier::BOLD | Modifier::DIM)
        );
    }

    #[test]
    fn a_status_row_too_long_for_the_pane_ends_in_an_ellipsis_and_spares_the_marker() {
        let row = Row::new(status(
            "1",
            "a-very-long-branch-name · opus-5 · 196k",
            Branch::More,
            Placement::SameTab,
        ));
        let buf = drawn(&row, 24, false);
        assert_eq!(text(&buf, 0), "  │        a-very-long… ");
    }

    #[test]
    fn only_a_row_you_are_on_is_bold_and_only_a_tab_you_are_not_in_is_dim() {
        assert!(base_style(&tab("1", "w", Placement::FocusedPane))
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(!base_style(&tab("1", "w", Placement::OtherTab))
            .add_modifier
            .contains(Modifier::BOLD));
        for content in [
            tab("2", "w", Placement::OtherTab),
            pane("0", "p", Branch::Last, Placement::OtherTab),
            agent("0", "a", Branch::More, Placement::OtherTab),
        ] {
            assert!(
                base_style(&content).add_modifier.contains(Modifier::DIM),
                "{content:?}"
            );
        }
        for content in [
            tab("1", "w", Placement::FocusedPane),
            pane("0", "p", Branch::Last, Placement::FocusedPane),
            agent("0", "a", Branch::More, Placement::SameTab),
        ] {
            assert!(
                !base_style(&content).add_modifier.contains(Modifier::DIM),
                "{content:?}"
            );
        }
    }

    #[test]
    fn selecting_a_row_drops_the_color_and_the_dimming_it_covers() {
        // Under reverse video both land on what is now the background.
        let row = Row::new(RowContent::Agent {
            index: "0".to_string(),
            label: "a".to_string(),
            branch: Branch::Last,
            placement: Placement::OtherTab,
            color: Some(NamedColor::Cyan),
        })
        .with_key(RowKey::Pane(1.into()))
        .with_indicator(Indicator::Attention);
        let buf = drawn(&row, 20, true);
        for x in 0..20 {
            let cell = &buf[(x, 0)];
            assert!(cell.modifier.contains(Modifier::REVERSED), "{x}");
            assert!(!cell.modifier.contains(Modifier::DIM), "{x}");
            assert_eq!(cell.fg, Color::Reset, "{x}");
        }
    }

    #[test]
    fn a_row_pointing_at_nothing_is_never_the_selected_one() {
        // Without that rule, every blank row draws as the selection while
        // nothing is selected.
        let row = Row::new(RowContent::Blank);
        let buf = drawn(&row, 8, true);
        assert!(!buf[(0, 0)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn the_selection_spans_the_pane_and_a_long_name_is_cut_before_the_marker() {
        let row = Row::new(agent(
            "1",
            "claude · a rather long session label",
            Branch::More,
            Placement::FocusedPane,
        ))
        .with_key(RowKey::Pane(1.into()))
        .with_indicator(Indicator::Attention);
        for width in [10u16, 24, 60] {
            let buf = drawn(&row, width, true);
            assert_eq!(text(&buf, 0).chars().count(), width as usize);
            let last = width - 1;
            assert_eq!(buf[(last, 0)].symbol(), "●", "the marker keeps its column");
            assert!(buf[(last, 0)].modifier.contains(Modifier::REVERSED));
            // The bar covers the padding a short row leaves as well as its text.
            assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        }
    }

    #[test]
    fn a_name_too_long_for_the_pane_ends_in_an_ellipsis() {
        let tab = drawn(
            &Row::new(tab("1", "a very long tab name", Placement::FocusedPane)),
            12,
            false,
        );
        assert_eq!(text(&tab, 0), "▌ 1: a ver… ");
        let pane = drawn(
            &Row::new(pane(
                "0",
                "some rather long title",
                Branch::Last,
                Placement::SameTab,
            )),
            20,
            false,
        );
        assert_eq!(text(&pane, 0), "  └─ 0: \u{f489}  some ra… ");
    }

    #[test]
    fn a_name_that_fills_the_field_exactly_is_drawn_whole() {
        // The ellipsis stands for text that was dropped, so a name with nothing
        // dropped never draws one.
        let row = Row::new(tab("1", "editor", Placement::FocusedPane));
        let width = row_text(&row.content).chars().count() as u16 + 1;
        let buf = drawn(&row, width, false);
        assert_eq!(text(&buf, 0), "▌ 1: editor ");
    }

    #[test]
    fn the_ellipsis_is_drawn_in_the_style_of_what_it_replaces() {
        // It stands in for the name. It therefore takes the styling of the name
        // rather than the color that rides on the icon two columns before it.
        let row = Row::new(RowContent::Agent {
            index: "0".to_string(),
            label: "a rather long session label".to_string(),
            branch: Branch::Last,
            placement: Placement::OtherTab,
            color: Some(NamedColor::Cyan),
        });
        let buf = drawn(&row, 20, false);
        assert_eq!(buf[(18, 0)].symbol(), "…");
        assert_eq!(buf[(18, 0)].fg, Color::Reset);
        assert!(buf[(18, 0)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_pane_too_narrow_for_the_tree_still_leaves_the_markers_column_alone() {
        // The width takes the name first and the tree in front of it after, and
        // neither can reach the column the marker is drawn in.
        let row = Row::new(pane("0", "nvim", Branch::Last, Placement::FocusedPane))
            .with_key(RowKey::Pane(1.into()))
            .with_indicator(Indicator::Working);
        for width in [1u16, 2, 4, 6] {
            let buf = drawn(&row, width, false);
            let line = text(&buf, 0);
            assert_eq!(line.chars().count(), width as usize, "{width}");
            assert_eq!(buf[(width - 1, 0)].symbol(), "○", "{width}");
            let head: String = line.chars().take((width - 1) as usize).collect();
            assert!(!head.contains('○'), "{width}");
        }
    }

    #[test]
    fn a_row_with_no_marker_leaves_its_column_blank() {
        let buf = drawn(
            &Row::new(tab("1", "editor", Placement::FocusedPane)),
            12,
            false,
        );
        assert_eq!(buf[(11, 0)].symbol(), " ");
    }

    /// A cell of `width` columns holding `text`, left against its edge.
    fn cell(text: &str, width: usize) -> TableCell {
        TableCell {
            text: text.to_string(),
            width,
            alignment: CellAlignment::Left,
        }
    }

    /// One agent row of the table, with `width` columns for its name.
    fn dashboard_agent(
        name: &str,
        width: usize,
        turn: Turn,
        placement: Placement,
        color: Option<NamedColor>,
    ) -> RowContent {
        RowContent::DashboardAgent {
            placement,
            turn,
            color,
            preview: RowPreview::Closed,
            name: cell(name, width),
            cells: vec![cell("working", 9), cell("1 wrangler", 10)],
        }
    }

    #[test]
    fn a_dashboard_heading_sits_over_the_columns_it_names() {
        // The heading row draws spaces where an agent row draws its kind icon.
        // The two rows therefore start every column in the same place, and a
        // wider AGENT column moves both together.
        //
        // Every cell here holds text that fits. The builder guarantees that,
        // because the drawing pads a cell and never shortens one.
        for width in [5usize, 8, 20] {
            let heading = RowContent::DashboardHeading {
                name: cell("AGENT", width),
                cells: vec![cell("TURN", 9), cell("TAB", 10)],
            };
            let row = dashboard_agent("docs", width, Turn::Working, Placement::SameTab, None);
            let above = row_text(&heading);
            let below = row_text(&row);
            assert_eq!(
                above.chars().count(),
                below.chars().count(),
                "width {width}"
            );
            for (name, value) in [
                ("AGENT", "docs"),
                ("TURN", "working"),
                ("TAB", "1 wrangler"),
            ] {
                assert_eq!(
                    column_of(&above, name),
                    column_of(&below, value),
                    "width {width}: {name} over {value}"
                );
            }
        }
    }

    #[test]
    fn a_dashboard_row_pads_a_short_cell_and_never_shortens_a_long_one() {
        // The drawing pads and nothing else. A cell arrives already fitted to
        // its columns. A value that overflows one here pushes every column
        // after it out of place.
        assert_eq!(
            row_text(&dashboard_agent(
                "docs",
                8,
                Turn::Idle,
                Placement::SameTab,
                None
            )),
            "  \u{25b8} \u{f167a}  docs      working    1 wrangler"
        );
    }

    #[test]
    fn a_count_sits_against_the_right_of_its_column() {
        let row = RowContent::DashboardHeading {
            name: cell("AGENT", 5),
            cells: vec![TableCell {
                text: "122k".to_string(),
                width: 6,
                alignment: CellAlignment::Right,
            }],
        };
        assert_eq!(row_text(&row), "       AGENT    122k");
    }

    #[test]
    fn a_dashboard_row_draws_its_color_on_the_icon_and_nowhere_else() {
        let content = dashboard_agent(
            "docs",
            8,
            Turn::Working,
            Placement::SameTab,
            Some(NamedColor::Cyan),
        );
        let buf = drawn(&Row::new(content), 40, false);
        assert_eq!(buf[(4, 0)].symbol(), ICON_AGENT.to_string());
        assert_eq!(buf[(4, 0)].fg, Color::Cyan, "the icon carries the color");
        assert_eq!(buf[(7, 0)].fg, Color::Reset, "the name stays default");
    }

    #[test]
    fn a_dashboard_row_draws_its_intensity_from_the_turn_rather_than_the_place() {
        // The dashboard orders by urgency, so the agent that wants you leads
        // the pane and draws brightest. The tree orders by place, so intensity
        // says placement there.
        for (turn, bold, dim) in [
            (Turn::Attention, true, false),
            (Turn::Working, false, false),
            (Turn::Idle, false, true),
        ] {
            // The placement is the same in all three, so nothing but the turn
            // can move the intensity.
            let style = base_style(&dashboard_agent("docs", 8, turn, Placement::OtherTab, None));
            assert_eq!(
                style.add_modifier.contains(Modifier::BOLD),
                bold,
                "{turn:?}"
            );
            assert_eq!(style.add_modifier.contains(Modifier::DIM), dim, "{turn:?}");
        }
    }

    #[test]
    fn the_gutter_of_a_dashboard_row_still_says_where_you_are() {
        for (placement, gutter) in [
            (Placement::FocusedPane, '\u{258c}'),
            (Placement::SameTab, ' '),
            (Placement::OtherTab, ' '),
        ] {
            let row = dashboard_agent("docs", 8, Turn::Attention, placement, None);
            assert!(row_text(&row).starts_with(gutter), "{placement:?}");
        }
    }

    #[test]
    fn a_dashboard_row_keeps_a_space_on_each_side_of_its_marker() {
        let row = Row::new(dashboard_agent(
            "docs",
            8,
            Turn::Attention,
            Placement::SameTab,
            None,
        ))
        .with_key(RowKey::Agent(
            agent_wrangler_core::agent::SessionId::new("one").unwrap(),
        ))
        .with_indicator(Indicator::Attention);
        let inset = DASHBOARD_MARKER_INSET as u16;
        for width in [4u16, 12, 40] {
            let buf = drawn(&row, width, false);
            assert_eq!(text(&buf, 0).chars().count(), width as usize, "{width}");
            assert_eq!(buf[(width - 1 - inset, 0)].symbol(), "\u{25cf}", "{width}");
            // The marker sits inside the pane rather than against its edge.
            assert_eq!(buf[(width - 1, 0)].symbol(), " ", "{width}");
        }
    }

    #[test]
    fn what_the_dashboard_says_about_itself_draws_in_neither_channel() {
        for content in [
            RowContent::DashboardNoAgents,
            RowContent::DashboardPaneTooNarrow,
        ] {
            assert_eq!(base_style(&content), Style::new(), "{content:?}");
        }
        assert_eq!(row_text(&RowContent::DashboardNoAgents), "  no agents");
        assert_eq!(
            row_text(&RowContent::DashboardPaneTooNarrow),
            "  widen the pane"
        );
    }

    #[test]
    fn a_cut_cell_carries_its_mark_and_a_cell_that_fits_carries_none() {
        assert_eq!(cut_to_columns("branch", 6), "branch");
        assert_eq!(cut_to_columns("branch", 10), "branch");
        assert_eq!(cut_to_columns("a-long-branch", 6), "a-lon\u{2026}");
        // A column with no room at all draws nothing rather than a bare mark.
        assert_eq!(cut_to_columns("branch", 0), "");
        assert_eq!(cut_to_columns("branch", 1), "\u{2026}");
    }

    #[test]
    fn the_description_field_leaves_room_for_the_indent_and_the_edge() {
        assert_eq!(notification_body_field(24), 19);
        // A very narrow pane still gives a field to wrap into.
        assert_eq!(notification_body_field(2), 1);
    }

    #[test]
    fn wrapping_breaks_on_whitespace_and_splits_a_word_too_long_to_fit() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(
            wrap("supercalifragilistic", 8),
            vec!["supercal", "ifragili", "stic"]
        );
        assert!(wrap("", 8).is_empty());
    }
}
