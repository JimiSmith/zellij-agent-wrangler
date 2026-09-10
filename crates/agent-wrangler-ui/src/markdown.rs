//! The markdown that an agent writes, turned into the runs a preview row draws.
//!
//! An agent answers in markdown, so the message under an open dashboard row
//! arrives with headings, list items, fences and inline emphasis in it.
//! `tui-markdown` parses that and returns text in the same `ratatui-core` that
//! every row is drawn from. This module gives it a style sheet of its own, and
//! divides the result into the lines and the [`TextRun`]s that a row carries.
//!
//! The style sheet takes no color. Color in this sidebar says what a row is and
//! whose turn it is, and markdown would give it a third meaning. So the whole
//! block draws in intensity alone: a heading is bold, a quote and a code span
//! are dim, a link is underlined, and deleted text is crossed out.
//!
//! A line breaks on the last whitespace that fits, and the break drops the
//! whitespace it falls on. Every other space is kept as the markdown wrote it,
//! so a table and a code block keep the columns that the parser lined them up
//! in. A line still wider than the field breaks inside a word, and the row
//! drawing cuts whatever is wider than the pane.

use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::Line;
use tui_markdown::{AlertKind, Options, StyleSheet};

use crate::model::{TextEmphasis, TextRun};

/// The styles and the markers that a preview draws markdown with.
///
/// This sheet overrides every method that the crate answers with a color. The
/// methods left out already give an intensity effect: the metadata of a
/// heading, raw HTML, a footnote and the text of an image are dim, and the term
/// of a definition is bold.
#[derive(Clone, Debug)]
struct PreviewStyles;

