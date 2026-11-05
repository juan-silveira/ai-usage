//! Translations shared by the CLI, the app and its web views.
//!
//! Each locale is a flat JSON map in `locales/`, embedded in the binary.
//! Strings use `{name}` placeholders. A key missing from a locale falls back
//! to English, so a partial translation never shows a raw key.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::model::UsageWindow;
use crate::sessions::{DetailKind, Session};

/// Supported locales: code and the language's own name.
pub const LOCALES: &[(&str, &str)] = &[
    ("en", "English"),
    ("pt-BR", "Português (Brasil)"),
    ("es", "Español"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("it", "Italiano"),
    ("ru", "Русский"),
    ("pl", "Polski"),
    ("tr", "Türkçe"),
    ("zh-CN", "简体中文"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("hi", "हिन्दी"),
    ("id", "Bahasa Indonesia"),
    ("vi", "Tiếng Việt"),
];

const FALLBACK: &str = "en";

fn source(code: &str) -> Option<&'static str> {
    Some(match code {
        "en" => include_str!("../locales/en.json"),
        "pt-BR" => include_str!("../locales/pt-BR.json"),
        "es" => include_str!("../locales/es.json"),
        "fr" => include_str!("../locales/fr.json"),
        "de" => include_str!("../locales/de.json"),
        "it" => include_str!("../locales/it.json"),
        "ru" => include_str!("../locales/ru.json"),
        "pl" => include_str!("../locales/pl.json"),
        "tr" => include_str!("../locales/tr.json"),
        "zh-CN" => include_str!("../locales/zh-CN.json"),
        "ja" => include_str!("../locales/ja.json"),
        "ko" => include_str!("../locales/ko.json"),
        "hi" => include_str!("../locales/hi.json"),
        "id" => include_str!("../locales/id.json"),
        "vi" => include_str!("../locales/vi.json"),
        _ => return None,
    })
}

type Table = HashMap<String, String>;

fn table(code: &str) -> &'static Table {
    static TABLES: OnceLock<HashMap<&'static str, Table>> = OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        LOCALES
            .iter()
            .filter_map(|(code, _)| {
                let raw = source(code)?;
                let map = serde_json::from_str(raw)
                    .unwrap_or_else(|e| panic!("locales/{code}.json: {e}"));
                Some((*code, map))
            })
            .collect()
    });
    tables
        .get(code)
        .or_else(|| tables.get(FALLBACK))
        .expect("English locale")
}

fn current() -> &'static RwLock<&'static str> {
    static CURRENT: OnceLock<RwLock<&'static str>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(resolve(None)))
}

/// Maps a requested locale (or the system's, for `None`/"auto") to a
/// supported one: exact match, then same language, then English.
pub fn resolve(requested: Option<&str>) -> &'static str {
    let wanted = match requested {
        Some(code) if !code.is_empty() && code != "auto" => code.to_string(),
        _ => system_locale().unwrap_or_default(),
    };
    let wanted = wanted.replace('_', "-");
    let lang = wanted.split('-').next().unwrap_or_default();
    LOCALES
        .iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(&wanted))
        .or_else(|| {
            LOCALES.iter().find(|(code, _)| {
                code.split('-')
                    .next()
                    .is_some_and(|l| l.eq_ignore_ascii_case(lang))
            })
        })
        .map_or(FALLBACK, |(code, _)| code)
}

fn system_locale() -> Option<String> {
    // POSIX variables first so `LANG=xx ai-usage` works everywhere.
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.split(['.', '@']).next().unwrap_or_default();
            if !v.is_empty() && v != "C" && v != "POSIX" {
                return Some(v.to_string());
            }
        }
    }
    sys_locale::get_locale()
}

/// Sets the active locale ("auto" follows the system) and returns it.
pub fn set_locale(requested: &str) -> &'static str {
    let code = resolve(Some(requested));
    *current().write().unwrap() = code;
    code
}

pub fn locale() -> &'static str {
    *current().read().unwrap()
}

/// The active locale's strings, English filling any gaps (for the web views).
pub fn strings() -> Table {
    let mut all = table(FALLBACK).clone();
    all.extend(table(locale()).iter().map(|(k, v)| (k.clone(), v.clone())));
    all
}

