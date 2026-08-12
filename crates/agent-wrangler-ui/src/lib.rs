//! What a client shows, kept apart from the client showing it.
//!
//! A client here is whatever puts the agents in front of the user: the zellij
//! sidebar, or a program of its own. What differs between them is where the
//! session's shape comes from and how the finished cells reach a screen. What
//! does not differ is everything in between, which is what lives here: the rows,
//! the order they are drawn in, the glyphs and styles they are drawn with, what
//! the selection can land on, and how the pane's height is divided between the
//! tree and the calls at its foot.
//!
//! The drawing ends in a [`ratatui_core::buffer::Buffer`] rather than in a
//! string. That is the widest thing both kinds of client can take: a buffer
//! becomes escape sequences for a plugin that can only print (see [`ansi`]), and
//! goes straight to a backend for a client that owns its terminal.

/// How big the pane a frame is composed for is, re-exported so that a client can
/// say so without naming the library the cells come from.
pub use ratatui_core::layout::Rect;

pub mod ansi;
pub mod frame;
pub mod model;
pub mod options;
pub mod render;
pub mod selection;
pub mod tree;
