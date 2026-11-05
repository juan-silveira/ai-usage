//! `ai-usage` — AI subscription usage and live agent sessions.
//!
//!   ai-usage                     usage table
//!   ai-usage --json              usage as JSON (scripts, status bars, displays)
//!   ai-usage --watch 60          refresh every 60 seconds
//!   ai-usage sessions [--json]   live Claude Code / Codex sessions
//!   ai-usage hooks install|uninstall|status [claude|codex]
//!   ai-usage hook <provider>     hook handler (called by the agents, reads stdin)

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ai_usage_core::hook_install::{self, Target};
use ai_usage_core::i18n::{self, t, tf};
use ai_usage_core::{list_sessions, Poller, ProviderStatus, ProviderUsage, SessionState};
use chrono::{Datelike, Local, Utc};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hook") => run_hook(args.get(1).map(String::as_str)),
        Some("hooks") => run_hooks(&args[1..]),
        Some("sessions") => {
            print_sessions(args.iter().any(|a| a == "--json"));
            ExitCode::SUCCESS
        }
        _ => {
            run_usage(&args);
            ExitCode::SUCCESS
        }
    }
}

fn run_usage(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let watch = args.iter().position(|a| a == "--watch").map(|i| {
        args.get(i + 1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(60u64)
    });

    // Shares the app's cache and cooldowns, so running both never doubles
    // the requests to the rate-limited usage endpoints.
    let mut poller = Poller::default();
    loop {
        let mut usage = poller.poll();
        i18n::localize_usage(&mut usage);
        if json {
            println!("{}", serde_json::to_string(&usage).expect("serializable"));
        } else {
            if watch.is_some() {
                print!("\x1b[2J\x1b[H");
            }
            print_table(&usage);
        }
        match watch {
            Some(secs) => std::thread::sleep(Duration::from_secs(secs)),
            None => break,
        }
    }
}

/// Runs inside the agent's hook pipeline: must be silent and always succeed,
/// so a problem here can never block or clutter the agent.
fn run_hook(provider: Option<&str>) -> ExitCode {
    if let Some(provider @ ("claude" | "codex")) = provider {
        if let Ok(payload) = serde_json::from_reader::<_, serde_json::Value>(std::io::stdin()) {
            let _ = ai_usage_core::sessions::apply_hook_event(provider, &payload);
        }
    }
    ExitCode::SUCCESS
}

fn run_hooks(args: &[String]) -> ExitCode {
    let action = args.first().map(String::as_str).unwrap_or("status");
    let only = args.get(1).map(String::as_str);
    let targets: Vec<Target> = hook_install::targets()
        .into_iter()
        .filter(|t| only.is_none_or(|p| p == t.provider))
        .collect();
    if targets.is_empty() {
        eprintln!("{}", t("cli.unknown_provider"));
        return ExitCode::FAILURE;
    }

    let mut ok = true;
    for target in &targets {
        let path = target.config_path.display();
        let result = match action {
            "install" => {
                let exe = current_exe();
                if exe.components().any(|c| c.as_os_str() == "target") {
                    eprintln!("{}", tf("cli.build_warning", &[("path", &exe.display())]));
                }
                hook_install::install(target, &exe).map(|_| {
                    tf(
                        "cli.hooks_installed",
                        &[("provider", &target.provider), ("path", &path)],
                    )
                })
            }
            "uninstall" => hook_install::uninstall(target).map(|removed| {
                if removed {
                    tf(
                        "cli.hooks_removed",
                        &[("provider", &target.provider), ("path", &path)],
                    )
                } else {
                    tf(
                        "cli.hooks_none",
                        &[("provider", &target.provider), ("path", &path)],
                    )
                }
            }),
            "status" => Ok(format!(
                "{}: {} ({path})",
                target.provider,
                if hook_install::is_installed(target) {
                    t("cli.installed")
                } else {
                    t("cli.not_installed")
                }
            )),
            other => {
                eprintln!("{}", tf("cli.unknown_action", &[("action", &other)]));
                return ExitCode::FAILURE;
            }
        };
        match result {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!(
                    "{}",
                    tf(
                        "cli.provider_error",
                        &[("provider", &target.provider), ("error", &e)]
                    )
                );
                ok = false;
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn current_exe() -> PathBuf {
    let exe = std::env::current_exe().expect("current executable path");
    exe.canonicalize().unwrap_or(exe)
}

fn print_sessions(json: bool) {
    let mut sessions = list_sessions();
    i18n::localize_sessions(&mut sessions);
    if json {
        println!(
            "{}",
            serde_json::to_string(&sessions).expect("serializable")
        );
        return;
    }
    if sessions.is_empty() {
        println!("{}", t("cli.no_sessions"));
        return;
    }
    for s in &sessions {
        let state = match s.state {
            SessionState::NeedsInput => format!("⚠ {}", t("session.state.needs_input")),
            SessionState::Done => format!("✓ {}", t("session.state.done")),
            SessionState::Working => format!("… {}", t("session.state.working")),
            SessionState::Idle => format!("· {}", t("session.state.idle")),
        };
        let since = format_duration((Utc::now() - s.updated_at).num_seconds());
        let detail = s
            .detail
            .as_deref()
            .map(|d| format!(" — {d}"))
            .unwrap_or_default();
        println!(
            "{:<7} {:<20} {state} {}{detail}",
            s.provider,
            s.project,
            tf("cli.ago", &[("time", &since)])
        );
    }
}

fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}min", secs / 60),
        _ => format!("{}h{:02}", secs / 3600, (secs % 3600) / 60),
    }
}

fn print_table(usage: &[ProviderUsage]) {
    println!(
        "{}\n",
        tf(
            "cli.updated_at",
            &[("time", &Local::now().format("%H:%M:%S"))]
        )
    );
    // Providers the user doesn't use would only add "not connected" noise.
    for p in usage
        .iter()
        .filter(|p| !matches!(p.status, ProviderStatus::NoCredentials))
    {
        let plan = p
            .plan
            .as_deref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        println!("{}{plan}", p.display_name);
        match &p.status {
            ProviderStatus::Ok { windows } => {
                for w in windows {
                    println!(
                        "  {:<22} {} {:>5.1}%   {}",
                        w.label,
                        bar(w.used_percent),
                        w.used_percent,
                        w.resets_at.map(format_reset).unwrap_or_default()
                    );
                }
            }
            ProviderStatus::NoCredentials => println!("  {}", t("cli.no_credentials")),
            ProviderStatus::TokenExpired => println!("  {}", t("cli.token_expired")),
            ProviderStatus::RateLimited { retry_after_secs } => println!(
                "  {}{}",
                t("cli.rate_limited"),
                retry_after_secs
                    .map(|s| tf("cli.retry_in", &[("n", &s.div_ceil(60))]))
                    .unwrap_or_default()
            ),
            ProviderStatus::Error { message } => {
                println!("  {}", tf("cli.error", &[("message", message)]))
            }
        }
        println!();
    }
}

fn bar(percent: f64) -> String {
    let filled = ((percent / 100.0) * 20.0).round() as usize;
    format!(
        "[{}{}]",
        "█".repeat(filled),
        "·".repeat(20 - filled.min(20))
    )
}

fn format_reset(at: chrono::DateTime<Utc>) -> String {
    let mins = (at - Utc::now()).num_minutes().max(0);
    let (d, h, m) = (mins / 1440, (mins % 1440) / 60, mins % 60);
    let left = if d > 0 {
        format!("{d}d {h}h")
    } else {
        format!("{h}h {m:02}m")
    };
    let local = at.with_timezone(&Local);
    let when = format!(
        "{} {}",
        i18n::weekday(local.weekday().num_days_from_monday() as usize),
        local.format("%H:%M")
    );
    tf("cli.resets", &[("left", &left), ("when", &when)])
}
