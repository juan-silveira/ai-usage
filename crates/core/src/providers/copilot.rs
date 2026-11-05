//! GitHub Copilot monthly quotas (chat, completions, premium requests) via
//! the endpoint the Copilot editor plugins use for their quota display.
//!
//! Token, first found: `$GH_TOKEN`, the Copilot plugins' own sign-in
//! (`~/.config/github-copilot/apps.json` or `hosts.json`), then `gh auth token`.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{http_agent, status_from_http_error, Provider};
use crate::cli_locate;
use crate::model::{ProviderStatus, UsageWindow};

const USER_URL: &str = "https://api.github.com/copilot_internal/user";

pub struct Copilot;

impl Provider for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn display_name(&self) -> &'static str {
        "Copilot"
    }

    fn poll(&self) -> (Option<String>, ProviderStatus) {
        let Some(token) = load_token() else {
            return (None, ProviderStatus::NoCredentials);
        };
        let response = http_agent()
            .get(USER_URL)
            .set("Authorization", &format!("token {token}"))
            .set("Accept", "application/json")
            .call();
        match response {
            Ok(resp) => match resp.into_json::<Value>() {
                Ok(body) => (plan(&body), parse_usage(&body)),
                Err(e) => (
                    None,
                    ProviderStatus::Error {
                        message: format!("invalid JSON: {e}"),
                    },
                ),
            },
            // Signed in to GitHub but without Copilot.
            Err(ureq::Error::Status(404, _)) => (None, ProviderStatus::NoCredentials),
            Err(e) => (None, status_from_http_error(e)),
        }
    }
}

fn load_token() -> Option<String> {
    let env = ["GH_TOKEN", "GITHUB_COPILOT_TOKEN"]
        .into_iter()
        .find_map(|v| std::env::var(v).ok().filter(|t| !t.trim().is_empty()));
    env.or_else(plugin_token).or_else(gh_token)
}

/// The editor plugins store `{"github.com:<app id>": {"oauth_token": …}}`.
fn plugin_token() -> Option<String> {
    let dir = copilot_config_dir()?;
    ["apps.json", "hosts.json"].into_iter().find_map(|file| {
        let raw = std::fs::read_to_string(dir.join(file)).ok()?;
        let json: Value = serde_json::from_str(&raw).ok()?;
        json.as_object()?
            .iter()
            .filter(|(host, _)| host.starts_with("github.com"))
            .find_map(|(_, v)| v.get("oauth_token")?.as_str().map(str::to_string))
    })
}

fn copilot_config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join("github-copilot"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .map(|d| d.join("github-copilot"))
    }
}

fn gh_token() -> Option<String> {
    let gh = cli_locate::find("gh")?;
    let out = Command::new(&gh)
        .args(["auth", "token"])
        .env("PATH", cli_locate::child_path(&gh))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !token.is_empty()).then_some(token)
}

fn plan(body: &Value) -> Option<String> {
    let sku = body.get("access_type_sku").and_then(Value::as_str);
    if sku.is_some_and(|s| s.contains("free")) {
        return Some("free".into());
    }
    body.get("copilot_plan")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn parse_usage(body: &Value) -> ProviderStatus {
    let resets_at = body
        .get("quota_reset_date_utc")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let Some(snapshots) = body.get("quota_snapshots").and_then(Value::as_object) else {
        return ProviderStatus::Error {
            message: "no quota_snapshots in response".into(),
        };
    };
    // Unlimited quotas and ones the plan doesn't include (entitlement 0)
    // say nothing useful.
    let windows: Vec<UsageWindow> = [
        ("premium_interactions", "premium"),
        ("chat", "chat"),
        ("completions", "completions"),
    ]
    .into_iter()
    .filter_map(|(id, scope)| {
        let q = snapshots.get(id)?;
        if q.get("unlimited").and_then(Value::as_bool) == Some(true)
            || q.get("entitlement").and_then(Value::as_f64).unwrap_or(0.0) <= 0.0
        {
            return None;
        }
        let remaining = q.get("percent_remaining")?.as_f64()?;
        let mut window = UsageWindow {
            key: format!("monthly_{scope}"),
            label: String::new(),
            days: Some(30),
            used_percent: (100.0 - remaining).clamp(0.0, 100.0),
            resets_at,
        };
        window.label = crate::i18n::window_label(&window);
        Some(window)
    })
    .collect();
    // Empty when everything is unlimited: nothing to watch, not an error.
    ProviderStatus::Ok { windows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn free_plan_quotas() {
        let body = json!({
            "access_type_sku": "free_limited_copilot",
            "copilot_plan": "individual",
            "quota_reset_date_utc": "2026-12-01T00:00:00.000Z",
            "quota_snapshots": {
                "chat": {"percent_remaining": 75.0, "unlimited": false, "entitlement": 50},
                "completions": {"percent_remaining": 100.0, "unlimited": false, "entitlement": 2000},
                "premium_interactions": {"percent_remaining": 0.0, "unlimited": false, "entitlement": 0}
            }
        });
        assert_eq!(plan(&body).as_deref(), Some("free"));
        let ProviderStatus::Ok { windows } = parse_usage(&body) else {
            panic!("expected Ok");
        };
        let keys: Vec<_> = windows.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, ["monthly_chat", "monthly_completions"]);
        assert_eq!(windows[0].used_percent, 25.0);
        assert!(windows[0].resets_at.is_some());
    }

    #[test]
    fn unlimited_quotas_are_skipped() {
        let body = json!({"quota_snapshots": {
            "chat": {"percent_remaining": 100.0, "unlimited": true, "entitlement": 0}
        }});
        assert!(matches!(parse_usage(&body), ProviderStatus::Ok { windows } if windows.is_empty()));
    }
}
