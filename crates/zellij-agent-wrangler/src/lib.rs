//! The sidebar's own logic, kept apart from the plugin that runs it.
//!
//! Nothing here calls zellij: a row model is built from values, drawn to text,
//! and read back in tests without a session, a pane, or a wasm host to run in.

pub mod agents;
pub mod model;
pub mod options;
pub mod render;
pub mod selection;
pub mod session;
pub mod tree;
