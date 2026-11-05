//! Fetches subscription usage limits for AI coding tools (Claude Code, Codex, …)
//! by reusing the credentials their CLIs already store on disk, and tracks
//! live agent sessions through the agents' hooks.
//!
//! Tokens are only ever read, never refreshed or rewritten: the CLIs rotate
//! refresh tokens, so refreshing here could log the user out of the CLI.

pub mod hook_install;
pub mod model;
pub mod poller;
pub mod providers;
pub mod sessions;

pub use model::{ProviderStatus, ProviderUsage, Stale, UsageWindow};
pub use poller::Poller;
pub use providers::{all_providers, Provider};
pub use sessions::{list_sessions, Session, SessionState};

/// Polls every registered provider sequentially.
pub fn fetch_all() -> Vec<ProviderUsage> {
    all_providers().iter().map(|p| p.fetch()).collect()
}
