//! The sidebar's own logic, kept apart from the plugin that runs it.
//!
//! Nothing here calls zellij: what a session looks like is read out of values,
//! and read back in tests without a session, a pane, or a wasm host to run in.
//!
//! What each module holds is the part of the sidebar that is about zellij in
//! particular: the shape of the session as zellij reports it, which pane an
//! agent named as its own, and the client every message to the daemon is run
//! through.

pub mod agents;
pub mod calls;
pub mod client;
pub mod options;
pub mod session;
