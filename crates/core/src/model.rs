use chrono::{DateTime, Utc};
use serde::Serialize;

/// One rate-limit window (e.g. the 5-hour session or the weekly cap).
#[derive(Debug, Clone, Serialize)]
pub struct UsageWindow {
    /// Stable identifier, e.g. "session", "weekly", "weekly_opus".
    pub key: String,
    /// Human label shown in the UI.
    pub label: String,
    /// Percentage of the window already consumed, 0–100.
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderStatus {
    Ok { windows: Vec<UsageWindow> },
    /// The CLI is not installed or the user never logged in.
    NoCredentials,
    /// The stored token expired; the provider's own CLI must refresh it.
    TokenExpired,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub display_name: String,
    /// Subscription tier when the provider reports it (e.g. "max", "plus").
    pub plan: Option<String>,
    pub fetched_at: DateTime<Utc>,
    #[serde(flatten)]
    pub status: ProviderStatus,
}

impl ProviderUsage {
    /// Highest usage across all windows, used for the tray icon/tooltip.
    pub fn peak_percent(&self) -> Option<f64> {
        match &self.status {
            ProviderStatus::Ok { windows } => windows
                .iter()
                .map(|w| w.used_percent)
                .fold(None, |acc, p| Some(acc.map_or(p, |a: f64| a.max(p)))),
            _ => None,
        }
    }
}
