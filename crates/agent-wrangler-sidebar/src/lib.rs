//! Multiplexer-neutral state and decisions for an agent wrangler sidebar.
//!
//! Adapters report session facts in this crate's vocabulary and execute the
//! effects it returns. No host API or host-owned type crosses this boundary.

pub mod application;
pub mod calls;
pub mod client;
pub mod model;
pub mod options;
pub mod session;

pub use application::Application;
pub use model::{
    AgentSnapshot, Broadcast, Command, Decision, Effect, Focus, FocusTarget, Input,
    InteractionItem, PaneId, PaneReport, PaneSnapshot, Permission, RenderedView, TabId, TabPanes,
    TabReport, UserAction, ViewAction,
};
pub use options::Options;
pub use session::{ReconciledFocus, ReconciledSession};
