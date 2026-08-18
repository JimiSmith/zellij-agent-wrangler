//! What a client shows, kept apart from the client that shows it.
//!
//! A client here is whatever puts the agents in front of the user: the zellij
//! sidebar, or a program of its own. Two clients differ in where the shape of
//! the session comes from, and in how the finished cells reach a screen. Two
//! clients do not differ in anything between those two points. That part lives
//! here:
//!
//! - the rows,
//! - the order the rows are drawn in,
//! - the glyphs and the styles the rows are drawn with,
//! - what the selection can land on,
//! - how the height of the pane is divided between the tree and the calls at
//!   its foot.
//!
//! The drawing ends in a [`ratatui_core::buffer::Buffer`] rather than in a
//! string. A buffer is the widest thing both kinds of client can take. A buffer
//! becomes escape sequences for a plugin that can only print (see [`ansi`]). A
//! buffer goes straight to a backend for a client that owns its terminal.

/// The size of the pane a frame is composed for. The re-export lets a client
/// give that size without a name for the library the cells come from.
pub use ratatui_core::layout::Rect;

pub mod ansi;
pub mod frame;
pub mod model;
pub mod options;
pub mod render;
pub mod selection;
pub mod tree;
