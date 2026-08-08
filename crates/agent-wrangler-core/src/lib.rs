//! What an agent session is, and what it is called by.
//!
//! A session is filed under the id the agent gives it and described by what the
//! agent writes about itself. Nothing here names a pane, a tab, a window or a
//! row: these are facts about agents, not about anywhere they are shown.

pub mod agent;
pub mod command;
pub mod label;
pub mod notify;
pub mod origin;
pub mod registry;

/// Finding what a session calls itself means reading the agent's own files, so
/// the modules that do it are behind the feature that turns file reading on.
#[cfg(feature = "native")]
pub mod payload;
#[cfg(feature = "native")]
pub mod titles;
