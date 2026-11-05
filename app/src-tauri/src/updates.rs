//! Checks GitHub Releases for a newer version of the app.
//!
//! This detects and announces updates (banner + notification). Installing
//! is done by tauri-plugin-updater (`install_update` in main.rs) from the
//! signed bundles CI attaches to each release.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// `owner/repo` on GitHub whose releases are checked.
pub const REPO: &str = "juan-silveira/ai-usage";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
    pub notes: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// `Ok(None)` when up to date or the repo has no releases yet.
pub fn check() -> Result<Option<UpdateInfo>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build()
        .get(&url)
        .set(
            "User-Agent",
            concat!("ai-usage/", env!("CARGO_PKG_VERSION")),
        )
        .set("Accept", "application/vnd.github+json")
        .call();
    let release: Release = match response {
        Ok(r) => r.into_json().map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if release.draft || release.prerelease || !is_newer(&release.tag_name, CURRENT) {
        return Ok(None);
    }
    Ok(Some(UpdateInfo {
        version: release.tag_name.trim_start_matches('v').to_string(),
        url: release.html_url,
        notes: release.body.unwrap_or_default().chars().take(400).collect(),
    }))
}

fn parse(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .take(3)
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn is_newer(candidate: &str, current: &str) -> bool {
    parse(candidate) > parse(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }
}
