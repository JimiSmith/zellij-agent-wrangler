//! A finished buffer, as the bytes that draw it on a terminal.
//!
//! This module is for the client that cannot reach a terminal itself. A zellij
//! plugin gets a pane and a print, and nothing else. The plugin reprints the
//! whole pane every time, so there is no cursor to move and no previous frame to
//! diff against. What the plugin prints is this.
//!
//! Cells drawn the same way run together into one escape sequence. The sequence
//! does not open and close once per cell. A row of thirty columns therefore
//! costs two sequences rather than thirty.

use ratatui_core::buffer::{Buffer, Cell};
use ratatui_core::layout::{Position, Rect};
use ratatui_core::style::{Color, Modifier};
use ratatui_core::widgets::{StatefulWidget, Widget};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use crate::frame::Frame;
use crate::model::RowKey;
use crate::render::Sidebar;

/// Back to the terminal's own defaults.
///
/// SGR parameters accumulate. Every change of style therefore starts with a drop
/// of the attributes already in effect rather than with the difference alone.
const RESET: &str = "\u{1b}[0m";

/// The first parameter of the eight-color table, foreground and background. The
/// bright forms sit sixty higher, and the extended forms sit eight higher, in
/// both tables.
const FOREGROUND: u8 = 30;
const BACKGROUND: u8 = 40;

/// How one cell is drawn.
///
/// A cell carries these as three fields rather than as a style. A run of cells
/// drawn the same way is gathered on these three fields.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Attrs {
    fg: Color,
    bg: Color,
    modifier: Modifier,
}

/// The terminal's own defaults, which is what a reset leaves in effect.
/// These defaults need no sequence.
const PLAIN: Attrs = Attrs {
    fg: Color::Reset,
    bg: Color::Reset,
    modifier: Modifier::empty(),
};

impl Attrs {
    fn of(cell: &Cell) -> Self {
        Attrs {
            fg: cell.fg,
            bg: cell.bg,
            modifier: cell.modifier,
        }
    }
}

/// Every attribute a cell can carry, and the parameter that turns it on.
const MODIFIERS: [(Modifier, &str); 9] = [
    (Modifier::BOLD, "1"),
    (Modifier::DIM, "2"),
    (Modifier::ITALIC, "3"),
    (Modifier::UNDERLINED, "4"),
    (Modifier::SLOW_BLINK, "5"),
    (Modifier::RAPID_BLINK, "6"),
    (Modifier::REVERSED, "7"),
    (Modifier::HIDDEN, "8"),
    (Modifier::CROSSED_OUT, "9"),
];

/// The parameter one color takes, with `base` as the value its channel counts
/// from.
///
/// The match is exhaustive on purpose. A color with no arm here is a color that
/// drops out silently, and the compiler is the only thing that can catch it.
fn color(color: Color, base: u8) -> Option<String> {
    let named = |offset: u8| Some((base + offset).to_string());
    match color {
        Color::Reset => None,
        Color::Black => named(0),
        Color::Red => named(1),
        Color::Green => named(2),
        Color::Yellow => named(3),
        Color::Blue => named(4),
        Color::Magenta => named(5),
        Color::Cyan => named(6),
        Color::Gray => named(7),
        Color::DarkGray => named(60),
        Color::LightRed => named(61),
        Color::LightGreen => named(62),
        Color::LightYellow => named(63),
        Color::LightBlue => named(64),
        Color::LightMagenta => named(65),
        Color::LightCyan => named(66),
        Color::White => named(67),
        Color::Indexed(which) => Some(format!("{};5;{which}", base + 8)),
        Color::Rgb(red, green, blue) => Some(format!("{};2;{red};{green};{blue}", base + 8)),
    }
}

/// The sequence that opens these attributes. If the attributes are the
/// defaults, the sequence is empty.
fn open(attrs: Attrs) -> String {
    let mut params: Vec<String> = MODIFIERS
        .iter()
        .filter(|(modifier, _)| attrs.modifier.contains(*modifier))
        .map(|(_, param)| param.to_string())
        .collect();
    params.extend(color(attrs.fg, FOREGROUND));
    params.extend(color(attrs.bg, BACKGROUND));
    if params.is_empty() {
        String::new()
    } else {
        format!("\u{1b}[{}m", params.join(";"))
    }
}

/// What to print to draw this buffer. The output holds the rows, one to a line.
/// Each row ends with the carriage return that a pane needs to start the next
/// row at its own left edge.
///
/// There is no trailing line break. The pane is exactly as tall as the buffer,
/// and one more line scrolls the first row off.
fn buffer_to_ansi(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        if y > area.top() {
            out.push_str("\r\n");
        }
        let mut standing = PLAIN;
        for x in area.left()..area.right() {
            let cell = &buffer[(x, y)];
            // The cells after the first of a wide glyph hold nothing. A print
            // of those cells pushes the rest of the row right.
            if cell.symbol().is_empty() {
                continue;
            }
            let attrs = Attrs::of(cell);
            if attrs != standing {
                if standing != PLAIN {
                    out.push_str(RESET);
                }
                out.push_str(&open(attrs));
                standing = attrs;
            }
            out.push_str(cell.symbol());
        }
        if standing != PLAIN {
            out.push_str(RESET);
        }
    }
    out
}

