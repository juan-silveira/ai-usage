//! Turns session state changes into desktop notifications.
//!
//! - A session that starts needing the user alerts immediately and then
//!   reminds every [`REMINDER_EVERY`] until answered or acknowledged.
//! - A finished turn alerts once, with a celebration for long turns.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ai_usage_core::i18n::tf;
use ai_usage_core::{Session, SessionState};
use chrono::{DateTime, Utc};

pub const REMINDER_EVERY: Duration = Duration::from_secs(120);
const MAX_REMINDERS: u32 = 5;
/// Turns at least this long get a celebratory "done" message.
const LONG_TURN_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    NeedsInput,
    Done,
}

pub struct Alert {
    pub kind: AlertKind,
    pub title: String,
    pub body: String,
}

struct Tracked {
    state: SessionState,
    updated_at: DateTime<Utc>,
    last_alert: Instant,
    reminders: u32,
    acknowledged: bool,
}

#[derive(Default)]
pub struct AlertTracker {
    seeded: bool,
    sessions: HashMap<String, Tracked>,
}

pub fn session_key(s: &Session) -> String {
    format!("{}-{}", s.provider, s.session_id)
}

fn provider_name(provider: &str) -> &str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        other => other,
    }
}

impl AlertTracker {
    /// Compares the latest scan with the previous one and returns the alerts
    /// to show. The first scan only records state, so restarting the app does
    /// not replay old events.
    pub fn update(&mut self, sessions: &[Session], now: Instant) -> Vec<Alert> {
        let mut alerts = Vec::new();
        let mut next = HashMap::with_capacity(sessions.len());

        for s in sessions {
            let key = session_key(s);
            let name = provider_name(&s.provider);
            let tracked = match self.sessions.remove(&key) {
                Some(mut t) if t.state == s.state && t.updated_at == s.updated_at => {
                    if s.state == SessionState::NeedsInput
                        && !t.acknowledged
                        && t.reminders < MAX_REMINDERS
                        && now.duration_since(t.last_alert) >= REMINDER_EVERY
                    {
                        t.reminders += 1;
                        t.last_alert = now;
                        alerts.push(Alert {
                            kind: AlertKind::NeedsInput,
                            title: tf("alert.reminder", &[("name", &name)]),
                            body: describe(s),
                        });
                    }
                    t
                }
                previous => {
                    let entered = previous.as_ref().is_none_or(|p| p.state != s.state);
                    if self.seeded {
                        match s.state {
                            // A new request while already waiting also alerts.
                            SessionState::NeedsInput => alerts.push(Alert {
                                kind: AlertKind::NeedsInput,
                                title: tf("alert.needs_input", &[("name", &name)]),
                                body: describe(s),
                            }),
                            SessionState::Done if entered => alerts.push(done_alert(name, s)),
                            _ => {}
                        }
                    }
                    Tracked {
                        state: s.state,
                        updated_at: s.updated_at,
                        last_alert: now,
                        reminders: 0,
                        acknowledged: false,
                    }
                }
            };
            next.insert(key, tracked);
        }

        self.sessions = next;
        self.seeded = true;
        alerts
    }

    /// Stops reminders for a session (the user clicked it in the window).
    pub fn acknowledge(&mut self, key: &str) {
        if let Some(t) = self.sessions.get_mut(key) {
            t.acknowledged = true;
        }
    }
}

fn describe(s: &Session) -> String {
    match &s.detail {
        Some(detail) => format!("{} — {detail}", s.project),
        None => s.project.clone(),
    }
}

fn done_alert(name: &str, s: &Session) -> Alert {
    let secs = s.last_turn_secs.unwrap_or(0);
    let mins = secs / 60;
    if secs >= LONG_TURN_SECS {
        Alert {
            kind: AlertKind::Done,
            title: tf("alert.done_long", &[("name", &name), ("n", &mins)]),
            body: describe(s),
        }
    } else {
        Alert {
            kind: AlertKind::Done,
            title: tf("alert.done", &[("name", &name)]),
            body: describe(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(state: SessionState, updated_secs: i64) -> Session {
        Session {
            provider: "claude".into(),
            session_id: "s".into(),
            cwd: "/p/proj".into(),
            project: "proj".into(),
            state,
            detail: None,
            detail_kind: None,
            model: None,
            turn_started_at: None,
            last_turn_secs: Some(400),
            updated_at: DateTime::from_timestamp(updated_secs, 0).unwrap(),
        }
    }

    #[test]
    fn first_scan_is_silent_then_alerts_on_changes() {
        let mut t = AlertTracker::default();
        let t0 = Instant::now();
        assert!(t
            .update(&[session(SessionState::NeedsInput, 1)], t0)
            .is_empty());

        let a = t.update(&[session(SessionState::Working, 2)], t0);
        assert!(a.is_empty());
        let a = t.update(&[session(SessionState::NeedsInput, 3)], t0);
        assert_eq!(a[0].title, tf("alert.needs_input", &[("name", &"Claude")]));
        let a = t.update(&[session(SessionState::Done, 4)], t0);
        assert!(a[0].title.starts_with("🎉"));
    }

    #[test]
    fn reminders_repeat_until_acknowledged() {
        let mut t = AlertTracker::default();
        let t0 = Instant::now();
        t.update(&[], t0);
        let waiting = [session(SessionState::NeedsInput, 1)];
        assert_eq!(t.update(&waiting, t0).len(), 1);
        assert!(t.update(&waiting, t0 + Duration::from_secs(60)).is_empty());
        let a = t.update(&waiting, t0 + REMINDER_EVERY);
        assert_eq!(a[0].title, tf("alert.reminder", &[("name", &"Claude")]));

        t.acknowledge("claude-s");
        assert!(t.update(&waiting, t0 + REMINDER_EVERY * 3).is_empty());
    }
}
