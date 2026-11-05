//! OpenRouter credits, via its documented API. OpenRouter has no CLI sign-in
//! to reuse, so it needs an API key: `$OPENROUTER_API_KEY`, or one saved
//! from the app (see [`save_key`]).
//!
//! The key's own spending limit is shown when it has one; otherwise the
//! account's credits used so far.

use std::path::PathBuf;

use serde_json::Value;

use super::{http_agent, status_from_http_error, Provider};
use crate::model::{ProviderStatus, UsageWindow};

const KEY_URL: &str = "https://openrouter.ai/api/v1/key";
const CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";

pub struct OpenRouter;

impl Provider for OpenRouter {
    fn id(&self) -> &'static str {
        "openrouter"
    }

    fn display_name(&self) -> &'static str {
        "OpenRouter"
    }

    fn poll(&self) -> (Option<String>, ProviderStatus) {
        let Some(key) = load_key() else {
            return (None, ProviderStatus::NoCredentials);
        };
        let get = |url: &str| -> Result<Value, ProviderStatus> {
            http_agent()
                .get(url)
                .set("Authorization", &format!("Bearer {key}"))
                .call()
                .map_err(status_from_http_error)?
                .into_json::<Value>()
                .map_err(|e| ProviderStatus::Error {
                    message: format!("invalid JSON: {e}"),
                })
        };
        let info = match get(KEY_URL) {
            Ok(v) => v,
            Err(status) => return (None, status),
        };
        let plan = info
            .pointer("/data/is_free_tier")
            .and_then(Value::as_bool)
            .map(|free| if free { "free" } else { "paid" }.to_string());
        // A key without its own limit falls back to the account's credits,
        // which need a second request.
        let credits = if limit_of(&info).is_none() {
            get(CREDITS_URL).ok()
        } else {
            None
        };
        (plan, parse_usage(&info, credits.as_ref()))
    }
}

/// Where a key saved from the app is kept (readable by the user only).
pub fn key_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("ai-usage").join("openrouter.key"))
}

/// Saves (or, with an empty key, removes) the key used by this provider.
pub fn save_key(key: &str) -> std::io::Result<()> {
    let path = key_path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    let key = key.trim();
    if key.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Whether a key is available, and whether it comes from the environment
/// (which the app can't change).
pub fn key_source() -> Option<&'static str> {
    if env_key().is_some() {
        Some("env")
    } else if file_key().is_some() {
        Some("file")
    } else {
        None
    }
}

fn env_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

fn file_key() -> Option<String> {
    std::fs::read_to_string(key_path()?)
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

fn load_key() -> Option<String> {
    env_key().or_else(file_key)
}

fn limit_of(info: &Value) -> Option<f64> {
    info.pointer("/data/limit")
        .and_then(Value::as_f64)
        .filter(|l| *l > 0.0)
}

fn parse_usage(info: &Value, credits: Option<&Value>) -> ProviderStatus {
    let (key, used) = if let Some(limit) = limit_of(info) {
        let usage = info
            .pointer("/data/usage")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        ("key_limit", usage / limit * 100.0)
    } else {
        let total = credits
            .and_then(|c| c.pointer("/data/total_credits"))
            .and_then(Value::as_f64);
        let spent = credits
            .and_then(|c| c.pointer("/data/total_usage"))
            .and_then(Value::as_f64);
        match (total, spent) {
            (Some(total), Some(spent)) if total > 0.0 => ("credits", spent / total * 100.0),
            // Free tier or no credits bought: nothing to measure against.
            _ => return ProviderStatus::Ok { windows: vec![] },
        }
    };
    let mut window = UsageWindow {
        key: key.into(),
        label: String::new(),
        days: None,
        used_percent: used.clamp(0.0, 100.0),
        resets_at: None,
    };
    window.label = crate::i18n::window_label(&window);
    ProviderStatus::Ok {
        windows: vec![window],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_limit_wins_over_credits() {
        let info = json!({"data": {"limit": 20.0, "usage": 5.0, "is_free_tier": false}});
        let ProviderStatus::Ok { windows } = parse_usage(&info, None) else {
            panic!("expected Ok");
        };
        assert_eq!(windows[0].key, "key_limit");
        assert_eq!(windows[0].used_percent, 25.0);
    }

    #[test]
    fn account_credits_without_key_limit() {
        let info = json!({"data": {"limit": null, "usage": 5.0}});
        let credits = json!({"data": {"total_credits": 10.0, "total_usage": 7.5}});
        let ProviderStatus::Ok { windows } = parse_usage(&info, Some(&credits)) else {
            panic!("expected Ok");
        };
        assert_eq!(windows[0].key, "credits");
        assert_eq!(windows[0].used_percent, 75.0);
    }
}