/// One frame, as the bytes that put it on screen, with `scroll` naming the row
/// that the top of the pane sits on.
///
/// The function makes a buffer of its own every time, because a client that can
/// only print has no previous frame to diff against. Such a client reprints the
/// whole pane or nothing at all.
///
/// A frame can be taller than its pane, because the dashboard keeps every row.
/// The rows are drawn into a buffer of the frame's own height, and the scroll
/// view clips that buffer to the pane. Both scrollbars are switched off: a
/// scrollbar takes a column from the right edge, and that column is the turn
/// marker's.
pub fn frame_to_ansi(frame: &Frame, selected: Option<&RowKey>, offset: usize) -> String {
    let pane = frame.area();
    let mut buffer = Buffer::empty(pane);
    let drawn = Sidebar {
        lines: frame.lines(),
        selected,
    };
    // A scroll view of no height panics where it draws its scrollbars, and a
    // frame of no rows has nothing to clip anyway.
    let height = u16::try_from(frame.lines().len()).unwrap_or(u16::MAX);
    if height == 0 || pane.width == 0 || pane.height == 0 {
        return buffer_to_ansi(&buffer);
    }
    let content = Rect::new(0, 0, pane.width, height);
    let mut view = ScrollView::new(content.as_size())
        .vertical_scrollbar_visibility(ScrollbarVisibility::Never)
        .horizontal_scrollbar_visibility(ScrollbarVisibility::Never);
    drawn.render(content, view.buf_mut());
    let top = Position::new(0, u16::try_from(offset).unwrap_or(u16::MAX));
    StatefulWidget::render(
        &view,
        pane,
        &mut buffer,
        &mut ScrollViewState::with_offset(top),
    );
    buffer_to_ansi(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui_core::style::Style;

    use crate::frame::{build_frame, RowsPastTheFoot};
    use crate::model::{Row, RowContent};
    use crate::options::DrawingOptions;

    fn buffer(width: u16, height: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, width, height))
    }

    /// The columns of the pane that the scrolling tests draw into.
    const PANE_COLUMNS: u16 = 10;

    /// A frame of `rows` header rows, each naming its own number, composed for
    /// a pane `height` rows tall.
    fn frame_of_numbered_rows(rows: usize, height: u16) -> Frame {
        let lines: Vec<Row> = (0..rows)
            .map(|at| {
                Row::new(RowContent::Header {
                    text: format!("row {at}"),
                })
            })
            .collect();
        build_frame(
            &[],
            &lines,
            &[],
            Rect::new(0, 0, PANE_COLUMNS, height),
            RowsPastTheFoot::Keep,
            &DrawingOptions::default(),
        )
    }

    /// The rows of a drawn pane, with every escape sequence dropped and the
    /// padding at the end of each row kept.
    fn drawn_rows(text: &str) -> Vec<String> {
        let mut plain = String::new();
        let mut inside = false;
        for c in text.chars() {
            match (inside, c) {
                (false, '\u{1b}') => inside = true,
                (true, 'm') => inside = false,
                (true, _) => {}
                (false, _) => plain.push(c),
            }
        }
        plain.split("\r\n").map(str::to_string).collect()
    }

    /// The rows of a drawn pane, with the padding at the end of each dropped.
    fn rows_without_padding(text: &str) -> Vec<String> {
        drawn_rows(text)
            .iter()
            .map(|row| row.trim().to_string())
            .collect()
    }

    #[test]
    fn a_pane_draws_the_rows_that_the_offset_names() {
        let frame = frame_of_numbered_rows(10, 3);
        // A heading row draws its text in capitals, with the indent that
        // every heading takes.
        assert_eq!(
            rows_without_padding(&frame_to_ansi(&frame, None, 0)),
            ["ROW 0", "ROW 1", "ROW 2"]
        );
        assert_eq!(
            rows_without_padding(&frame_to_ansi(&frame, None, 4)),
            ["ROW 4", "ROW 5", "ROW 6"]
        );
    }

    #[test]
    fn an_offset_past_the_last_row_still_draws_a_whole_pane() {
        // The application clamps the offset. A client that hands over a stale
        // one must still get a pane rather than blanks or a panic.
        let frame = frame_of_numbered_rows(10, 3);
        assert_eq!(
            rows_without_padding(&frame_to_ansi(&frame, None, 40)),
            ["ROW 7", "ROW 8", "ROW 9"]
        );
    }

    #[test]
    fn the_scroll_draws_nothing_in_the_column_that_the_turn_marker_takes() {
        // A scrollbar would take the rightmost column of the pane. That column
        // belongs to the turn marker, so both scrollbars are switched off.
        let frame = frame_of_numbered_rows(40, 4);
        for offset in [0, 1, 20, 36] {
            for row in drawn_rows(&frame_to_ansi(&frame, None, offset)) {
                assert_eq!(row.chars().count(), PANE_COLUMNS as usize, "{offset}");
                assert_eq!(row.chars().last(), Some(' '), "{offset}: {row}");
            }
        }
    }

    #[test]
    fn a_frame_shorter_than_its_pane_draws_what_it_has() {
        assert_eq!(
            rows_without_padding(&frame_to_ansi(&frame_of_numbered_rows(2, 5), None, 0)).len(),
            5
        );
    }

    #[test]
    fn a_frame_of_no_rows_draws_a_blank_pane() {
        let drawn = frame_to_ansi(&frame_of_numbered_rows(0, 3), None, 0);
        assert_eq!(rows_without_padding(&drawn), ["", "", ""]);
    }

    #[test]
    fn a_pane_of_nothing_is_blanks_and_no_sequences() {
        let drawn = buffer_to_ansi(&buffer(3, 2));
        assert_eq!(drawn, "   \r\n   ");
        assert!(!drawn.contains('\u{1b}'));
    }

    #[test]
    fn rows_are_separated_and_the_last_one_is_not_ended() {
        // A line break after the last row scrolls the pane by one.
        let drawn = buffer_to_ansi(&buffer(1, 3));
        assert_eq!(drawn.matches("\r\n").count(), 2);
        assert!(!drawn.ends_with("\r\n"));
        assert!(!drawn.contains('\n') || drawn.contains("\r\n"));
    }

    #[test]
    fn a_run_drawn_the_same_way_opens_once_and_is_closed_once() {
        let mut buf = buffer(6, 1);
        buf.set_string(0, 0, "abcdef", Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(buffer_to_ansi(&buf), "\u{1b}[1mabcdef\u{1b}[0m");
    }

    #[test]
    fn a_change_of_style_drops_what_was_standing_first() {
        // The parameters accumulate. A dim run after a bold run with no reset
        // between them draws bold and dim.
        let mut buf = buffer(4, 1);
        buf.set_string(0, 0, "ab", Style::new().add_modifier(Modifier::BOLD));
        buf.set_string(2, 0, "cd", Style::new().add_modifier(Modifier::DIM));
        assert_eq!(
            buffer_to_ansi(&buf),
            "\u{1b}[1mab\u{1b}[0m\u{1b}[2mcd\u{1b}[0m"
        );
    }

    #[test]
    fn returning_to_the_default_costs_a_reset_and_nothing_else() {
        let mut buf = buffer(4, 1);
        buf.set_string(0, 0, "ab", Style::new().fg(Color::Cyan));
        buf.set_string(2, 0, "cd", Style::new());
        assert_eq!(buffer_to_ansi(&buf), "\u{1b}[36mab\u{1b}[0mcd");
    }

    #[test]
    fn every_color_a_row_can_carry_takes_its_own_parameter() {
        // The palette the color of a session is drawn from, as the terminal
        // names it.
        for (color, param) in [
            (Color::Red, "31"),
            (Color::Green, "32"),
            (Color::Yellow, "33"),
            (Color::Blue, "34"),
            (Color::Magenta, "35"),
            (Color::Cyan, "36"),
            (Color::LightYellow, "93"),
            (Color::LightMagenta, "95"),
        ] {
            let mut buf = buffer(1, 1);
            buf.set_string(0, 0, "x", Style::new().fg(color));
            assert_eq!(
                buffer_to_ansi(&buf),
                format!("\u{1b}[{param}mx\u{1b}[0m"),
                "{color:?}"
            );
        }
    }

    #[test]
    fn a_background_is_the_same_table_ten_higher() {
        let mut buf = buffer(1, 1);
        buf.set_string(0, 0, "x", Style::new().bg(Color::Cyan));
        assert_eq!(buffer_to_ansi(&buf), "\u{1b}[46mx\u{1b}[0m");
        let mut extended = buffer(1, 1);
        extended.set_string(0, 0, "x", Style::new().fg(Color::Indexed(200)));
        assert_eq!(buffer_to_ansi(&extended), "\u{1b}[38;5;200mx\u{1b}[0m");
        let mut rgb = buffer(1, 1);
        rgb.set_string(0, 0, "x", Style::new().bg(Color::Rgb(1, 2, 3)));
        assert_eq!(buffer_to_ansi(&rgb), "\u{1b}[48;2;1;2;3mx\u{1b}[0m");
    }

    #[test]
    fn attributes_are_ordered_the_same_way_every_time() {
        let mut buf = buffer(1, 1);
        buf.set_string(
            0,
            0,
            "x",
            Style::new()
                .fg(Color::Cyan)
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
        );
        assert_eq!(buffer_to_ansi(&buf), "\u{1b}[1;7;36mx\u{1b}[0m");
    }

    #[test]
    fn a_style_that_ends_a_row_does_not_run_into_the_next() {
        let mut buf = buffer(2, 2);
        buf.set_string(0, 0, "ab", Style::new().add_modifier(Modifier::REVERSED));
        assert_eq!(buffer_to_ansi(&buf), "\u{1b}[7mab\u{1b}[0m\r\n  ");
    }
}
