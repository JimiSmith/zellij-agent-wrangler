//! Turning a row's structure into the line drawn for it.
//!
//! Every glyph the sidebar shows that is not the literal name of a thing is
//! chosen here: the gutter marking where you are, the icon marking what kind of
//! thing the row is, the tree branches, the index prefix, the heading's spacing
//! and case, and the styling.
//!
//! A row is drawn as a run of [`Segment`]s rather than one styled line, which is
//! what lets a pane's or agent's color sit on its icon alone while the name
//! beside it stays in the terminal's default.

use crate::model::{
    Branch, Indicator, NamedColor, Notification, Placement, Row, RowContent, RowKey,
};

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

/// The columns a description line has for its text in a pane `width` columns
/// wide: the indent comes off the front and the reserved right-hand column off
/// the end. Never zero, so wrapping to it always terminates.
pub fn notification_body_field(width: usize) -> usize {
    width.saturating_sub(BODY_INDENT.len() + 1).max(1)
}

/// How a piece of a row is drawn. Intensity says where you are, color says what
/// the thing is, and reverse marks the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub dim: bool,
    pub underlined: bool,
    pub reverse: bool,
    pub fg: Option<NamedColor>,
}

impl Style {
    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    fn underlined(mut self) -> Self {
        self.underlined = true;
        self
    }

    fn fg(mut self, color: NamedColor) -> Self {
        self.fg = Some(color);
        self
    }

    /// The style a selected row's pieces draw in: reverse video, with the color
    /// and the dimming of whatever it covers dropped.
    ///
    /// Under reverse video both land on what is now the background, so a colored
    /// icon would paint a block of color across the selected row and a dimmed one
    /// would wash it out. Pointing at another tab is what the sidebar is for, so
    /// the selected row is usually a dimmed one.
    pub fn selected(self) -> Self {
        Style {
            reverse: true,
            dim: false,
            fg: None,
            ..self
        }
    }

    /// The SGR sequence opening this style, empty when it is the default.
    pub fn open(self) -> String {
        let mut params: Vec<String> = Vec::new();
        if self.bold {
            params.push("1".to_string());
        }
        if self.dim {
            params.push("2".to_string());
        }
        if self.underlined {
            params.push("4".to_string());
        }
        if self.reverse {
            params.push("7".to_string());
        }
        if let Some(color) = self.fg {
            params.push(color.sgr().to_string());
        }
        if params.is_empty() {
            String::new()
        } else {
            format!("\u{1b}[{}m", params.join(";"))
        }
    }
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

/// The line drawn for a row, before it is fitted to the pane width. The paint
/// composes the same line out of styled pieces, so this is where a change to the
/// glyphs around a name is asserted.
#[allow(dead_code)]
pub fn row_text(content: &RowContent) -> String {
    match parts(content) {
        Parts::Whole(text) => text,
        Parts::Split {
            head, icon, tail, ..
        } => format!("{head}{icon}{tail}"),
    }
}

/// One piece of a drawn row: its text and the style that piece carries.
#[derive(Clone, Debug)]
pub struct Segment {
    pub text: String,
    pub style: Style,
}

/// The styled pieces a row is drawn as, before they are fitted to the pane
/// width.
///
/// A child's color rides on its icon alone. A whole row in an agent's color
/// drowns the list once more than a couple of agents are up, and the icon is
/// enough to tie the row to the thing it points at.
pub fn row_segments(content: &RowContent) -> Vec<Segment> {
    let base = base_style(content);
    match parts(content) {
        Parts::Whole(text) => vec![Segment { text, style: base }],
        Parts::Split {
            head,
            icon,
            tail,
            color,
        } => vec![
            Segment {
                text: head,
                style: base,
            },
            Segment {
                text: icon.to_string(),
                style: own_color(base, color),
            },
            Segment {
                text: tail,
                style: base,
            },
        ],
    }
}

/// Truncate or pad `text` to exactly `field` columns, counting characters.
pub fn fit(text: &str, field: usize) -> String {
    let count = text.chars().count();
    if count > field {
        text.chars().take(field).collect()
    } else {
        let mut out = text.to_string();
        out.push_str(&" ".repeat(field - count));
        out
    }
}

/// Fit a row's segments to `field` columns, truncating or padding the line as a
/// whole.
///
/// Only the tail of the line moves, so a truncated segment empties from the
/// right and the padding lands on the last segment, which is what makes the
/// selection bar span the full width rather than stopping at the name.
pub fn fit_segments(segments: Vec<Segment>, field: usize) -> Vec<Segment> {
    let joined: String = segments.iter().map(|s| s.text.as_str()).collect();
    let fitted = fit(&joined, field);
    let mut chars = fitted.chars();
    let mut out: Vec<Segment> = segments
        .into_iter()
        .map(|seg| Segment {
            text: chars.by_ref().take(seg.text.chars().count()).collect(),
            style: seg.style,
        })
        .collect();
    let padding: String = chars.collect();
    if let Some(last) = out.last_mut() {
        last.text.push_str(&padding);
    }
    out
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
        RowContent::Header { .. } => Style::default().bold().underlined(),
        RowContent::Blank => Style::default().dim(),
        RowContent::Window {
            placement, color, ..
        } => own_color(intensity(*placement), *color),
        RowContent::Pane { placement, .. } | RowContent::Agent { placement, .. } => {
            intensity(*placement)
        }
        RowContent::NotificationTitle { .. } => Style::default(),
        // Dimmed, so the title leads and the description reads as its detail.
        // Intensity is not available for that: it says where you are.
        RowContent::NotificationBody { .. } => Style::default().dim(),
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
        Placement::Here => Style::default().bold(),
        Placement::Focused => Style::default(),
        Placement::Unfocused => Style::default().dim(),
    }
}

