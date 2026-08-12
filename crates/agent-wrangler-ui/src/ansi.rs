//! A finished buffer, as the bytes that draw it on a terminal.
//!
//! For the client that cannot reach a terminal itself. A zellij plugin is given
//! a pane and a print, and nothing else: it reprints the whole pane every time,
//! so there is no cursor to move and no previous frame to diff against. What it
//! prints is this.
//!
//! Cells drawn the same way are run together into one escape sequence rather
//! than opened and closed one at a time, which is the difference between a
//! thirty column line costing thirty sequences and costing two.

use ratatui_core::buffer::{Buffer, Cell};
use ratatui_core::style::{Color, Modifier};
use ratatui_core::widgets::Widget;

use crate::frame::Frame;
use crate::model::RowKey;
use crate::render::Sidebar;

/// Back to the terminal's own defaults.
///
/// SGR parameters accumulate, so every change of style is opened by dropping
/// what was standing rather than by naming the difference.
const RESET: &str = "\u{1b}[0m";

/// The first parameter of the eight-color table, foreground and background. The
/// bright forms sit sixty higher and the extended forms eight, in both.
const FOREGROUND: u8 = 30;
const BACKGROUND: u8 = 40;

/// How one cell is drawn.
///
/// A cell carries these as three fields rather than as a style, and it is these
/// three that a run of equally drawn cells is gathered on.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Attrs {
    fg: Color,
    bg: Color,
    modifier: Modifier,
}

/// The terminal's own defaults, which is what a reset leaves standing and so
/// what needs no sequence to ask for.
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

/// The parameter one color takes, given the base its channel counts from.
///
/// Exhaustive on purpose: a color this cannot name is one that would otherwise
/// be dropped silently, and the compiler is the only thing that would notice.
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

/// The sequence opening these attributes, empty when they are the defaults.
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

/// What to print to draw this buffer: its rows, one to a line, with the carriage
/// return a pane needs to start the next one at its own left edge.
///
/// No trailing line break: the pane is exactly as tall as the buffer, and one
/// more line would scroll the first one off.
pub fn draw(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        if y > area.top() {
            out.push_str("\r\n");
        }
        let mut standing = PLAIN;
        for x in area.left()..area.right() {
            let cell = &buffer[(x, y)];
            // The cells after the first of a wide glyph hold nothing, and
            // printing them would push the rest of the line right.
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

/// One frame, as the bytes that put it on screen.
///
/// A buffer of its own every time, because a client that can only print has no
/// previous frame to diff against: it reprints the pane entire or not at all.
pub fn pane(frame: &Frame, selected: Option<&RowKey>) -> String {
    let area = frame.area();
    let mut buffer = Buffer::empty(area);
    Sidebar {
        lines: frame.lines(),
        selected,
    }
    .render(area, &mut buffer);
    draw(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui_core::layout::Rect;
    use ratatui_core::style::Style;

    fn buffer(width: u16, height: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, width, height))
    }

    #[test]
    fn a_pane_of_nothing_is_blanks_and_no_sequences() {
        let drawn = draw(&buffer(3, 2));
        assert_eq!(drawn, "   \r\n   ");
        assert!(!drawn.contains('\u{1b}'));
    }

    #[test]
    fn rows_are_separated_and_the_last_one_is_not_ended() {
        // A line break after the last row scrolls the pane by one.
        let drawn = draw(&buffer(1, 3));
        assert_eq!(drawn.matches("\r\n").count(), 2);
        assert!(!drawn.ends_with("\r\n"));
        assert!(!drawn.contains('\n') || drawn.contains("\r\n"));
    }

    #[test]
    fn a_run_drawn_the_same_way_opens_once_and_is_closed_once() {
        let mut buf = buffer(6, 1);
        buf.set_string(0, 0, "abcdef", Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(draw(&buf), "\u{1b}[1mabcdef\u{1b}[0m");
    }

    #[test]
    fn a_change_of_style_drops_what_was_standing_first() {
        // The parameters accumulate, so a bold run followed by a dim one that
        // did not reset would be drawn bold and dim.
        let mut buf = buffer(4, 1);
        buf.set_string(0, 0, "ab", Style::new().add_modifier(Modifier::BOLD));
        buf.set_string(2, 0, "cd", Style::new().add_modifier(Modifier::DIM));
        assert_eq!(draw(&buf), "\u{1b}[1mab\u{1b}[0m\u{1b}[2mcd\u{1b}[0m");
    }

    #[test]
    fn returning_to_the_default_costs_a_reset_and_nothing_else() {
        let mut buf = buffer(4, 1);
        buf.set_string(0, 0, "ab", Style::new().fg(Color::Cyan));
        buf.set_string(2, 0, "cd", Style::new());
        assert_eq!(draw(&buf), "\u{1b}[36mab\u{1b}[0mcd");
    }

    #[test]
    fn every_color_a_row_can_carry_takes_its_own_parameter() {
        // The palette a session's color is drawn from, as the terminal names it.
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
                draw(&buf),
                format!("\u{1b}[{param}mx\u{1b}[0m"),
                "{color:?}"
            );
        }
    }

    #[test]
    fn a_background_is_the_same_table_ten_higher() {
        let mut buf = buffer(1, 1);
        buf.set_string(0, 0, "x", Style::new().bg(Color::Cyan));
        assert_eq!(draw(&buf), "\u{1b}[46mx\u{1b}[0m");
        let mut extended = buffer(1, 1);
        extended.set_string(0, 0, "x", Style::new().fg(Color::Indexed(200)));
        assert_eq!(draw(&extended), "\u{1b}[38;5;200mx\u{1b}[0m");
        let mut rgb = buffer(1, 1);
        rgb.set_string(0, 0, "x", Style::new().bg(Color::Rgb(1, 2, 3)));
        assert_eq!(draw(&rgb), "\u{1b}[48;2;1;2;3mx\u{1b}[0m");
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
        assert_eq!(draw(&buf), "\u{1b}[1;7;36mx\u{1b}[0m");
    }

    #[test]
    fn a_style_that_ends_a_row_does_not_run_into_the_next() {
        let mut buf = buffer(2, 2);
        buf.set_string(0, 0, "ab", Style::new().add_modifier(Modifier::REVERSED));
        assert_eq!(draw(&buf), "\u{1b}[7mab\u{1b}[0m\r\n  ");
    }
}
