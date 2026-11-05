//! OpenAI Codex (ChatGPT plan) usage via the endpoint the Codex CLI uses for
//! its own rate-limit display.

use std::path::PathBuf;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{http_agent, status_from_http_error, Provider};
use crate::model::{ProviderStatus, UsageWindow};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// Windows at least this long are weekly allowances. Codex has sent the weekly
/// window in `primary_window` when the 5-hour one is disabled, so we classify
/// by length rather than by slot.
const WEEKLY_THRESHOLD_SECS: i64 = 86_400;

pub struct Codex;

struct Credentials {
    access_token: String,
    account_id: Option<String>,
}

impl Provider for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn poll(&self) -> (Option<String>, ProviderStatus) {
        let Some(creds) = load_credentials() else {
            return (None, ProviderStatus::NoCredentials);
        };
        if jwt_expiry(&creds.access_token).is_some_and(|exp| exp <= Utc::now().timestamp()) {
            return (None, ProviderStatus::TokenExpired);
        }

        let mut req = http_agent()
            .get(USAGE_URL)
            .set("Authorization", &format!("Bearer {}", creds.access_token))
            .set("User-Agent", "codex-cli");
        if let Some(account) = &creds.account_id {
            req = req.set("ChatGPT-Account-Id", account);
        }

        match req.call() {
            Ok(resp) => match resp.into_json::<Value>() {
                Ok(body) => {
                    let plan = body
                        .get("plan_type")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    (plan, parse_usage(&body))
                }
                Err(e) => (
                    None,
                    ProviderStatus::Error {
                        message: format!("invalid JSON: {e}"),
                    },
                ),
            },
            Err(e) => (None, status_from_http_error(e)),
        }
    }
}

fn auth_path() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
        .map(|d| d.join("auth.json"))
}

fn load_credentials() -> Option<Credentials> {
    let raw = std::fs::read_to_string(auth_path()?).ok()?;
    parse_credentials(&raw)
}

fn parse_credentials(raw: &str) -> Option<Credentials> {
    let json: Value = serde_json::from_str(raw).ok()?;
    let tokens = json.get("tokens")?;
    let access_token = tokens.get("access_token")?.as_str()?.trim().to_string();
    if access_token.is_empty() {
        return None;
    }
    Some(Credentials {
        access_token,
        account_id: tokens
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// Reads the `exp` claim (unix seconds) from the access token, a JWT. The
/// signature is not checked; this only avoids a request that would 401.
fn jwt_expiry(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()?.get("exp")?.as_i64()
}

fn parse_usage(body: &Value) -> ProviderStatus {
    let Some(rate_limit) = body.get("rate_limit").filter(|v| !v.is_null()) else {
        return ProviderStatus::Error {
            message: "no rate_limit in response".into(),
        };
    };

    let mut windows: Vec<UsageWindow> = [("primary_window", false), ("secondary_window", true)]
        .into_iter()
        .filter_map(|(slot, default_weekly)| {
            let w = rate_limit.get(slot).filter(|v| !v.is_null())?;
            let used = w.get("used_percent")?.as_f64()?;
            let weekly = w
                .get("limit_window_seconds")
                .and_then(Value::as_i64)
                .map_or(default_weekly, |secs| secs >= WEEKLY_THRESHOLD_SECS);
            let resets_at = w
                .get("reset_at")
                .and_then(Value::as_i64)
                .and_then(|ts| DateTime::from_timestamp(ts, 0));
            let (key, label) = if weekly {
                ("weekly", "Semanal")
            } else {
                ("session", "Sessão (5h)")
            };
            Some(UsageWindow {
                key: key.into(),
                label: label.into(),
                used_percent: used.clamp(0.0, 100.0),
                resets_at,
            })
        })
        .collect();

    if windows.is_empty() {
        return ProviderStatus::Error {
            message: "no usage windows in response".into(),
        };
    }
    windows.sort_by_key(|w| w.key != "session");
    ProviderStatus::Ok { windows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_windows_by_length() {
        let body = json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {"used_percent": 77, "limit_window_seconds": 604800, "reset_at": 1900000000},
                "secondary_window": null
            }
        });
        let ProviderStatus::Ok { windows } = parse_usage(&body) else {
            panic!("expected Ok");
        };
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].key, "weekly");
        assert_eq!(windows[0].used_percent, 77.0);
    }

    #[test]
    fn session_sorted_first() {
        let body = json!({"rate_limit": {
            "primary_window": {"used_percent": 5, "limit_window_seconds": 18000, "reset_at": 1900000000},
            "secondary_window": {"used_percent": 20, "limit_window_seconds": 604800, "reset_at": 1900500000}
        }});
        let ProviderStatus::Ok { windows } = parse_usage(&body) else {
            panic!("expected Ok");
        };
        assert_eq!(windows[0].key, "session");
        assert_eq!(windows[1].key, "weekly");
    }

    #[test]
    fn reads_jwt_expiry() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"exp":1749476472}"#);
        assert_eq!(jwt_expiry(&format!("h.{payload}.sig")), Some(1749476472));
        assert_eq!(jwt_expiry("not-a-jwt"), None);
    }

    #[test]
    fn reads_auth_file_format() {
        let raw = r#"{"auth_mode":"chatgpt","tokens":{"access_token":"a","account_id":"acc"}}"#;
        let c = parse_credentials(raw).unwrap();
        assert_eq!(c.access_token, "a");
        assert_eq!(c.account_id.as_deref(), Some("acc"));
    }
}
