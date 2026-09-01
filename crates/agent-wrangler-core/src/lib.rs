//! What an agent session is, and what it is called by.
//!
//! A session is stored under the id that the agent gives it. A session is
//! described by what the agent writes about itself. Nothing here names a pane, a
//! tab, a window or a row. These are facts about agents, not facts about the
//! places that show them.

pub mod agent;
pub mod client_message;
pub mod command;
pub mod label;
pub mod notify;
pub mod origin;
pub mod registry;
pub mod status_line;

/// A client reads the records that the daemon handed it, so this module is
/// behind `json`, the feature that turns the parser on. It opens no file.
#[cfg(feature = "json")]
pub mod preview;

/// What a session calls itself comes from the agent's own files. The modules
/// that read those files are behind `native`, the feature that turns file
/// reading on. A client takes `json` instead. The daemon hands a client its
/// records, and the client reads them. It goes looking for no file.
#[cfg(feature = "native")]
pub mod payload;
#[cfg(feature = "native")]
pub mod titles;
