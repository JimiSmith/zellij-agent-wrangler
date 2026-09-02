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
pub fn message_lines(message: &str, field: usize) -> Vec<Vec<TextRun>> {
    let options = Options::new(PreviewStyles);
    let mut lines: Vec<Vec<TextRun>> = Vec::new();
    for line in tui_markdown::from_str_with_options(message, &options)
        .lines
        .iter()
    {
        let characters = characters_of(runs_of(line));
        match table_row(&characters) {
            true => lines.push(joined(&characters)),
            false => lines.extend(wrap_runs(&characters, field)),
        }
    }
    lines.retain(|line| !line.is_empty());
    lines
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
fn table_row(characters: &[Character]) -> bool {
    matches!(characters.first(), Some((first, _)) if TABLE_EDGES.contains(first))
}

/// The runs of one parsed line.
///
/// A line carries a style of its own, and every span of it carries a second.
/// The style of the line is what a heading and a quote arrive as, so the two
/// are merged before the emphasis is read.
fn runs_of(line: &Line<'_>) -> Vec<TextRun> {
    line.spans
        .iter()
        .filter(|span| !span.content.is_empty())
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

/// One character of a line, and the emphasis of the run it came from.
///
/// A line is walked one character at a time rather than one run at a time.
/// Markdown divides a word into two runs when it emphasises part of the word,
/// as in `**re**start`, so a run boundary says nothing about where a line can
/// break.
type Character = (char, TextEmphasis);

/// `characters` wrapped to `field` columns, in the lines they become.
///
/// The break falls on the last whitespace that fits, and inside a word that is
/// longer than the field. The whitespace at a break is dropped, and every other
/// space is kept as the markdown wrote it. A table and a code block therefore
/// keep the columns that the parser lined them up in.
///
/// A line of whitespace alone becomes no line at all.
fn wrap_runs(characters: &[Character], field: usize) -> Vec<Vec<TextRun>> {
    let mut lines: Vec<Vec<TextRun>> = Vec::new();
    let mut start = 0;
    while characters.len() - start > field {
        let limit = start + field;
        let (end, next) = break_before(characters, start, limit).unwrap_or((limit, limit));
        lines.push(joined(&characters[start..end]));
        start = next;
    }
    if drawn(&characters[start..]) {
        lines.push(joined(&characters[start..]));
    }
    lines
}

/// The characters of a line, each carrying the emphasis of the run it is in.
fn characters_of(runs: Vec<TextRun>) -> Vec<Character> {
    let mut characters: Vec<Character> = Vec::new();
    for run in runs {
        for character in run.text.chars() {
            characters.push((character, run.emphasis));
        }
    }
    characters
}

/// The end of a line that starts at `start` and reaches no further than
/// `limit`, and the start of the line after it. `None` for a line with no
/// whitespace to break on, which is cut at the limit instead.
///
/// The break falls where the last whitespace of the line begins, and the whole
/// run of whitespace there is dropped.
fn break_before(characters: &[Character], start: usize, limit: usize) -> Option<(usize, usize)> {
    let end = (start + 1..=limit)
        .rev()
        .find(|at| characters[*at].0.is_whitespace() && !characters[at - 1].0.is_whitespace())?;
    let next = (end..characters.len())
        .find(|at| !characters[*at].0.is_whitespace())
        .unwrap_or(characters.len());
    Some((end, next))
}

/// Whether `characters` holds anything that a reader can see. A line of
/// whitespace alone is drawn as no line, the way a blank line is.
fn drawn(characters: &[Character]) -> bool {
    characters
        .iter()
        .any(|(character, _)| !character.is_whitespace())
}

/// The runs of one line, with every neighbour that shares an emphasis joined
/// into one. The trailing whitespace is dropped, which costs a row no columns
/// and moves nothing that a reader can see.
fn joined(characters: &[Character]) -> Vec<TextRun> {
    let kept = match characters.iter().rposition(|(c, _)| !c.is_whitespace()) {
        Some(last) => &characters[..=last],
        None => &[][..],
    };
    let mut runs: Vec<TextRun> = Vec::new();
    for (character, emphasis) in kept {
        match runs.last_mut() {
            Some(last) if last.emphasis == *emphasis => last.text.push(*character),
            _ => runs.push(TextRun {
                text: character.to_string(),
                emphasis: *emphasis,
            }),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

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
