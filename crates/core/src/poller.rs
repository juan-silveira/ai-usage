//! Stateful polling for long-running monitors (the desktop app).
//!
//! The usage endpoints are rate limited. When one answers 429 the poller
//! waits as long as the server asks (bounded), backing off further on repeated
//! 429s, and meanwhile keeps serving the last good result marked as stale
//! instead of replacing it with an error. Network errors also fall back to
//! the last good result, but are retried on the next regular poll.
//!
//! The state is cached on disk, shared by the app and the CLI, so restarting
//! the app or running the CLI alongside it doesn't hit the endpoint again
//! when a fresh result (or a cooldown) is already known.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{ProviderStatus, ProviderUsage, Stale};
use crate::providers::{all_providers, Provider};

const MIN_BACKOFF_SECS: i64 = 60;
const DEFAULT_BACKOFF_SECS: i64 = 5 * 60;
const MAX_BACKOFF_SECS: i64 = 30 * 60;
/// A good result younger than this (just under a minute) is reused instead
/// of fetching again — covers app restarts and the CLI running alongside,
/// while a 1-minute schedule still gets a fresh result every tick.
const MIN_FETCH_INTERVAL_SECS: i64 = 55;

#[derive(Default, Serialize, Deserialize)]
struct ProviderState {
    last_ok: Option<ProviderUsage>,
    next_attempt: Option<DateTime<Utc>>,
    rate_limited: u32,
}

pub struct Poller {
    providers: Vec<Box<dyn Provider>>,
    state: HashMap<String, ProviderState>,
    cache: Option<PathBuf>,
}

impl Default for Poller {
    /// All providers, with the shared on-disk cache.
    fn default() -> Self {
        let mut poller = Self::new(all_providers());
        poller.cache = crate::sessions::state_dir()
            .and_then(|d| d.parent().map(|p| p.join("usage-cache.json")));
        poller
    }
}

impl Poller {
    /// A poller without disk cache (tests, embedding).
    pub fn new(providers: Vec<Box<dyn Provider>>) -> Self {
        Self {
            providers,
            state: HashMap::new(),
            cache: None,
        }
    }

    pub fn poll(&mut self) -> Vec<ProviderUsage> {
        self.load_cache();
        let now = Utc::now();
        let providers = std::mem::take(&mut self.providers);
        let results = providers
            .iter()
            .map(|p| {
                let state = self.state.entry(p.id().to_string()).or_default();
                let fresh = state.last_ok.as_ref().filter(|last| {
                    now - last.fetched_at < Duration::seconds(MIN_FETCH_INTERVAL_SECS)
                });
                match state.next_attempt {
                    // Still cooling down: don't touch the endpoint.
                    Some(at) if at > now => held_back(state, p.fetch_placeholder(), at),
                    _ if fresh.is_some() => fresh.unwrap().clone(),
                    _ => apply(state, p.fetch(), now),
                }
            })
            .collect();
        self.providers = providers;
        self.save_cache();
        results
    }

    /// Drops what is known about a provider (after signing out), so the next
    /// poll checks it again instead of reusing a recent result.
    pub fn forget(&mut self, provider: &str) {
        self.load_cache();
        self.state.remove(provider);
        self.save_cache();
    }

    /// Picks up results/cooldowns written by another process (CLI or a
    /// previous run of the app).
    fn load_cache(&mut self) {
        let Some(path) = &self.cache else { return };
        let Some(disk) = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<HashMap<String, ProviderState>>(&b).ok())
        else {
            return;
        };
        for (id, theirs) in disk {
            let ours = self.state.entry(id).or_default();
            let newer = |t: &ProviderState, o: &ProviderState| match (&t.last_ok, &o.last_ok) {
                (Some(t), Some(o)) => t.fetched_at > o.fetched_at,
                (Some(_), None) => true,
                _ => false,
            };
            if newer(&theirs, ours) {
                ours.last_ok = theirs.last_ok;
            }
            if theirs.next_attempt > ours.next_attempt {
                ours.next_attempt = theirs.next_attempt;
                ours.rate_limited = ours.rate_limited.max(theirs.rate_limited);
            }
        }
    }

    fn save_cache(&self) {
        let Some(path) = &self.cache else { return };
        if let (Some(dir), Ok(bytes)) = (path.parent(), serde_json::to_vec(&self.state)) {
            let _ = std::fs::create_dir_all(dir);
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}

/// What to show while a provider is in cooldown.
fn held_back(
    state: &ProviderState,
    placeholder: ProviderUsage,
    retry_at: DateTime<Utc>,
) -> ProviderUsage {
    match &state.last_ok {
        Some(last) => with_stale(last, &crate::i18n::t("stale.rate_limited"), Some(retry_at)),
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
            eprintln!(
                "ai-usage: {} usage endpoint returned 429 (Retry-After: {}), attempt {}",
                result.provider,
                retry_after_secs.map_or("none".to_string(), |s| format!("{s}s")),
                state.rate_limited
            );
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
                days: None,
                label: "s".into(),
                used_percent: pct,
                resets_at: None,
            }],
        }
    }

    /// Pretends the last good result is older than the minimum interval.
    fn age(poller: &mut Poller) {
        for state in poller.state.values_mut() {
            if let Some(last) = &mut state.last_ok {
                last.fetched_at -= Duration::seconds(MIN_FETCH_INTERVAL_SECS + 1);
            }
        }
    }

    #[test]
    fn recent_result_is_reused_without_fetching() {
        let queue = Arc::new(Mutex::new(vec![ok(10.0)]));
        let calls = Arc::new(Mutex::new(0));
        let mut poller = Poller::new(vec![Box::new(Fake {
            queue,
            calls: calls.clone(),
        })]);
        poller.poll();
        assert_eq!(poller.poll()[0].peak_percent(), Some(10.0));
        assert_eq!(*calls.lock().unwrap(), 1);
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
        age(&mut poller);
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
        age(&mut poller);
        assert_eq!(poller.poll()[0].stale.as_ref().unwrap().reason, "offline");
        let r = &poller.poll()[0];
        assert!(r.stale.is_none());
        assert_eq!(r.peak_percent(), Some(12.0));
    }
}
