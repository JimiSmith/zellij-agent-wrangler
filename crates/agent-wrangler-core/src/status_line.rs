//! The line that hangs under an agent's row, spelled from a template that the
//! user writes.
//!
//! A record holds what a session works with. This module spells the line. The
//! split is the same one that [`label`] makes: the code that collects a fact
//! does not decide how a reader sees it.
//!
//! A template is parsed once, when the options are read. Drawing a row
//! therefore runs no check and cannot fail.
//!
//! [`short_model_name`] and [`short_token_count`] are public, because the
//! dashboard spells the same two values in its own columns. Both views then
//! read a model and a count the same way.
//!
//! [`label`]: crate::label::label

use crate::agent::Agent;

/// The word that opens a field, and the word that closes one.
const OPEN: char = '{';
const CLOSE: char = '}';

/// The name that the user writes for each value. A user types these words, so
/// they are fixed and never renamed.
const BRANCH: &str = "branch";
const MODEL: &str = "model";
const CONTEXT_TOKENS: &str = "context_tokens";

/// The prefix that every Claude model id carries, and the length of the date
/// that some of them end with.
const MODEL_PREFIX: &str = "claude-";
const MODEL_DATE: usize = 8;

/// How many tokens a count draws in full before it switches to thousands.
const IN_FULL: u64 = 1000;

/// One value that a status line can draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusField {
    Branch,
    Model,
    ContextTokens,
}

impl StatusField {
    /// The field that a written name asks for, or `None` for a name that this
    /// build does not know.
    fn read(name: &str) -> Option<Self> {
        match name {
            BRANCH => Some(StatusField::Branch),
            MODEL => Some(StatusField::Model),
            CONTEXT_TOKENS => Some(StatusField::ContextTokens),
            _ => None,
        }
    }

    /// What this field says about one session. An empty answer draws nothing.
    fn spell(self, agent: &Agent) -> String {
        match self {
            StatusField::Branch => agent.status.branch.clone(),
            StatusField::Model => short_model_name(&agent.status.model),
            StatusField::ContextTokens => short_token_count(agent.status.context_tokens),
        }
    }
}

/// The model under a name short enough for a narrow pane.
///
/// The record keeps the id that the agent wrote, because the id is the fact.
/// This drops the prefix that every id carries and the date that some ids end
/// with, so `claude-opus-4-5-20251101` reads `opus-4-5`.
pub fn short_model_name(id: &str) -> String {
    let named = id.strip_prefix(MODEL_PREFIX).unwrap_or(id);
    match named.rsplit_once('-') {
        Some((head, tail))
            if tail.len() == MODEL_DATE && tail.chars().all(|c| c.is_ascii_digit()) =>
        {
            head.to_string()
        }
        _ => named.to_string(),
    }
}

/// A count of tokens, short enough for a narrow field.
///
/// A count of nothing draws nothing, because a session that has answered
/// nothing has spent no context. Anything from a thousand up draws in
/// thousands, rounded to the nearest one.
pub fn short_token_count(tokens: u64) -> String {
    match tokens {
        0 => String::new(),
        counted if counted < IN_FULL => counted.to_string(),
        counted => format!("{}k", (counted + IN_FULL / 2) / IN_FULL),
    }
}

/// One piece of a template: text to draw as written, or a value to look up.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Field(StatusField),
}

/// A status line template, already parsed.
///
/// A name that this build does not know stays as the user wrote it, braces and
/// all. A mistake therefore appears on the pane rather than disappearing in
/// silence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusTemplate(Vec<Piece>);

impl StatusTemplate {
    /// The template that `text` asks for, or `None` for text that asks for no
    /// line at all.
    pub fn new(text: &str) -> Option<Self> {
        if text.trim().is_empty() {
            return None;
        }
        let mut pieces: Vec<Piece> = Vec::new();
        let mut literal = String::new();
        let mut rest = text;
        while let Some(opened) = rest.find(OPEN) {
            let after = &rest[opened + OPEN.len_utf8()..];
            // A brace with no partner, and a name that this build does not
            // know, are both text that the user wrote. Both are drawn as
            // written.
            let named = after.find(CLOSE).and_then(|closed| {
                StatusField::read(&after[..closed]).map(|field| (field, closed))
            });
            let Some((field, closed)) = named else {
                literal.push_str(&rest[..opened + OPEN.len_utf8()]);
                rest = after;
                continue;
            };
            literal.push_str(&rest[..opened]);
            if !literal.is_empty() {
                pieces.push(Piece::Literal(std::mem::take(&mut literal)));
            }
            pieces.push(Piece::Field(field));
            rest = &after[closed + CLOSE.len_utf8()..];
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            pieces.push(Piece::Literal(literal));
        }
        Some(StatusTemplate(pieces))
    }

