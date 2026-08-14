//! Turning a row's structure into the cells drawn for it.
//!
//! Every glyph a client shows that is not the literal name of a thing is chosen
//! here: the gutter marking where you are, the icon marking what kind of thing
//! the row is, the tree branches, the index prefix, the heading's spacing and
//! case, and the styling.
//!
//! A row is drawn as a run of spans rather than as one styled line, which is
//! what lets a pane's or agent's color sit on its icon alone while the name
//! beside it stays in the terminal's default.
//!
//! What comes out is cells in a buffer rather than a finished string. A client
//! that owns its terminal hands those to a backend; one that can only print
//! turns them back into escape sequences. Neither decision belongs to a row.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;

use crate::model::{Branch, NamedColor, Placement, Row, RowContent, RowKey};

/// Column 0: a block marks "where you are", a space does not.
///
/// A fixed position no row color can imitate, carried by exactly two rows: the
/// active tab, and the pane you are in inside it.
fn gutter(here: bool) -> char {
    if here {
        '▌'
    } else {
        ' '
    }
}

/// The glyph for a child's position in its tab: the last one closes the tree.
fn branch(branch: Branch) -> char {
    match branch {
        Branch::More => '├',
        Branch::Last => '└',
    }
}

/// What kind of thing the row is, drawn immediately before its name. Nerd Font
/// glyphs, one column wide.
///
/// This is the only thing distinguishing an agent from a plain pane: a child row
/// draws its name in the terminal's default whatever color the thing carries, so
/// color cannot be read as "this is an agent".
const ICON_PANE: char = '\u{f489}';
const ICON_AGENT: char = '\u{f167a}';

/// A description line hangs beneath its title, indented to the column the
/// title's text starts in (past the gutter, the icon and the gap after it).
const BODY_INDENT: &str = "    ";

/// What a name too long for the pane ends in, standing for the part of it that
/// was not drawn.
const ELLIPSIS: char = '…';

/// The columns a description line has for its text in a pane `width` columns
/// wide: the indent comes off the front and the reserved right-hand column off
/// the end. Never zero, so wrapping to it always terminates.
pub fn notification_body_field(width: usize) -> usize {
    width.saturating_sub(BODY_INDENT.len() + 1).max(1)
}

/// A row's text, split so a color can land on the kind icon alone.
enum Parts {
    /// One undivided line: a heading, a blank, or a tab row, which has no icon
    /// column of its own.
    Whole(String),
    /// A child row, carrying the color its icon is drawn in.
    Split {
        head: String,
        icon: char,
        tail: String,
        color: Option<NamedColor>,
    },
}

/// A child row's pieces: the gutter, the branch and index, then its kind icon
/// and name. The icon sits with the name it labels rather than out at the
/// margin, so the tree it hangs off reads as one unbroken structure.
fn child_parts(
    placement: Placement,
    icon: char,
    position: Branch,
    index: &str,
    name: &str,
    color: Option<NamedColor>,
) -> Parts {
    Parts::Split {
        head: format!(
            "{} {}─ {index}: ",
            gutter(placement.here()),
            branch(position)
        ),
        icon,
        // Two spaces, not one: the icons overhang the single column they are
        // declared as, and one space leaves the name touching the glyph.
        tail: format!("  {name}"),
        color,
    }
}

/// Split a row into the pieces it is drawn as.
fn parts(content: &RowContent) -> Parts {
    match content {
        // The single leading space is load-bearing: it aligns the underline.
        RowContent::Header { text } => Parts::Whole(format!(" {}", text.to_uppercase())),
        RowContent::Blank => Parts::Whole(String::new()),
        RowContent::Window {
            index,
            name,
            placement,
            ..
        } => Parts::Whole(format!("{} {index}: {name}", gutter(placement.here()))),
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
        // No gutter and no branch: the entry hangs off nothing, and the area it
        // sits in is never where you are.
        RowContent::NotificationTitle { title, color } => Parts::Split {
            head: " ".to_string(),
            icon: ICON_AGENT,
            tail: format!("  {title}"),
            color: *color,
        },
        RowContent::NotificationBody { text } => Parts::Whole(format!("{BODY_INDENT}{text}")),
    }
}