/// Apply a thing's own color, leaving the style untouched when it has none.
fn own_color(style: Style, color: Option<NamedColor>) -> Style {
    match color {
        Some(c) => style.fg(c),
        None => style,
    }
}

/// The finished line for `row` in a pane `width` columns wide, as the escape
/// sequences and text to print.
///
/// The rightmost column is reserved for the turn-state indicator, so a long name
/// is cut before it can collide with one. A selected row is drawn in reverse
/// video across the full width, indicator column included.
pub fn paint(row: &Row, width: usize, selected: bool) -> String {
    let field = width.saturating_sub(1);
    let segments = fit_segments(row_segments(&row.content), field);
    let base = base_style(&row.content);

    let mut out = String::new();
    for segment in segments {
        let style = if selected {
            segment.style.selected()
        } else {
            segment.style
        };
        out.push_str(&style.open());
        out.push_str(&segment.text);
        out.push_str("\u{1b}[0m");
    }

    let (glyph, color) = match row.indicator.resolve() {
        Some((glyph, color)) => (glyph, color),
        None => (' ', None),
    };
    let mut style = own_color(base, color);
    if selected {
        style = style.selected();
    }
    out.push_str(&style.open());
    out.push(glyph);
    out.push_str("\u{1b}[0m");
    out
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

/// The rows a notification entry is drawn as: its title over the wrapped lines
/// of its message.
/// Every line carries the entry's own key, so a click anywhere in it lands on
/// the same thing.
pub fn notification_rows(entry: &Notification, index: usize, width: usize) -> Vec<Row> {
    let key = RowKey::Notification(index);
    let mut rows = vec![Row::new(RowContent::NotificationTitle {
        title: entry.agent.clone(),
        color: entry.color,
    })
    .with(Indicator::Attention)
    .at(key.clone())];
    for line in wrap(&entry.message, notification_body_field(width)) {
        rows.push(Row::new(RowContent::NotificationBody { text: line }).at(key.clone()));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_description_line_starts_under_its_titles_text() {
        let title = row_text(&RowContent::NotificationTitle {
            title: "claude".to_string(),
            color: None,
        });
        let body = row_text(&RowContent::NotificationBody {
            text: "vim · api".to_string(),
        });
        assert_eq!(title, " \u{f167a}  claude");
        assert_eq!(body, "    vim · api");
        // Columns, not byte offsets: the icon is one column and several bytes.
        let column = |line: &str, text: &str| line.chars().count() - text.chars().count();
        assert_eq!(column(&title, "claude"), column(&body, "vim · api"));
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
        let segments = row_segments(&content);
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["  └─ 0: ", "\u{f167a}", "  a"]);
        assert!(segments[1].style.fg.is_some(), "the icon carries the color");
        assert!(segments[0].style.fg.is_none(), "the tree stays default");
        assert!(segments[2].style.fg.is_none(), "the name stays default");
        // Dimming is the placement channel and the color the identity one, so an
        // unfocused agent keeps its icon color rather than trading it for dim.
        assert!(segments.iter().all(|s| s.style.dim));
    }

    #[test]
    fn only_a_row_you_are_on_is_bold_and_only_a_tab_you_are_not_in_is_dim() {
        assert!(base_style(&tab("1", "w", Placement::Here)).bold);
        assert!(!base_style(&tab("1", "w", Placement::Unfocused)).bold);
        for content in [
            tab("2", "w", Placement::Unfocused),
            pane("0", "p", Branch::Last, Placement::Unfocused),
            agent("0", "a", Branch::More, Placement::Unfocused),
        ] {
            assert!(base_style(&content).dim, "{content:?}");
        }
        for content in [
            tab("1", "w", Placement::Here),
            pane("0", "p", Branch::Last, Placement::Here),
            agent("0", "a", Branch::More, Placement::Focused),
        ] {
            assert!(!base_style(&content).dim, "{content:?}");
        }
    }

    #[test]
    fn selecting_a_row_drops_the_color_and_the_dimming_it_covers() {
        // Under reverse video both land on what is now the background.
        let style = Style::default().dim().fg(NamedColor::Cyan).selected();
        assert!(style.reverse);
        assert!(!style.dim);
        assert_eq!(style.fg, None);
    }

    #[test]
    fn fitting_pads_the_last_segment_and_empties_the_others_from_the_right() {
        let row = agent("0", "a", Branch::Last, Placement::Focused);
        let width = row_text(&row).chars().count();

        let padded = fit_segments(row_segments(&row), width + 3);
        let texts: Vec<String> = padded.iter().map(|s| s.text.clone()).collect();
        assert_eq!(texts[0], "  └─ 0: ", "the tree is untouched");
        assert_eq!(texts[1], "\u{f167a}", "the icon keeps its own segment");
        assert!(
            texts[2].ends_with("   "),
            "padding lands on the last segment"
        );

        // Narrower than the prefix: the tail empties, the head is what survives.
        let cut = fit_segments(row_segments(&row), 2);
        assert_eq!(cut.iter().map(|s| s.text.chars().count()).sum::<usize>(), 2);
        assert_eq!(cut[2].text, "");
    }

    #[test]
    fn every_painted_line_is_exactly_the_pane_width() {
        // The selection bar spans the pane, so a short row is padded to it and a
        // long one cut, indicator column included.
        let strip = |line: String| {
            let mut out = String::new();
            let mut chars = line.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        for width in [10usize, 24, 60] {
            let row = Row::new(agent(
                "1",
                "claude · a rather long session label",
                Branch::More,
                Placement::Here,
            ))
            .with(Indicator::Attention);
            assert_eq!(strip(paint(&row, width, false)).chars().count(), width);
            assert_eq!(strip(paint(&row, width, true)).chars().count(), width);
        }
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