impl StyleSheet for PreviewStyles {
    fn heading(&self, _level: u8) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    fn code(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn link(&self) -> Style {
        Style::new().add_modifier(Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> Style {
        Style::new()
            .add_modifier(Modifier::DIM)
            .add_modifier(Modifier::ITALIC)
    }

    fn metadata_block(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn math_inline(&self) -> Style {
        Style::new().add_modifier(Modifier::ITALIC)
    }

    fn math_display(&self) -> Style {
        Style::new().add_modifier(Modifier::ITALIC)
    }

    fn alert(&self, _kind: AlertKind) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    fn table_header(&self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    fn table_border(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    /// No `#` before a heading. The heading is bold, and bold says the same
    /// thing in a narrow field at no cost in columns.
    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    /// No fence line above and below a code block. A fence costs a whole row
    /// and says nothing that the dim text does not.
    fn code_block_fence(&self) -> &str {
        ""
    }

    /// No icon before an alert. The icons are emoji, and terminals do not
    /// agree on how many columns an emoji takes.
    fn alert_icon(&self, _kind: AlertKind) -> &str {
        ""
    }
}

/// The lines that a message draws as, each wrapped to `field` columns.
///
/// A blank line of the markdown draws nothing. The block under a row is a few
/// lines tall, and a row spent on a paragraph break is a row not spent on what
/// the agent said.
///
/// A table row is the one line that is not wrapped. See [`table_row`].
/// A zero-cell field produces no lines. An oversized grapheme becomes an
/// ellipsis on its own line. Other graphemes stay whole across style boundaries.
pub fn message_lines(message: &str, field: usize) -> Vec<Vec<TextRun>> {
    if field == 0 {
        return Vec::new();
    }
    let options = Options::new(PreviewStyles);
    let mut lines: Vec<Vec<TextRun>> = Vec::new();
    for line in tui_markdown::from_str_with_options(message, &options)
        .lines
        .iter()
    {
        let graphemes = runs_of(&crate::render::elide(line.clone(), usize::MAX));
        match table_row(&graphemes) {
            true => lines.push(joined(&graphemes)),
            false => lines.extend(wrap_runs(&graphemes, field)),
        }
    }
    lines.retain(|line| !line.is_empty());
    lines
}

/// The parsed message lines, clipped to `field` columns without wrapping.
/// Blank lines draw nothing. Each clipped line retains its inline emphasis.
pub fn clipped_message_lines(message: &str, field: usize) -> Vec<Vec<TextRun>> {
    let options = Options::new(PreviewStyles);
    tui_markdown::from_str_with_options(message, &options)
        .lines
        .iter()
        .filter(|line| {
            line.spans
                .iter()
                .any(|span| !span.content.trim().is_empty())
        })
        .map(|line| joined(&runs_of(&crate::render::elide(line.clone(), field))))
        .collect()
}

/// The glyphs that the crate draws the frame of a table with: the top, the rule
/// under the headings, the foot, and the left edge of a row.
const TABLE_EDGES: [char; 4] = ['┌', '├', '└', '│'];

/// Whether the parser drew this line as part of a table.
///
/// A table row is drawn on one line however wide it is, and the row drawing
/// cuts what is wider than the pane. A wrapped table row loses the columns that
/// a table is for, which is worse than a cut end.
///
/// The crate returns text and no structure with it, so the first glyph of the
/// line is the only evidence there is. A paragraph that starts with a table
/// glyph is cut rather than wrapped, and loses the end of a line longer than
/// the pane.
fn table_row(graphemes: &[TextRun]) -> bool {
    graphemes.first().is_some_and(|first| {
        first
            .text
            .chars()
            .next()
            .is_some_and(|c| TABLE_EDGES.contains(&c))
    })
}

/// The grapheme runs of a line normalized by [`crate::render::elide`].
///
/// A line carries a style of its own, and every span of it carries a second.
/// The style of the line is what a heading and a quote arrive as, so the two
/// are merged before the emphasis is read.
fn runs_of(line: &Line<'_>) -> Vec<TextRun> {
    line.spans
        .iter()
        // elide returns one complete grapheme per span. Match Buffer's
        // discard policy before joining runs, so controls cannot reach output.
        .filter(|span| crate::render::display_width(&span.content) != 0)
        .map(|span| TextRun {
            text: span.content.to_string(),
            emphasis: emphasis_of(line.style.patch(span.style)),
        })
        .collect()
}

/// The emphasis that a style carries. This drops the colors of the style, so
/// [`PreviewStyles`] sets none.
fn emphasis_of(style: Style) -> TextEmphasis {
    TextEmphasis {
        bold: style.add_modifier.contains(Modifier::BOLD),
        italic: style.add_modifier.contains(Modifier::ITALIC),
        dim: style.add_modifier.contains(Modifier::DIM),
        underlined: style.add_modifier.contains(Modifier::UNDERLINED),
        crossed_out: style.add_modifier.contains(Modifier::CROSSED_OUT),
    }
}

/// Wrap complete graphemes from [`crate::render::elide`] to terminal cells.
/// That helper assigns each grapheme the emphasis of its first byte, even when
/// markdown splits a grapheme across spans. Run boundaries do not break words.
/// The last whitespace that fits is dropped; other whitespace stays in place.
fn wrap_runs(graphemes: &[TextRun], field: usize) -> Vec<Vec<TextRun>> {
    if field == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut start = 0;
    while start < graphemes.len() {
        let mut room = field;
        let mut limit = start;
        while limit < graphemes.len() {
            let width = crate::render::display_width(&graphemes[limit].text);
            if width > room {
                break;
            }
            room -= width;
            limit += 1;
        }
        if limit == start {
            // Consume an oversized grapheme so even a one-cell field advances.
            lines.push(vec![TextRun {
                text: crate::render::cut_to_columns(&graphemes[start].text, field),
                emphasis: graphemes[start].emphasis,
            }]);
            start += 1;
            continue;
        }
        let (end, next) = if limit == graphemes.len() {
            (limit, limit)
        } else {
            break_before(graphemes, start, limit).unwrap_or((limit, limit))
        };
        let line = joined(&graphemes[start..end]);
        if !line.is_empty() {
            lines.push(line);
        }
        start = next;
    }
    lines
}

/// A space with a combining mark is visible text, not a word separator.
fn whitespace(grapheme: &TextRun) -> bool {
    grapheme.text.chars().all(char::is_whitespace)
}

/// Break at the last whitespace boundary that fits, including the first
/// grapheme outside the field. Drop the whole whitespace run at that boundary.
fn break_before(graphemes: &[TextRun], start: usize, limit: usize) -> Option<(usize, usize)> {
    let end = (start + 1..=limit)
        .rev()
        .find(|at| whitespace(&graphemes[*at]) && !whitespace(&graphemes[at - 1]))?;
    let next = (end..graphemes.len())
        .find(|at| !whitespace(&graphemes[*at]))
        .unwrap_or(graphemes.len());
    Some((end, next))
}

/// Join adjacent graphemes with equal emphasis and drop trailing whitespace.
fn joined(graphemes: &[TextRun]) -> Vec<TextRun> {
    let kept = match graphemes.iter().rposition(|g| !whitespace(g)) {
        Some(last) => &graphemes[..=last],
        None => &[][..],
    };
    let mut runs: Vec<TextRun> = Vec::new();
    for grapheme in kept {
        match runs.last_mut() {
            Some(last) if last.emphasis == grapheme.emphasis => last.text.push_str(&grapheme.text),
            _ => runs.push(grapheme.clone()),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_width_message_returns_no_lines() {
        assert!(message_lines("x", 0).is_empty());
        assert!(message_lines("│ table", 0).is_empty());
    }

    #[test]
    fn wrapped_graphemes_keep_buffer_cells_and_first_byte_emphasis() {
        use ratatui_core::{buffer::Buffer, layout::Rect, text::Span};

        for (message, field, expected) in [
            ("**界**X", 2, vec![vec!["界", " "], vec!["X"]]),
            ("**界**X", 1, vec![vec!["…"], vec!["X"]]),
            ("**e**\u{301}X", 1, vec![vec!["e\u{301}"], vec!["X"]]),
            ("**e**\u{301}", 1, vec![vec!["e\u{301}"]]),
            ("**✈**\u{fe0f}X", 2, vec![vec!["✈️", " "], vec!["X"]]),
            ("**👩**\u{200d}💻X", 2, vec![vec!["👩‍💻", " "], vec!["X"]]),
            ("**🇳**🇱X", 2, vec![vec!["🇳🇱", " "], vec!["X"]]),
            ("**👍**🏽X", 2, vec![vec!["👍🏽", " "], vec!["X"]]),
            ("**👍**🏽X", 1, vec![vec!["…"], vec!["X"]]),
            ("**a \u{301}**X", 2, vec![vec!["a", " \u{301}"], vec!["X"]]),
            ("**a \u{301}**", 2, vec![vec!["a", " \u{301}"]]),
            ("**👩**\u{200d}💻", 2, vec![vec!["👩‍💻", " "]]),
            ("**✈**\u{fe0f}", 1, vec![vec!["…"]]),
            ("**🇳**🇱", 1, vec![vec!["…"]]),
            ("**ab**X", 2, vec![vec!["a", "b"], vec!["X"]]),
            ("**界**X", 3, vec![vec!["界", " ", "X"]]),
        ] {
            // Raw parser-style spans also cover sequences for which markdown
            // punctuation rules do not recognize an emphasis delimiter.
            let (bold, plain) = message
                .strip_prefix("**")
                .unwrap()
                .split_once("**")
                .unwrap();
            let parsed = Line::from(vec![
                Span::styled(bold, Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(plain),
            ]);
            let graphemes = runs_of(&crate::render::elide(parsed, usize::MAX));
            let lines = wrap_runs(&graphemes, field);
            assert_eq!(lines.len(), expected.len(), "{message:?} field={field}");
            for (runs, symbols) in lines.iter().zip(expected) {
                let line = Line::from(
                    runs.iter()
                        .map(|run| {
                            let style = if run.emphasis.bold {
                                Style::new().add_modifier(Modifier::BOLD)
                            } else {
                                Style::new()
                            };
                            Span::styled(run.text.clone(), style)
                        })
                        .collect::<Vec<_>>(),
                );
                let text: String = runs.iter().map(|run| run.text.as_str()).collect();
                assert!(crate::render::display_width(&text) <= field);
                let mut buffer = Buffer::empty(Rect::new(0, 0, field as u16 + 2, 1));
                buffer[(0, 0)]
                    .set_symbol("L")
                    .set_style(Style::new().add_modifier(Modifier::ITALIC));
                buffer[(field as u16 + 1, 0)]
                    .set_symbol("R")
                    .set_style(Style::new().add_modifier(Modifier::DIM));
                let left = buffer[(0, 0)].clone();
                let right = buffer[(field as u16 + 1, 0)].clone();
                buffer.set_line(1, 0, &line, field as u16);
                for (offset, symbol) in symbols.iter().enumerate() {
                    let cell = &buffer[(offset as u16 + 1, 0)];
                    assert_eq!(cell.symbol(), *symbol, "{message:?} field={field}");
                    if *symbol != " " {
                        assert_eq!(
                            cell.modifier.contains(Modifier::BOLD),
                            *symbol != "X",
                            "{message:?} {symbol:?}"
                        );
                    }
                }
                for offset in symbols.len()..field {
                    assert_eq!(buffer[(offset as u16 + 1, 0)].symbol(), " ");
                }
                assert_eq!(buffer[(0, 0)], left);
                assert_eq!(buffer[(field as u16 + 1, 0)], right);
            }
        }
    }

    #[test]
    fn controls_and_standalone_zero_width_graphemes_are_discarded() {
        use ratatui_core::{buffer::Buffer, layout::Rect, text::Span};
        let parsed = Line::from(vec![Span::styled(
            "\u{301}A\t\u{7}界\r\nB",
            Style::new().add_modifier(Modifier::ITALIC),
        )]);
        let graphemes = runs_of(&crate::render::elide(parsed, usize::MAX));
        let lines = wrap_runs(&graphemes, 4);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 1);
        assert_eq!(lines[0][0].text, "A界B");
        assert!(lines[0][0].emphasis.italic);
        assert!(wrap_runs(&graphemes, 0).is_empty());
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        buffer[(0, 0)].set_symbol("L");
        buffer[(5, 0)].set_symbol("R");
        buffer.set_line(
            1,
            0,
            &Line::from(Span::styled(
                lines[0][0].text.clone(),
                Style::new().add_modifier(Modifier::ITALIC),
            )),
            4,
        );
        for (x, symbol) in ["L", "A", "界", " ", "B", "R"].iter().enumerate() {
            assert_eq!(buffer[(x as u16, 0)].symbol(), *symbol);
        }
        for x in [1, 2, 4] {
            assert!(buffer[(x, 0)].modifier.contains(Modifier::ITALIC));
        }
        assert!(message_lines("\u{7}", 1).is_empty());
    }

    #[test]
    fn parsed_unicode_wraps_without_changing_ascii_run_grouping() {
        assert_eq!(drawn("**e**\u{301}X", 1), vec!["e\u{301}", "X"]);
        assert_eq!(drawn("界 界", 2), vec!["界", "界"]);
        assert_eq!(drawn("a \u{301}X", 2), vec!["a \u{301}", "X"]);
        assert_eq!(drawn("a \u{301}", 2), vec!["a \u{301}"]);
        let lines = message_lines("a **re**start now", 10);
        assert_eq!(
            lines[0].iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            vec!["a ", "re", "start"]
        );
        assert!(lines[0][1].emphasis.bold);
        assert!(!lines[0][2].emphasis.bold);
    }

    #[test]
    fn clipped_unicode_preview_preserves_buffer_graphemes() {
        use ratatui_core::{buffer::Buffer, layout::Rect, text::Span};

        for (message, field, expected) in [
            ("**e**\u{301}X", 1, vec!["…"]),
            ("**e**\u{301}", 1, vec!["e\u{301}"]),
            ("界X", 2, vec!["…"]),
            ("界X", 3, vec!["界", " ", "X"]),
            ("✈️X", 2, vec!["…"]),
            ("👩‍💻X", 2, vec!["…"]),
            ("👩‍💻X", 3, vec!["👩‍💻", " ", "X"]),
            ("ASCII", 3, vec!["A", "S", "…"]),
            ("界", 0, vec![]),
        ] {
            let runs = clipped_message_lines(message, field);
            let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
            buffer[(0, 0)].set_symbol("L");
            buffer[(field as u16 + 1, 0)].set_symbol("R");
            if let Some(runs) = runs.first() {
                let line = Line::from(
                    runs.iter()
                        .map(|run| Span::raw(run.text.clone()))
                        .collect::<Vec<_>>(),
                );
                buffer.set_line(1, 0, &line, field as u16);
            }
            for (offset, symbol) in expected.iter().enumerate() {
                assert_eq!(
                    buffer[(1 + offset as u16, 0)].symbol(),
                    *symbol,
                    "{message:?} field={field}"
                );
            }
            for offset in expected.len()..field {
                assert_eq!(buffer[(1 + offset as u16, 0)].symbol(), " ");
            }
            assert_eq!(buffer[(0, 0)].symbol(), "L");
            assert_eq!(buffer[(field as u16 + 1, 0)].symbol(), "R");
        }
    }

    /// The text of every line, with nothing said about emphasis.
    fn drawn(message: &str, field: usize) -> Vec<String> {
        message_lines(message, field)
            .into_iter()
            .map(|runs| runs.iter().map(|run| run.text.as_str()).collect())
            .collect()
    }

    /// The emphasis of the run that holds `text`. A quote draws its `>` in the
    /// same run as the words after it, so the run is found by what it holds
    /// rather than by what it equals.
    fn emphasis(message: &str, text: &str) -> TextEmphasis {
        message_lines(message, 80)
            .into_iter()
            .flatten()
            .find(|run| run.text.contains(text))
            .unwrap_or_else(|| panic!("no run drew {text}"))
            .emphasis
    }

    #[test]
    fn a_plain_message_draws_as_it_was_written() {
        assert_eq!(drawn("the port is done", 80), vec!["the port is done"]);
    }

    #[test]
    fn the_markers_around_emphasis_are_not_drawn() {
        assert_eq!(
            drawn("the **port** is `done`", 80),
            vec!["the port is done"]
        );
    }

    #[test]
    fn a_heading_is_bold_and_keeps_no_marker() {
        assert_eq!(drawn("## The port\n\ndone", 80), vec!["The port", "done"]);
        assert!(emphasis("## The port", "The port").bold);
    }

    #[test]
    fn every_role_takes_the_effect_that_the_style_sheet_gives_it() {
        assert!(emphasis("a **bold** word", "bold").bold);
        assert!(emphasis("a *slanted* word", "slanted").italic);
        assert!(emphasis("a `quoted` word", "quoted").dim);
        assert!(emphasis("a ~~cut~~ word", "cut").crossed_out);
        assert!(emphasis("a [link](https://example.com) here", "link").underlined);
        assert!(emphasis("> a quote", "a quote").dim);
    }

    #[test]
    fn a_list_item_keeps_its_bullet() {
        assert_eq!(
            drawn("- one item\n- two item", 80),
            vec!["- one item", "- two item"]
        );
    }

    #[test]
    fn a_code_block_draws_no_fence() {
        assert_eq!(drawn("```rust\nlet x = 1;\n```", 80), vec!["let x = 1;"]);
    }

    #[test]
    fn a_blank_line_draws_no_line() {
        assert_eq!(drawn("one\n\n\ntwo", 80), vec!["one", "two"]);
        assert!(drawn("", 80).is_empty());
        assert!(drawn("   ", 80).is_empty());
    }

    #[test]
    fn wrapping_breaks_on_whitespace_and_splits_a_word_too_long_to_fit() {
        assert_eq!(drawn("one two three", 7), vec!["one two", "three"]);
        assert_eq!(
            drawn("supercalifragilistic", 8),
            vec!["supercal", "ifragili", "stic"]
        );
    }

    #[test]
    fn a_break_inside_an_emphasised_run_keeps_the_emphasis_on_both_lines() {
        let lines = message_lines("**one two three**", 7);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().flatten().all(|run| run.emphasis.bold));
    }

    #[test]
    fn a_word_that_two_runs_share_is_never_broken_in_the_middle() {
        // `**re**start` is one word of two runs. A field wide enough for the
        // word holds both runs on one line.
        let lines = message_lines("a **re**start now", 10);
        assert_eq!(
            lines
                .iter()
                .map(|runs| runs.iter().map(|run| run.text.as_str()).collect::<String>())
                .collect::<Vec<String>>(),
            vec!["a restart", "now"]
        );
    }

    #[test]
    fn a_table_keeps_the_columns_that_the_parser_lined_up() {
        let table = "| Crate | Role |\n| --- | --- |\n| core | records |\n| ui | rows |";
        assert_eq!(
            drawn(table, 80),
            vec![
                "┌───────┬─────────┐",
                "│ Crate │ Role    │",
                "├───────┼─────────┤",
                "│ core  │ records │",
                "│ ui    │ rows    │",
                "└───────┴─────────┘",
            ]
        );
    }

    #[test]
    fn a_table_wider_than_the_field_stays_on_one_line_each() {
        // A wrapped table row loses its columns, so the row drawing cuts the
        // line at the edge of the pane instead.
        let table = "| Crate | Role |\n| --- | --- |\n| core | records |";
        let lines = drawn(table, 10);
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|line| line.chars().count() > 10));
    }

    #[test]
    fn a_code_block_keeps_the_indentation_that_was_written() {
        assert_eq!(
            drawn("```rust\nfn main() {\n    let x = 1;\n}\n```", 80),
            vec!["fn main() {", "    let x = 1;", "}"]
        );
    }

    #[test]
    fn a_break_drops_the_whitespace_it_falls_on_and_keeps_the_rest() {
        assert_eq!(drawn("one   two   three", 11), vec!["one   two", "three"]);
    }

    #[test]
    fn neighbours_that_share_an_emphasis_draw_as_one_run() {
        let lines = message_lines("one two three", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 1);
    }
}
