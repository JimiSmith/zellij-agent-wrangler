//! The sidebar's own logic, kept apart from the plugin that runs it.
//!
//! Nothing here calls zellij: a row model is built from values, drawn to text,
//! and read back in tests without a session, a pane, or a wasm host to run in.

pub mod agents;
pub mod command;
pub mod model;
pub mod options;
pub mod render;
pub mod selection;
pub mod tree;

/// The only module that depends on `zellij-tile`, and so the only one behind
/// the feature that pulls that crate in.
#[cfg(feature = "plugin")]
pub mod session;

#[cfg(feature = "native")]
pub mod install;
#[cfg(feature = "native")]
pub mod payload;
#[cfg(feature = "native")]
pub mod titles;