/// The line drawn for a row, before it is fitted to the pane width. The drawing
/// composes the same line out of styled spans, so this is where a change to the
/// glyphs around a name is asserted.
pub fn row_text(content: &RowContent) -> String {
    match parts(content) {
        Parts::Whole(text) => text,
        Parts::Split {
            head, icon, tail, ..
        } => format!("{head}{icon}{tail}"),
    }
}

/// The styled spans a row is drawn as, before they are fitted to the pane width.
///
/// A child's color rides on its icon alone. A whole row in an agent's color
/// drowns the list once more than a couple of agents are up, and the icon is
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
    }
}

/// The style a row's own text draws in, which the right-edge indicator inherits
/// when it carries no state color of its own.
///
/// The channels are kept apart: intensity says where you are, and the kind icon
/// says what the row is. A child's color belongs to the icon rather than the
/// name, so only a tab row styles its whole line with a color. Nothing here
/// varies with a row's turn state, which the indicator carries on its own.
pub fn base_style(content: &RowContent) -> Style {
    match content {
        RowContent::Header { .. } => Style::new()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED),
        RowContent::Blank => Style::new().add_modifier(Modifier::DIM),
        RowContent::Window {
            placement, color, ..
        } => own_color(intensity(*placement), *color),
        RowContent::Pane { placement, .. } | RowContent::Agent { placement, .. } => {
            intensity(*placement)
        }
        RowContent::NotificationTitle { .. } => Style::new(),
        // Dimmed, so the title leads and the description reads as its detail.
        // Intensity is not available for that: it says where you are.
        RowContent::NotificationBody { .. } => Style::new().add_modifier(Modifier::DIM),
    }
}

/// How brightly a row draws, which is the one channel saying where you are: bold
/// for the row you are on, dim for a tab you are not in, and plain for the rest
/// of the tab you are in.
///
/// Dimming the whole of an unfocused tab (its icons and its inherited
/// indicators included) sets it behind the current tab as a block, which is what
/// makes the current tab findable at a glance in a long list.
fn intensity(placement: Placement) -> Style {
    match placement {
        Placement::Here => Style::new().add_modifier(Modifier::BOLD),
        Placement::Focused => Style::new(),
        Placement::Unfocused => Style::new().add_modifier(Modifier::DIM),
    }
}

/// Apply a thing's own color, leaving the style untouched when it has none.
fn own_color(style: Style, color: Option<NamedColor>) -> Style {
    match color {
        Some(c) => style.fg(color_of(c)),
        None => style,
    }
}

/// The terminal color a name in the palette is drawn in.
///
/// Named rather than numbered, so what the user's theme calls cyan is what a
/// cyan session is drawn in.
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

/// What a selected row's cells take on: reverse video, with the color and the
/// dimming of whatever it covers dropped.
///
/// Under reverse video both land on what is now the background, so a colored
/// icon would paint a block of color across the selected row and a dimmed one
/// would wash it out. Pointing at somewhere you are not is what the selection is
/// for, so the selected row is usually a dimmed one.
///
/// It is applied over the finished row rather than folded into each span, which
/// is what makes the bar span the full width: the padding a short row leaves is
/// covered by the same patch as its text.
fn selection() -> Style {
    Style::new()
        .fg(Color::Reset)
        .add_modifier(Modifier::REVERSED)
        .remove_modifier(Modifier::DIM)
}

