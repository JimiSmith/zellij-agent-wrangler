//! The command that announces a call for the user.
//!
//! A notifier carries what to run as words, not as a line to split. The split
//! happens once, where the user wrote the line. Every later holder of the
//! notifier therefore holds the arguments that a process is spawned with.
//!
//! The program stays apart from its arguments. A notifier with no program to run
//! is not a notifier. This module refuses such a notifier, so nothing is left to
//! test at the moment when a call is raised.

/// What raises a desktop notification. The title and the body come after the
/// arguments, which is the shape that `notify-send` and similar programs take.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Notifier {
    program: String,
    arguments: Vec<String>,
}

impl Notifier {
    /// The notifier that these words name, or `None` where they name nothing to
    /// run.
    ///
    /// A notifier with no program does not fail when a call comes. Nobody asked
    /// for that notifier, which is the same answer as off.
    pub fn new(words: Vec<String>) -> Option<Self> {
        let mut words = words.into_iter();
        let program = words.next()?;
        match program.trim().is_empty() {
            true => None,
            false => Some(Notifier {
                program,
                arguments: words.collect(),
            }),
        }
    }

    /// The words that built it, to pass on unchanged.
    pub fn program_and_arguments(&self) -> Vec<String> {
        let mut words = vec![self.program.clone()];
        words.extend(self.arguments.iter().cloned());
        words
    }

    /// What to run.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Everything to run it with for one notification: the arguments it was
    /// given, then the title and the body.
    pub fn arguments_for_notification(&self, title: &str, body: &str) -> Vec<String> {
        let mut arguments = self.arguments.clone();
        arguments.push(title.to_string());
        arguments.push(body.to_string());
        arguments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notifier(words: &[&str]) -> Option<Notifier> {
        Notifier::new(words.iter().map(|word| word.to_string()).collect())
    }

    #[test]
    fn the_title_and_the_body_come_after_whatever_it_was_given() {
        let notifier = notifier(&["notify-send", "--urgency", "low"]).unwrap();
        assert_eq!(notifier.program(), "notify-send");
        assert_eq!(
            notifier.arguments_for_notification("claude", "the port"),
            ["--urgency", "low", "claude", "the port"]
        );
    }

    #[test]
    fn a_notifier_of_its_own_still_takes_them_last() {
        let notifier = notifier(&["/opt/my notifier"]).unwrap();
        assert_eq!(
            notifier.arguments_for_notification("claude", "the port"),
            ["claude", "the port"]
        );
    }

    #[test]
    fn words_that_name_no_program_name_no_notifier() {
        assert_eq!(notifier(&[]), None);
        assert_eq!(notifier(&["", "--urgency"]), None);
        assert_eq!(notifier(&["   "]), None);
    }

    #[test]
    fn the_words_come_back_as_they_went_in() {
        let notifier = notifier(&["/opt/my notifier", "-u"]).unwrap();
        assert_eq!(notifier.program_and_arguments(), ["/opt/my notifier", "-u"]);
    }
}