    /// The line that this template spells for one agent, or `None` when the
    /// line says nothing about the session.
    ///
    /// A field with no value spells nothing, and the separators around it stay.
    /// So a template that names fields draws no row at all while every one of
    /// those fields is empty. Without that rule, a session that reports none of
    /// these values draws a row of bare separators.
    ///
    /// A template that names no field is text that the user asked for. Such a
    /// template draws that text whatever the record holds.
    pub fn spell(&self, agent: &Agent) -> Option<String> {
        let mut line = String::new();
        let mut fields = 0usize;
        let mut spoke = 0usize;
        for piece in &self.0 {
            match piece {
                Piece::Literal(text) => line.push_str(text),
                Piece::Field(field) => {
                    let value = field.spell(agent);
                    fields += 1;
                    spoke += usize::from(!value.is_empty());
                    line.push_str(&value);
                }
            }
        }
        if fields > 0 && spoke == 0 {
            return None;
        }
        match line.trim().is_empty() {
            true => None,
            false => Some(line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agent::tests::agent;
    use crate::agent::StatusFacts;

    fn working(branch: &str, model: &str, context_tokens: u64) -> Agent {
        agent("one", 1).with_status(StatusFacts {
            branch: branch.to_string(),
            model: model.to_string(),
            context_tokens,
        })
    }

    fn spelled(template: &str, agent: &Agent) -> Option<String> {
        StatusTemplate::new(template)?.spell(agent)
    }

    #[test]
    fn a_template_with_nothing_in_it_asks_for_no_line() {
        assert_eq!(StatusTemplate::new(""), None);
        assert_eq!(StatusTemplate::new("   "), None);
    }

    #[test]
    fn every_field_is_spelled_from_the_record() {
        let record = working("main", "claude-opus-5", 195_547);
        assert_eq!(
            spelled("{branch} · {model} · {context_tokens}", &record),
            Some("main · opus-5 · 196k".to_string())
        );
    }

    #[test]
    fn a_field_can_be_drawn_more_than_once_and_in_any_order() {
        let record = working("main", "claude-opus-5", 0);
        assert_eq!(
            spelled("{branch}/{branch}", &record),
            Some("main/main".to_string())
        );
    }

    #[test]
    fn a_name_this_build_does_not_know_stays_as_the_user_wrote_it() {
        // A dropped name leaves a mistake with no symptom. The pane shows it
        // instead.
        let record = working("main", "", 0);
        assert_eq!(
            spelled("{branch} {turn}", &record),
            Some("main {turn}".to_string())
        );
    }

    #[test]
    fn a_brace_with_no_partner_is_text() {
        let record = working("main", "", 0);
        assert_eq!(spelled("{ {branch}", &record), Some("{ main".to_string()));
        assert_eq!(spelled("100% {", &record), Some("100% {".to_string()));
    }

    #[test]
    fn a_template_of_text_alone_draws_that_text() {
        assert_eq!(
            spelled("nothing to report", &working("", "", 0)),
            Some("nothing to report".to_string())
        );
    }

    #[test]
    fn a_field_with_no_value_spells_nothing_and_leaves_its_separators() {
        let record = working("", "claude-opus-5", 0);
        assert_eq!(
            spelled("{branch} · {model} · {context_tokens}", &record),
            Some(" · opus-5 · ".to_string())
        );
    }

    #[test]
    fn a_template_whose_every_field_is_empty_draws_no_row() {
        // The whole reason for the rule: a session that reports none of these
        // values must not draw a row of bare separators.
        let record = working("", "", 0);
        assert_eq!(
            spelled("{branch} · {model} · {context_tokens}", &record),
            None
        );
    }

    #[test]
    fn one_field_with_something_to_say_is_enough_to_draw_the_row() {
        let record = working("", "", 41_000);
        assert_eq!(
            spelled("{branch} · {model} · {context_tokens}", &record),
            Some(" ·  · 41k".to_string())
        );
    }

    #[test]
    fn a_template_of_whitespace_around_a_field_draws_no_row_either() {
        assert_eq!(spelled("  {branch}  ", &working("", "", 0)), None);
    }

    #[test]
    fn a_model_drops_the_prefix_every_id_carries_and_the_date_some_carry() {
        for (id, want) in [
            ("claude-opus-5", "opus-5"),
            ("claude-opus-4-5-20251101", "opus-4-5"),
            ("claude-haiku-4-5-20251001", "haiku-4-5"),
            ("claude-opus-5[1m]", "opus-5[1m]"),
            ("gpt-5", "gpt-5"),
            ("", ""),
        ] {
            assert_eq!(short_model_name(id), want, "{id}");
        }
    }

    #[test]
    fn a_tail_of_digits_that_is_not_a_date_is_part_of_the_name() {
        // Only a run of exactly eight digits reads as a date.
        assert_eq!(short_model_name("claude-opus-4-5"), "opus-4-5");
        assert_eq!(short_model_name("claude-opus-123456789"), "opus-123456789");
    }

    #[test]
    fn a_count_of_tokens_is_short_enough_to_sit_beside_two_other_values() {
        for (tokens, want) in [
            (0, ""),
            (1, "1"),
            (999, "999"),
            (1_000, "1k"),
            (1_499, "1k"),
            (1_500, "2k"),
            (195_547, "196k"),
            (1_200_000, "1200k"),
        ] {
            assert_eq!(short_token_count(tokens), want, "{tokens}");
        }
    }
}
