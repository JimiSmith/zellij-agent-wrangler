//! What a client is asked to show.
//!
//! Only the questions the drawing answers. The configuration of a client is the
//! business of that client. A layout hands strings to a plugin, and a program
//! reads a command line. Neither of those is a fact about the look of a row.

pub use agent_wrangler_core::label::Label;
pub use agent_wrangler_core::status_line::StatusTemplate;

/// Everything that the drawing can be asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawingOptions {
    pub label: Label,
    /// Whether a block per agent follows the tree. A block holds the same
    /// sessions, gathered by the agent they are rather than by the place they
    /// are in.
    pub sections: bool,
    /// Whether an agent's row says whose turn it is.
    pub turn_state: bool,
    /// Whether the calls for the user are listed at the foot of the pane.
    pub notifications: bool,
    /// The line that hangs under an agent's row, spelled from the record.
    /// `None` draws no such line, and `None` is the default.
    pub status_line: Option<StatusTemplate>,
}

impl Default for DrawingOptions {
    fn default() -> Self {
        DrawingOptions {
            label: Label::default(),
            sections: false,
            turn_state: true,
            notifications: true,
            status_line: None,
        }
    }
}
