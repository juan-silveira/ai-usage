mod claude;
mod codex;

use std::time::Duration;

use chrono::Utc;

use crate::model::{ProviderStatus, ProviderUsage};

pub use claude::Claude;
pub use codex::Codex;

pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    /// Returns the plan name (if known) and the usage status.
    fn poll(&self) -> (Option<String>, ProviderStatus);

    fn fetch(&self) -> ProviderUsage {
        let (plan, status) = self.poll();
        ProviderUsage {
            provider: self.id().to_string(),
            display_name: self.display_name().to_string(),
            plan,
            fetched_at: Utc::now(),
            status,
        }
    }
}

pub fn all_providers() -> Vec<Box<dyn Provider>> {
    vec![Box::new(Claude), Box::new(Codex)]
}

pub(crate) fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("ia-usage/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Maps a ureq error to a provider status; 401/403 mean the token is stale.
pub(crate) fn status_from_http_error(err: ureq::Error) -> ProviderStatus {
    match err {
        ureq::Error::Status(401 | 403, _) => ProviderStatus::TokenExpired,
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let snippet: String = body.chars().take(200).collect();
            ProviderStatus::Error {
                message: format!("HTTP {code}: {snippet}"),
            }
        }
        ureq::Error::Transport(t) => ProviderStatus::Error {
            message: t.to_string(),
        },
    }
}