/// Fit a line to `field` columns, ending one that has to be cut with an
/// ellipsis.
///
/// A name that simply stops reads as the whole name of something else, which is
/// the one thing a list of names must not do: two panes in the same directory
/// differ in their tails. The ellipsis takes the last column of the field rather
/// than the one after it, so the column kept for the turn-state marker stays
/// clear whatever a name does.
///
/// The cut is made span by span so the styling survives it, and the ellipsis is
/// drawn in the style of the span the cut fell in: it stands for the text it
/// replaced. A field too narrow to hold even the tree in front of a name cuts
/// into that instead, which is the same order the width takes them away in.
///
/// Columns are counted as characters, the measure the tree in front of a name is
/// composed with.
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
        // Running out mid-span is certain: the line is longer than the field,
        // and the field is one wider than the room its text was given.
        if room == 0 {
            kept.push(Span::styled(format!("{head}{ELLIPSIS}"), span.style));
            break;
        }
        kept.push(Span::styled(head, span.style));
    }
    Line::from(kept)
}

/// The finished pane: every row drawn in order, with the selected one in reverse
/// video.
///
/// The rightmost column is kept for the turn-state indicator, so a long name is
/// cut before it can collide with one, and the cut is marked with an ellipsis.
pub struct Sidebar<'a> {
    pub lines: &'a [Row],
    /// What the selection is on, which is nothing at all when the keys are not
    /// coming to this client.
    pub selected: Option<&'a RowKey>,
}

