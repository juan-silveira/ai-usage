//! What goes in the system panel: one item per provider (logo + text, like a
//! weather applet), or a single app icon when the text is turned off.

use std::collections::BTreeMap;

use ia_usage_core::{ProviderStatus, ProviderUsage, Session, SessionState};
use serde::{Deserialize, Serialize};

pub const APP_ITEM: &str = "app";
pub const APP_ICON: &[u8] = include_bytes!("../icons/128x128.png");

/// Which usage number the panel text shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarFormat {
    /// No provider items, just the app icon.
    Off,
    /// Highest usage among the provider's windows.
    #[default]
    Peak,
    /// The 5-hour session window.
    Session,
    /// Session and weekly, e.g. "5/17%".
    SessionWeekly,
}

/// Per-provider panel options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderBar {
    #[serde(default = "yes")]
    pub show: bool,
    #[serde(default = "yes")]
    pub show_icon: bool,
    #[serde(default)]
    pub show_name: bool,
    /// Text shown when `show_name` is on; empty means the default ("CL", …).
    #[serde(default)]
    pub name: String,
}

fn yes() -> bool {
    true
}

impl Default for ProviderBar {
    fn default() -> Self {
        Self {
            show: true,
            show_icon: true,
            show_name: false,
            name: String::new(),
        }
    }
}

pub fn default_name(provider: &str) -> String {
    match provider {
        "claude" => "CL".into(),
        "codex" => "CX".into(),
        other => other.chars().take(2).collect::<String>().to_uppercase(),
    }
}

pub fn provider_icon(provider: &str) -> Option<&'static [u8]> {
    match provider {
        "claude" => Some(include_bytes!("../icons/providers/claude.png")),
        "codex" => Some(include_bytes!("../icons/providers/codex.png")),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BarItem {
    pub id: String,
    pub icon: Option<&'static [u8]>,
    pub label: String,
    pub tooltip: String,
}

pub fn items(
    usage: &[ProviderUsage],
    sessions: &[Session],
    format: BarFormat,
    providers: &BTreeMap<String, ProviderBar>,
) -> Vec<BarItem> {
    let waiting = |provider: Option<&str>| {
        sessions
            .iter()
            .filter(|s| s.state == SessionState::NeedsInput)
            .filter(|s| provider.is_none_or(|p| s.provider == p))
            .count()
    };

    let mut items = Vec::new();
    if format != BarFormat::Off {
        for p in usage {
            let ProviderStatus::Ok { windows } = &p.status else {
                continue;
            };
            let config = providers.get(&p.provider).cloned().unwrap_or_default();
            if !config.show {
                continue;
            }
            let pct = |key: &str| {
                windows
                    .iter()
                    .find(|w| w.key == key)
                    .map(|w| w.used_percent.round())
            };
            let Some(peak) = p.peak_percent().map(f64::round) else {
                continue;
            };
            let value = match format {
                BarFormat::Session => format!("{:.0}%", pct("session").unwrap_or(peak)),
                BarFormat::SessionWeekly => match (pct("session"), pct("weekly")) {
                    (Some(s), Some(w)) => format!("{s:.0}/{w:.0}%"),
                    _ => format!("{peak:.0}%"),
                },
                _ => format!("{peak:.0}%"),
            };

            let icon = provider_icon(&p.provider).filter(|_| config.show_icon);
            let mut parts = Vec::new();
            let n = waiting(Some(&p.provider));
            if n > 0 {
                parts.push(format!("⚠{n}"));
            }
            // Without a logo the number alone would be ambiguous between
            // providers unless the user hid the name on purpose.
            if config.show_name || (config.show_icon && icon.is_none()) {
                let name = config.name.trim();
                parts.push(if name.is_empty() {
                    default_name(&p.provider)
                } else {
                    name.to_string()
                });
            }
            parts.push(value);

            items.push(BarItem {
                id: p.provider.clone(),
                icon,
                label: parts.join(" "),
                tooltip: provider_tooltip(p),
            });
        }
    }

    if items.is_empty() {
        let n = waiting(None);
        items.push(BarItem {
            id: APP_ITEM.into(),
            icon: Some(APP_ICON),
            label: if n > 0 {
                format!("⚠{n}")
            } else {
                String::new()
            },
            tooltip: summary_tooltip(usage, n),
        });
    }
    items
}

fn provider_tooltip(p: &ProviderUsage) -> String {
    let mut lines = vec![p.display_name.clone()];
    if let ProviderStatus::Ok { windows } = &p.status {
        lines.extend(
            windows
                .iter()
                .map(|w| format!("{}: {:.0}%", w.label, w.used_percent)),
        );
    }
    lines.join("\n")
}

fn summary_tooltip(usage: &[ProviderUsage], waiting: usize) -> String {
    let mut lines = vec!["IA Usage".to_string()];
    lines.extend(usage.iter().map(|p| match p.peak_percent() {
        Some(pct) => format!("{}: {pct:.0}%", p.display_name),
        None => format!("{}: —", p.display_name),
    }));
    if waiting > 0 {
        lines.push(format!("{waiting} sessão(ões) esperando você"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ia_usage_core::UsageWindow;

    fn usage(provider: &str, session: f64, weekly: f64) -> ProviderUsage {
        let w = |key: &str, used| UsageWindow {
            key: key.into(),
            label: key.into(),
            used_percent: used,
            resets_at: None,
        };
        ProviderUsage {
            provider: provider.into(),
            display_name: provider.into(),
            plan: None,
            fetched_at: Utc::now(),
            status: ProviderStatus::Ok {
                windows: vec![w("session", session), w("weekly", weekly)],
            },
        }
    }

    fn waiting_session(provider: &str) -> Session {
        Session {
            provider: provider.into(),
            session_id: "s".into(),
            cwd: "/p".into(),
            project: "p".into(),
            state: SessionState::NeedsInput,
            detail: None,
            model: None,
            turn_started_at: None,
            last_turn_secs: None,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn default_is_icon_and_number_per_provider() {
        let items = items(
            &[usage("claude", 5.0, 17.0), usage("codex", 40.0, 2.0)],
            &[],
            BarFormat::Peak,
            &BTreeMap::new(),
        );
        let labels: Vec<_> = items
            .iter()
            .map(|i| (i.id.as_str(), i.label.as_str()))
            .collect();
        assert_eq!(labels, [("claude", "17%"), ("codex", "40%")]);
        assert!(items.iter().all(|i| i.icon.is_some()));
    }

    #[test]
    fn custom_name_no_icon_and_waiting_badge() {
        let mut cfg = BTreeMap::new();
        cfg.insert(
            "claude".to_string(),
            ProviderBar {
                show: true,
                show_icon: false,
                show_name: true,
                name: "Claudinho".into(),
            },
        );
        cfg.insert(
            "codex".to_string(),
            ProviderBar {
                show: false,
                ..Default::default()
            },
        );
        let items = items(
            &[usage("claude", 5.0, 17.0), usage("codex", 40.0, 2.0)],
            &[waiting_session("claude")],
            BarFormat::SessionWeekly,
            &cfg,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "⚠1 Claudinho 5/17%");
        assert!(items[0].icon.is_none());
    }

    #[test]
    fn falls_back_to_app_icon() {
        let items = items(
            &[usage("claude", 5.0, 17.0)],
            &[waiting_session("codex")],
            BarFormat::Off,
            &BTreeMap::new(),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, APP_ITEM);
        assert_eq!(items[0].label, "⚠1");
    }
}
