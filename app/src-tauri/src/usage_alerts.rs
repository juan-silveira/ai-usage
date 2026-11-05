//! Alerts when a usage window crosses one of the user's levels, set
//! separately for the 5-hour session and for weekly windows. Each threshold fires once per window period: the
//! window's reset time is part of the key, so a new period starts clean.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ia_usage_core::{ProviderStatus, ProviderUsage};

pub struct UsageAlert {
    pub title: String,
    pub body: String,
}

#[derive(Default)]
pub struct ThresholdTracker {
    /// (provider, window key, resets_at) -> highest threshold already alerted.
    fired: HashMap<(String, String, Option<DateTime<Utc>>), u8>,
}

impl ThresholdTracker {
    pub fn update(
        &mut self,
        usage: &[ProviderUsage],
        session_levels: &[u8],
        weekly_levels: &[u8],
    ) -> Vec<UsageAlert> {
        let mut alerts = Vec::new();
        let mut seen = HashMap::new();
        for p in usage {
            let ProviderStatus::Ok { windows } = &p.status else {
                continue;
            };
            for w in windows {
                let key = (p.provider.clone(), w.key.clone(), w.resets_at);
                let already = self.fired.get(&key).copied().unwrap_or(0);
                let thresholds = if w.key == "session" {
                    session_levels
                } else {
                    weekly_levels
                };
                // Only the highest newly crossed threshold, not one per step.
                let crossed = thresholds
                    .iter()
                    .copied()
                    .filter(|&t| t > already && w.used_percent >= f64::from(t))
                    .max();
                let level = match crossed {
                    Some(t) => {
                        let reset = w
                            .resets_at
                            .map(|r| format!(" — reinicia {}", crate::bar::local_day_time(r)))
                            .unwrap_or_default();
                        alerts.push(UsageAlert {
                            title: format!("{}: {} passou de {t}%", p.display_name, w.label),
                            body: format!("Uso atual {:.0}%{reset}", w.used_percent),
                        });
                        t
                    }
                    None => already,
                };
                seen.insert(key, level);
            }
        }
        // Drop finished periods so the map doesn't grow forever.
        self.fired = seen;
        alerts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ia_usage_core::UsageWindow;

    fn usage(pct: f64, reset_secs: i64) -> Vec<ProviderUsage> {
        vec![ProviderUsage {
            provider: "claude".into(),
            display_name: "Claude".into(),
            plan: None,
            fetched_at: Utc::now(),
            status: ProviderStatus::Ok {
                windows: vec![UsageWindow {
                    key: "session".into(),
                    label: "Sessão (5h)".into(),
                    used_percent: pct,
                    resets_at: DateTime::from_timestamp(reset_secs, 0),
                }],
            },
            stale: None,
        }]
    }

    #[test]
    fn fires_once_per_threshold_and_resets_with_window() {
        let mut t = ThresholdTracker::default();
        let th = [80, 95];
        assert!(t.update(&usage(50.0, 1000), &th, &[]).is_empty());
        let a = t.update(&usage(82.0, 1000), &th, &[]);
        assert_eq!(a.len(), 1);
        assert!(a[0].title.contains("80%"));
        assert!(t.update(&usage(85.0, 1000), &th, &[]).is_empty());
        // Jumping past both remaining thresholds alerts once, for the highest.
        let a = t.update(&usage(99.0, 1000), &th, &[]);
        assert_eq!(a.len(), 1);
        assert!(a[0].title.contains("95%"));
        // New period: starts over.
        assert!(t.update(&usage(10.0, 2000), &th, &[]).is_empty());
        assert_eq!(t.update(&usage(81.0, 2000), &th, &[]).len(), 1);
    }

    #[test]
    fn session_levels_do_not_apply_to_weekly() {
        let mut t = ThresholdTracker::default();
        let mut u = usage(18.0, 1000);
        if let ProviderStatus::Ok { windows } = &mut u[0].status {
            windows[0].key = "weekly".into();
        }
        // 10% is a session level only; weekly alerts at 95%.
        assert!(t.update(&u, &[10], &[95]).is_empty());
    }
}
