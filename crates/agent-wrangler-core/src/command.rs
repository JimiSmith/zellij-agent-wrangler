//! This module reads a command line written as one string. It gives back the
//! words that a process is spawned from.
//!
//! The reader honors quotes. A path with a space survives the round trip from
//! the text a human wrote to the argument that a program receives.

/// Split `line` into words. A run inside single or double quotes is one word,
/// however much whitespace it holds.
///
/// A quote is a boundary, not a character. A quote opens a word even when the
/// text inside it is empty, so `''` is one empty argument rather than none. The
/// function recognizes no escape character. An unclosed quote runs to the end of
/// the line.
pub fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_line_splits_on_whitespace() {
        assert_eq!(words("notify-send  -u low"), ["notify-send", "-u", "low"]);
        assert!(words("   ").is_empty());
    }

    #[test]
    fn a_quoted_run_is_one_word_however_much_space_it_holds() {
        assert_eq!(
            words(r#""/home/a b/wrangler" hook 'a  b'"#),
            ["/home/a b/wrangler", "hook", "a  b"]
        );
    }

    #[test]
    fn a_quote_opens_a_word_even_when_it_holds_nothing() {
        assert_eq!(words("cmd '' x"), ["cmd", "", "x"]);
    }

    #[test]
    fn an_unclosed_quote_runs_to_the_end_of_the_line() {
        assert_eq!(words("cmd \"a b"), ["cmd", "a b"]);
    }
}
