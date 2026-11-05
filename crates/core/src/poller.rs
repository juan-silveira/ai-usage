//! Stateful polling for long-running monitors (the desktop app).
//!
//! The usage endpoints are rate limited. When one answers 429 the poller
//! waits as long as the server asks (bounded), backing off further on repeated
//! 429s, and meanwhile keeps serving the last good result marked as stale
//! instead of replacing it with an error. Network errors also fall back to
//! the last good result, but are retried on the next regular poll.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use crate::model::{ProviderStatus, ProviderUsage, Stale};
use crate::providers::{all_providers, Provider};

const MIN_BACKOFF_SECS: i64 = 60;
const DEFAULT_BACKOFF_SECS: i64 = 5 * 60;
const MAX_BACKOFF_SECS: i64 = 30 * 60;

#[derive(Default)]
struct ProviderState {
    last_ok: Option<ProviderUsage>,
    next_attempt: Option<DateTime<Utc>>,
    rate_limited: u32,
}

pub struct Poller {
    providers: Vec<Box<dyn Provider>>,
    state: HashMap<&'static str, ProviderState>,
}

impl Default for Poller {
    fn default() -> Self {
        Self::new(all_providers())
    }
}

impl Poller {
    pub fn new(providers: Vec<Box<dyn Provider>>) -> Self {
        Self {
            providers,
            state: HashMap::new(),
        }
    }

    pub fn poll(&mut self) -> Vec<ProviderUsage> {
        let now = Utc::now();
        let providers = std::mem::take(&mut self.providers);
        let results = providers
            .iter()
            .map(|p| {
                let state = self.state.entry(p.id()).or_default();
                match state.next_attempt {
                    // Still cooling down: don't touch the endpoint.
                    Some(at) if at > now => held_back(state, p.fetch_placeholder(), at),
                    _ => apply(state, p.fetch(), now),
                }
            })
            .collect();
        self.providers = providers;
        results
    }
}

/// What to show while a provider is in cooldown.
fn held_back(
    state: &ProviderState,
    placeholder: ProviderUsage,
    retry_at: DateTime<Utc>,
) -> ProviderUsage {
    match &state.last_ok {
        Some(last) => with_stale(last, "limite de consultas (HTTP 429)", Some(retry_at)),
        None => placeholder,
    }
}

fn with_stale(
    last: &ProviderUsage,
    reason: &str,
    retry_at: Option<DateTime<Utc>>,
) -> ProviderUsage {
    let mut usage = last.clone();
    usage.stale = Some(Stale {
        reason: reason.to_string(),
        retry_at,
    });
    usage
}

fn apply(state: &mut ProviderState, result: ProviderUsage, now: DateTime<Utc>) -> ProviderUsage {
    match &result.status {
        ProviderStatus::Ok { .. } => {
            state.rate_limited = 0;
            state.next_attempt = None;
            state.last_ok = Some(result.clone());
            result
        }
        ProviderStatus::RateLimited { retry_after_secs } => {
            state.rate_limited += 1;
            let backoff = DEFAULT_BACKOFF_SECS
                .saturating_mul(1 << (state.rate_limited - 1).min(4))
                .min(MAX_BACKOFF_SECS);
            let wait = retry_after_secs
                .map(|s| (s as i64).clamp(MIN_BACKOFF_SECS, MAX_BACKOFF_SECS))
                .unwrap_or(backoff);
            let retry_at = now + Duration::seconds(wait);
            state.next_attempt = Some(retry_at);
            held_back(state, result, retry_at)
        }
        ProviderStatus::Error { message } => match &state.last_ok {
            Some(last) => with_stale(last, message, None),
            None => result,
        },
        // Login problems are real state changes: stop showing old numbers.
        ProviderStatus::NoCredentials | ProviderStatus::TokenExpired => {
            state.last_ok = None;
            state.next_attempt = None;
            result
        }
    }
}

trait Placeholder {
    fn fetch_placeholder(&self) -> ProviderUsage;
}

impl<P: Provider + ?Sized> Placeholder for P {
    /// A rate-limited entry for a provider with no good data yet.
    fn fetch_placeholder(&self) -> ProviderUsage {
        ProviderUsage {
            provider: self.id().to_string(),
            display_name: self.display_name().to_string(),
            plan: None,
            fetched_at: Utc::now(),
            status: ProviderStatus::RateLimited {
                retry_after_secs: None,
            },
            stale: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::model::UsageWindow;

    /// Returns queued statuses and counts real fetches.
    struct Fake {
        queue: Arc<Mutex<Vec<ProviderStatus>>>,
        calls: Arc<Mutex<u32>>,
    }

    impl Provider for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn display_name(&self) -> &'static str {
            "Fake"
        }
        fn poll(&self) -> (Option<String>, ProviderStatus) {
            *self.calls.lock().unwrap() += 1;
            (None, self.queue.lock().unwrap().remove(0))
        }
    }

    fn ok(pct: f64) -> ProviderStatus {
        ProviderStatus::Ok {
            windows: vec![UsageWindow {
                key: "session".into(),
                label: "s".into(),
                used_percent: pct,
                resets_at: None,
            }],
        }
    }

    #[test]
    fn rate_limit_keeps_last_good_data_and_stops_calling() {
        let queue = Arc::new(Mutex::new(vec![
            ok(10.0),
            ProviderStatus::RateLimited {
                retry_after_secs: Some(120),
            },
        ]));
        let calls = Arc::new(Mutex::new(0));
        let mut poller = Poller::new(vec![Box::new(Fake {
            queue: queue.clone(),
            calls: calls.clone(),
        })]);

        assert!(poller.poll()[0].stale.is_none());
        let r = &poller.poll()[0];
        assert!(matches!(r.status, ProviderStatus::Ok { .. }));
        let stale = r.stale.as_ref().expect("stale");
        assert!(stale.retry_at.unwrap() > Utc::now() + Duration::seconds(100));

        // During cooldown the endpoint is not called again.
        let r = &poller.poll()[0];
        assert!(r.stale.is_some());
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[test]
    fn network_error_falls_back_but_retries_next_time() {
        let queue = Arc::new(Mutex::new(vec![
            ok(10.0),
            ProviderStatus::Error {
                message: "offline".into(),
            },
            ok(12.0),
        ]));
        let calls = Arc::new(Mutex::new(0));
        let mut poller = Poller::new(vec![Box::new(Fake {
            queue,
            calls: calls.clone(),
        })]);
        poller.poll();
        assert_eq!(poller.poll()[0].stale.as_ref().unwrap().reason, "offline");
        let r = &poller.poll()[0];
        assert!(r.stale.is_none());
        assert_eq!(r.peak_percent(), Some(12.0));
    }
}
