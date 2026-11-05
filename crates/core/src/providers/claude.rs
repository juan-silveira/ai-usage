//! Claude Code subscription usage via the OAuth usage endpoint (the same data
//! `/usage` shows inside Claude Code).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{http_agent, status_from_http_error, Provider};
use crate::model::{ProviderStatus, UsageWindow};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";

pub struct Claude;

struct Credentials {
    access_token: String,
    expires_at_ms: Option<i64>,
    subscription: Option<String>,
}

impl Provider for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude"
    }

    fn poll(&self) -> (Option<String>, ProviderStatus) {
        let Some(creds) = load_credentials() else {
            return (None, ProviderStatus::NoCredentials);
        };
        let plan = creds.subscription.clone();
        if creds
            .expires_at_ms
            .is_some_and(|exp| exp <= Utc::now().timestamp_millis())
        {
            return (plan, ProviderStatus::TokenExpired);
        }

        let response = http_agent()
            .get(USAGE_URL)
            .set("Authorization", &format!("Bearer {}", creds.access_token))
            .set("anthropic-beta", OAUTH_BETA)
            .call();
        let status = match response {
            Ok(resp) => match resp.into_json::<Value>() {
                Ok(body) => parse_usage(&body),
                Err(e) => ProviderStatus::Error {
                    message: format!("invalid JSON: {e}"),
                },
            },
            Err(e) => status_from_http_error(e),
        };
        (plan, status)
    }
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
}

fn load_credentials() -> Option<Credentials> {
    let from_file = config_dir()
        .map(|d| d.join(".credentials.json"))
        .and_then(|p| std::fs::read_to_string(p).ok());
    let raw = from_file.or_else(read_keychain)?;
    parse_credentials(&raw)
}

/// On macOS Claude Code keeps its credentials in the login Keychain.
#[cfg(target_os = "macos")]
fn read_keychain() -> Option<String> {
    let out = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(target_os = "macos"))]
fn read_keychain() -> Option<String> {
    None
}

fn parse_credentials(raw: &str) -> Option<Credentials> {
    let json: Value = serde_json::from_str(raw).ok()?;
    let oauth = json.get("claudeAiOauth")?;
    let access_token = oauth.get("accessToken")?.as_str()?.trim().to_string();
    if access_token.is_empty() {
        return None;
    }
    Some(Credentials {
        access_token,
        expires_at_ms: oauth.get("expiresAt").and_then(Value::as_i64),
        subscription: oauth
            .get("subscriptionType")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The response is an object of buckets like
/// `{"five_hour": {"utilization": 12.0, "resets_at": "…"}, "seven_day": …}`.
/// Every bucket with a numeric `utilization` becomes a window, so new
/// per-model buckets show up without code changes.
fn parse_usage(body: &Value) -> ProviderStatus {
    let Some(obj) = body.as_object() else {
        return ProviderStatus::Error {
            message: "unexpected response shape".into(),
        };
    };

    let mut windows: Vec<UsageWindow> = obj
        .iter()
        .filter_map(|(key, bucket)| {
            let used = bucket.get("utilization")?.as_f64()?;
            let resets_at = bucket
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));
            let (key, label) = window_name(key);
            Some(UsageWindow {
                key,
                label,
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
    windows.sort_by_key(|w| window_order(&w.key));
    ProviderStatus::Ok { windows }
}

fn window_name(raw: &str) -> (String, String) {
    match raw {
        "five_hour" => ("session".into(), "Sessão (5h)".into()),
        "seven_day" => ("weekly".into(), "Semanal".into()),
        other => {
            let suffix = other.strip_prefix("seven_day_").unwrap_or(other);
            let mut label = suffix.replace('_', " ");
            if let Some(first) = label.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            (format!("weekly_{suffix}"), format!("Semanal · {label}"))
        }
    }
}

fn window_order(key: &str) -> u8 {
    match key {
        "session" => 0,
        "weekly" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_known_and_extra_buckets() {
        let body = json!({
            "five_hour": {"utilization": 42.0, "resets_at": "2030-01-01T05:00:00Z"},
            "seven_day": {"utilization": 10, "resets_at": null},
            "seven_day_opus": {"utilization": 3.5},
            "seven_day_sonnet": null,
            "extra_usage": {"is_enabled": false}
        });
        let ProviderStatus::Ok { windows } = parse_usage(&body) else {
            panic!("expected Ok");
        };
        let keys: Vec<_> = windows.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, ["session", "weekly", "weekly_opus"]);
        assert_eq!(windows[0].used_percent, 42.0);
        assert!(windows[0].resets_at.is_some());
        assert_eq!(windows[2].label, "Semanal · Opus");
    }

    #[test]
    fn empty_response_is_error() {
        assert!(matches!(
            parse_usage(&json!({})),
            ProviderStatus::Error { .. }
        ));
    }

    #[test]
    fn reads_credentials_file_format() {
        let raw =
            r#"{"claudeAiOauth":{"accessToken":"tok","expiresAt":123,"subscriptionType":"max"}}"#;
        let c = parse_credentials(raw).unwrap();
        assert_eq!(c.access_token, "tok");
        assert_eq!(c.expires_at_ms, Some(123));
        assert_eq!(c.subscription.as_deref(), Some("max"));
    }
}
