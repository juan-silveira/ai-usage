//! Cursor usage for the current billing cycle, via the endpoint behind the
//! usage page of cursor.com's dashboard.
//!
//! The access token is read (read-only) from the Cursor app's own storage,
//! `User/globalStorage/state.vscdb`, a SQLite database.

use std::path::PathBuf;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{http_agent, status_from_http_error, Provider};
use crate::model::{ProviderStatus, UsageWindow};

const USAGE_URL: &str = "https://cursor.com/api/usage-summary";

pub struct Cursor;

impl Provider for Cursor {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn display_name(&self) -> &'static str {
        "Cursor"
    }

    fn poll(&self) -> (Option<String>, ProviderStatus) {
        let Some(token) = load_token() else {
            return (None, ProviderStatus::NoCredentials);
        };
        let Some(claims) = jwt_claims(&token) else {
            return (None, ProviderStatus::TokenExpired);
        };
        if claims
            .get("exp")
            .and_then(Value::as_i64)
            .is_some_and(|exp| exp <= Utc::now().timestamp())
        {
            return (None, ProviderStatus::TokenExpired);
        }
        // The dashboard's session cookie is "<user id>::<access token>".
        let Some(user) = claims
            .get("sub")
            .and_then(Value::as_str)
            .and_then(|s| s.rsplit('|').next())
        else {
            return (None, ProviderStatus::TokenExpired);
        };

        let response = http_agent()
            .get(USAGE_URL)
            .set(
                "Cookie",
                &format!("WorkosCursorSessionToken={user}%3A%3A{token}"),
            )
            .call();
        match response {
            Ok(resp) => match resp.into_json::<Value>() {
                Ok(body) => (
                    body.get("membershipType")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    parse_usage(&body),
                ),
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

fn state_db() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        dirs::home_dir()?.join("Library/Application Support")
    } else {
        // %APPDATA% on Windows, ~/.config on Linux.
        dirs::config_dir()?
    };
    Some(base.join("Cursor/User/globalStorage/state.vscdb"))
}

fn load_token() -> Option<String> {
    let path = state_db().filter(|p| p.exists())?;
    let db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let token: String = db
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'",
            [],
            |row| row.get(0),
        )
        .ok()?;
    let token = token.trim().trim_matches('"').to_string();
    (!token.is_empty()).then_some(token)
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn parse_usage(body: &Value) -> ProviderStatus {
    let date = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
    };
    let (start, end) = (date("billingCycleStart"), date("billingCycleEnd"));
    let days = start
        .zip(end)
        .map(|(s, e)| ((e - s).num_hours() as f64 / 24.0).round() as u32);
    let Some(individual) = body.get("individualUsage") else {
        return ProviderStatus::Error {
            message: "no individualUsage in response".into(),
        };
    };

    let mut windows = Vec::new();
    let mut push = |key: &str, used: f64| {
        let mut window = UsageWindow {
            key: key.into(),
            label: String::new(),
            days,
            used_percent: used.clamp(0.0, 100.0),
            resets_at: end,
        };
        window.label = crate::i18n::window_label(&window);
        windows.push(window);
    };
    // The plan's included usage is the billing-cycle window ("weekly" key:
    // the long window, labelled by its length).
    if body.get("isUnlimited").and_then(Value::as_bool) != Some(true) {
        if let Some(pct) = individual
            .pointer("/plan/totalPercentUsed")
            .and_then(Value::as_f64)
        {
            push("weekly", pct);
        }
    }
    // Pay-as-you-go spending against its cap, when turned on with one.
    let on_demand = individual.get("onDemand");
    let enabled = on_demand
        .and_then(|o| o.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true);
    let used = on_demand
        .and_then(|o| o.get("used"))
        .and_then(Value::as_f64);
    let limit = on_demand
        .and_then(|o| o.get("limit"))
        .and_then(Value::as_f64);
    if let (true, Some(used), Some(limit)) = (enabled, used, limit) {
        if limit > 0.0 {
            push("monthly_on_demand", used / limit * 100.0);
        }
    }
    ProviderStatus::Ok { windows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_and_on_demand_windows() {
        let body = json!({
            "billingCycleStart": "2026-10-15T21:59:21.495Z",
            "billingCycleEnd": "2026-11-15T21:59:21.495Z",
            "membershipType": "pro",
            "isUnlimited": false,
            "individualUsage": {
                "plan": {"totalPercentUsed": 42.5},
                "onDemand": {"enabled": true, "used": 500, "limit": 2000}
            }
        });
        let ProviderStatus::Ok { windows } = parse_usage(&body) else {
            panic!("expected Ok");
        };
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].key, "weekly");
        assert_eq!(windows[0].days, Some(31));
        assert_eq!(windows[0].used_percent, 42.5);
        assert_eq!(windows[1].key, "monthly_on_demand");
        assert_eq!(windows[1].used_percent, 25.0);
    }

    #[test]
    fn on_demand_without_cap_is_skipped() {
        let body = json!({
            "individualUsage": {
                "plan": {"totalPercentUsed": 0},
                "onDemand": {"enabled": true, "used": 3, "limit": null}
            }
        });
        let ProviderStatus::Ok { windows } = parse_usage(&body) else {
            panic!("expected Ok");
        };
        assert_eq!(windows.len(), 1);
    }
}
