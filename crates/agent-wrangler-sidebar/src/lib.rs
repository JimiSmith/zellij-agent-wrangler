//! Multiplexer-neutral state and decisions for an agent wrangler sidebar.
//!
//! An adapter reports session facts in the vocabulary of this crate. The
//! adapter then runs the effects that this crate returns. No host API and no
//! host-owned type crosses this boundary.

pub mod application;
pub mod calls;
pub mod client;
pub mod model;
pub mod options;
pub mod session;

pub use application::Application;
pub use model::{
    AgentSnapshot, Broadcast, ClientMessage, Command, Decision, Effect, Focus, FocusTarget, Input,
    InteractionItem, PaneId, PaneReport, PaneVisibility, Permission, RenderedView, SessionLayout,
    SidebarPaneReport, TabId, TabLayout, TabReport, UserAction, ViewAction,
};
pub use options::Options;
pub use session::{ReconciledFocus, ReconciledSession};