/// Translated string for `key`; the key itself if no locale has it.
pub fn t(key: &str) -> String {
    table(locale())
        .get(key)
        .or_else(|| table(FALLBACK).get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// [`t`] with `{name}` placeholders filled in.
pub fn tf(key: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    args.iter().fold(t(key), |s, (name, value)| {
        s.replace(&format!("{{{name}}}"), &value.to_string())
    })
}

/// Short weekday name, Monday = 0.
pub fn weekday(monday_index: usize) -> String {
    t("weekdays_short")
        .split(',')
        .nth(monday_index)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Label for a usage window from its key and length, in the active locale.
pub fn window_label(w: &UsageWindow) -> String {
    match w.key.as_str() {
        "session" => t("window.session"),
        "weekly" => match w.days {
            None | Some(7) => t("window.weekly"),
            Some(28..=31) => t("window.monthly"),
            Some(n) => tf("window.days", &[("n", &n)]),
        },
        "credits" => t("window.credits"),
        "key_limit" => t("window.key_limit"),
        other => {
            if let Some(scope) = other.strip_prefix("weekly_") {
                tf("window.weekly_scoped", &[("scope", &scope_name(scope))])
            } else if let Some(scope) = other.strip_prefix("monthly_") {
                tf("window.monthly_scoped", &[("scope", &scope_name(scope))])
            } else {
                w.label.clone()
            }
        }
    }
}

/// "on_demand" → translated "On-demand"; unknown scopes (e.g. Claude's
/// per-model "opus") are just capitalized.
fn scope_name(raw: &str) -> String {
    let key = format!("scope.{raw}");
    let name = t(&key);
    if name == key {
        capitalize(raw)
    } else {
        name
    }
}

fn capitalize(raw: &str) -> String {
    let words = raw.replace('_', " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Detail line for a session, in the active locale.
pub fn session_detail(s: &Session) -> Option<String> {
    match s.detail_kind {
        Some(DetailKind::Permission) => Some(match &s.detail {
            Some(tool) => tf("session.detail.permission", &[("tool", tool)]),
            None => t("session.detail.permission_request"),
        }),
        Some(DetailKind::Failed) => Some(t("session.detail.failed")),
        Some(DetailKind::Interrupted) => Some(t("session.detail.interrupted")),
        None => s.detail.clone(),
    }
}

/// Fills in display labels (windows) for the active locale.
pub fn localize_usage(usage: &mut [crate::ProviderUsage]) {
    for p in usage {
        if let crate::ProviderStatus::Ok { windows } = &mut p.status {
            for w in windows {
                w.label = window_label(w);
            }
        }
    }
}

/// Replaces coded session details with text in the active locale.
pub fn localize_sessions(sessions: &mut [Session]) {
    for s in sessions {
        s.detail = session_detail(s);
        s.detail_kind = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_locale_parses_and_has_every_key() {
        let en = table("en");
        for (code, _) in LOCALES {
            let raw = source(code).expect("listed locale has a file");
            let map: Table = serde_json::from_str(raw).unwrap();
            let missing: Vec<_> = en.keys().filter(|k| !map.contains_key(*k)).collect();
            assert!(missing.is_empty(), "{code} is missing {missing:?}");
            for (k, v) in &map {
                let placeholders = |s: &str| {
                    let mut p: Vec<String> = s
                        .split('{')
                        .skip(1)
                        .filter_map(|x| x.split_once('}').map(|(n, _)| n.to_string()))
                        .collect();
                    p.sort();
                    p
                };
                if let Some(base) = en.get(k) {
                    assert_eq!(placeholders(v), placeholders(base), "{code}: {k}");
                }
            }
            assert_eq!(
                map["weekdays_short"].split(',').count(),
                7,
                "{code}: weekdays_short"
            );
        }
    }

    #[test]
    fn resolves_by_language() {
        assert_eq!(resolve(Some("pt_BR")), "pt-BR");
        assert_eq!(resolve(Some("pt-PT")), "pt-BR");
        assert_eq!(resolve(Some("zh-TW")), "zh-CN");
        assert_eq!(resolve(Some("es-MX")), "es");
        assert_eq!(resolve(Some("xx")), "en");
    }
}
