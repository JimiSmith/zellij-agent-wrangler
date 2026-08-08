//! The command a call for the user is announced with.
//!
//! What to run is carried as the words it is run from rather than as a line to
//! be split, so the splitting happens once where the user wrote it and every
//! hand it passes through after that holds the arguments a process is actually
//! spawned with.
//!
//! The program is kept apart from its arguments, because a notifier with nothing
//! to run is not a notifier: refusing it here is what leaves nothing to check at
//! the moment a call is raised.

/// What a desktop notification is raised by. The title and the body are appended
/// to the arguments, which is the shape `notify-send` and its like take.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Notifier {
    program: String,
    arguments: Vec<String>,
}

impl Notifier {
    /// The notifier these words name, or `None` where they name nothing that
    /// could be run.
    ///
    /// A notifier that is no program is not one that fails when a call comes: it
    /// is one that was never asked for, which is the same answer as off.
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

    /// The words it was built from, for handing on unchanged.
    pub fn words(&self) -> Vec<String> {
        let mut words = vec![self.program.clone()];
        words.extend(self.arguments.iter().cloned());
        words
    }

    /// What to run.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Everything to run it with for one notification: whatever it was given,
    /// then the title and the body.
    pub fn arguments(&self, title: &str, body: &str) -> Vec<String> {
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
            notifier.arguments("claude", "the port"),
            ["--urgency", "low", "claude", "the port"]
        );
    }

    #[test]
    fn a_notifier_of_its_own_still_takes_them_last() {
        let notifier = notifier(&["/opt/my notifier"]).unwrap();
        assert_eq!(
            notifier.arguments("claude", "the port"),
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
        assert_eq!(notifier.words(), ["/opt/my notifier", "-u"]);
    }
}