impl Sidebar<'_> {
    /// Whether this row is the one the selection is on. A row that points at
    /// nothing is never it, however the selection compares.
    fn is_selected(&self, row: &Row) -> bool {
        row.key.is_some() && row.key.as_ref() == self.selected
    }

    /// One row's cells: its text fitted to everything but the last column, then
    /// the marker in the column kept back for it.
    fn draw(&self, row: &Row, area: Rect, buf: &mut Buffer) {
        let field = area.width.saturating_sub(1);
        let base = base_style(&row.content);
        let line = elide(row_line(&row.content), field as usize);
        buf.set_line(area.x, area.y, &line, field);
        if let Some((glyph, color)) = row.indicator.resolve() {
            if let Some(cell) = buf.cell_mut((area.x + field, area.y)) {
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

/// Wrap `text` to `field` columns, breaking on whitespace where one fits and
/// mid-word where a single word is longer than the field.
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
        RowContent::Window {
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

    /// One row's cells, as a client `width` columns wide would draw them.
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

    /// The text of one drawn line, the styling dropped.
    fn text(buf: &Buffer, y: u16) -> String {
        (buf.area().left()..buf.area().right())
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn a_tab_row_leads_with_its_gutter() {
        assert_eq!(
            row_text(&tab("1", "editor", Placement::Here)),
            "▌ 1: editor"
        );
        assert_eq!(
            row_text(&tab("2", "shell", Placement::Unfocused)),
            "  2: shell"
        );
    }

    #[test]
    fn a_child_row_is_indented_under_its_tab() {
        assert_eq!(
            row_text(&pane("0", "nvim", Branch::More, Placement::Focused)),
            "  ├─ 0: \u{f489}  nvim"
        );
        assert_eq!(
            row_text(&pane("1", "bash", Branch::Last, Placement::Here)),
            "▌ └─ 1: \u{f489}  bash"
        );
    }

    #[test]
    fn a_pane_and_an_agent_land_in_the_same_columns() {
        // Swapping which of a tab's panes runs an agent must not shift the tree,
        // so the two forms differ only in the icon and the name.
        let pane_row = row_text(&pane("0", "name", Branch::Last, Placement::Here));
        let agent_row = row_text(&agent("0", "name", Branch::Last, Placement::Here));
        assert_ne!(pane_row, agent_row);
        assert_eq!(pane_row.chars().count(), agent_row.chars().count());
        assert_eq!(
            pane_row.replace(ICON_PANE, ""),
            agent_row.replace(ICON_AGENT, "")
        );
    }

    #[test]
    fn an_icon_takes_the_one_column_it_is_drawn_as() {
        // The tree is composed by counting characters and drawn by measuring
        // columns, and the two agree only while these glyphs measure one column.
        // A glyph that measured two would take the cell after it and shift every
        // name in the pane one place right.
        for (content, icon) in [
            (
                pane("1", "nvim", Branch::Last, Placement::Focused),
                ICON_PANE,
            ),
            (
                agent("1", "nvim", Branch::Last, Placement::Focused),
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
            placement: Placement::Unfocused,
            color: Some(NamedColor::Cyan),
        };
        let buf = drawn(&Row::new(content), 20, false);
        assert_eq!(text(&buf, 0).trim_end(), "  └─ 0: \u{f167a}  a");
        assert_eq!(buf[(8, 0)].fg, Color::Cyan, "the icon carries the color");
        assert_eq!(buf[(7, 0)].fg, Color::Reset, "the tree stays default");
        assert_eq!(buf[(11, 0)].fg, Color::Reset, "the name stays default");
        // Dimming is the placement channel and the color the identity one, so an
        // unfocused agent keeps its icon color rather than trading it for dim.
        for x in 0..12 {
            assert!(buf[(x, 0)].modifier.contains(Modifier::DIM), "{x}");
        }
    }

    #[test]
    fn only_a_row_you_are_on_is_bold_and_only_a_tab_you_are_not_in_is_dim() {
        assert!(base_style(&tab("1", "w", Placement::Here))
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(!base_style(&tab("1", "w", Placement::Unfocused))
            .add_modifier
            .contains(Modifier::BOLD));
        for content in [
            tab("2", "w", Placement::Unfocused),
            pane("0", "p", Branch::Last, Placement::Unfocused),
            agent("0", "a", Branch::More, Placement::Unfocused),
        ] {
            assert!(
                base_style(&content).add_modifier.contains(Modifier::DIM),
                "{content:?}"
            );
        }
        for content in [
            tab("1", "w", Placement::Here),
            pane("0", "p", Branch::Last, Placement::Here),
            agent("0", "a", Branch::More, Placement::Focused),
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
            placement: Placement::Unfocused,
            color: Some(NamedColor::Cyan),
        })
        .at(RowKey::Pane(1.into()))
        .with(Indicator::Attention);
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
        // Every blank line would otherwise be drawn as the selection whenever
        // nothing was selected.
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
            Placement::Here,
        ))
        .at(RowKey::Pane(1.into()))
        .with(Indicator::Attention);
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
            &Row::new(tab("1", "a very long tab name", Placement::Here)),
            12,
            false,
        );
        assert_eq!(text(&tab, 0), "▌ 1: a ver… ");
        let pane = drawn(
            &Row::new(pane(
                "0",
                "some rather long title",
                Branch::Last,
                Placement::Focused,
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
        let row = Row::new(tab("1", "editor", Placement::Here));
        let width = row_text(&row.content).chars().count() as u16 + 1;
        let buf = drawn(&row, width, false);
        assert_eq!(text(&buf, 0), "▌ 1: editor ");
    }

    #[test]
    fn the_ellipsis_is_drawn_in_the_style_of_what_it_replaces() {
        // It stands in for the name, so it takes the name's styling rather than
        // the color riding on the icon two columns before it.
        let row = Row::new(RowContent::Agent {
            index: "0".to_string(),
            label: "a rather long session label".to_string(),
            branch: Branch::Last,
            placement: Placement::Unfocused,
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
        let row = Row::new(pane("0", "nvim", Branch::Last, Placement::Here))
            .at(RowKey::Pane(1.into()))
            .with(Indicator::Working);
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
        let buf = drawn(&Row::new(tab("1", "editor", Placement::Here)), 12, false);
        assert_eq!(buf[(11, 0)].symbol(), " ");
    }

    #[test]
    fn the_description_field_leaves_room_for_the_indent_and_the_edge() {
        assert_eq!(notification_body_field(24), 19);
        // Absurdly narrow panes still yield a field to wrap into.
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
