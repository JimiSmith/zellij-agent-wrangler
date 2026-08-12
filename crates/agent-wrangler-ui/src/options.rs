//! What a client has been asked to show.
//!
//! Only the questions the drawing answers to. How a client is configured is its
//! own business: a plugin is handed strings by a layout and a program reads a
//! command line, and neither of those is a fact about what a row looks like.

pub use agent_wrangler_core::label::Label;

/// Everything the drawing can be asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub label: Label,
    /// Whether the tree is followed by a block per agent, the same sessions
    /// gathered by which agent they are rather than by where they are.
    pub sections: bool,
    /// Whether an agent's row says whose turn it is.
    pub turn_state: bool,
    /// Whether the calls for the user are listed at the foot of the pane.
    pub notifications: bool,
}

impl Default for View {
    fn default() -> Self {
        View {
            label: Label::default(),
            sections: false,
            turn_state: true,
            notifications: true,
        }
    }
}
