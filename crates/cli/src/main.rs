//! `ia-usage` — AI subscription usage and live agent sessions.
//!
//!   ia-usage                     usage table
//!   ia-usage --json              usage as JSON (scripts, status bars, displays)
//!   ia-usage --watch 60          refresh every 60 seconds
//!   ia-usage sessions [--json]   live Claude Code / Codex sessions
//!   ia-usage hooks install|uninstall|status [claude|codex]
//!   ia-usage hook <provider>     hook handler (called by the agents, reads stdin)

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use chrono::{Local, Utc};
use ia_usage_core::hook_install::{self, Target};
use ia_usage_core::{fetch_all, list_sessions, ProviderStatus, ProviderUsage, SessionState};

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

    loop {
        let usage = fetch_all();
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
            let _ = ia_usage_core::sessions::apply_hook_event(provider, &payload);
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
        eprintln!("provedor desconhecido: use claude ou codex");
        return ExitCode::FAILURE;
    }

    let mut ok = true;
    for target in &targets {
        let path = target.config_path.display();
        let result = match action {
            "install" => {
                let exe = current_exe();
                if exe.components().any(|c| c.as_os_str() == "target") {
                    eprintln!(
                        "aviso: instalando hooks apontando para um binário de build ({}).\n       \
                         Prefira `cargo install --path crates/cli` e rode o ia-usage instalado.",
                        exe.display()
                    );
                }
                hook_install::install(target, &exe)
                    .map(|_| format!("{}: hooks instalados em {path}", target.provider))
            }
            "uninstall" => hook_install::uninstall(target).map(|removed| {
                if removed {
                    format!("{}: hooks removidos de {path}", target.provider)
                } else {
                    format!("{}: nenhum hook do ia-usage em {path}", target.provider)
                }
            }),
            "status" => Ok(format!(
                "{}: {} ({path})",
                target.provider,
                if hooks_installed(target) {
                    "instalado"
                } else {
                    "não instalado"
                }
            )),
            other => {
                eprintln!("ação desconhecida: {other} (use install, uninstall ou status)");
                return ExitCode::FAILURE;
            }
        };
        match result {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("{}: erro: {e}", target.provider);
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

fn hooks_installed(target: &Target) -> bool {
    std::fs::read_to_string(&target.config_path).is_ok_and(|text| {
        text.contains(&format!(" hook {}\"", target.provider)) && text.contains("ia-usage")
    })
}

fn print_sessions(json: bool) {
    let sessions = list_sessions();
    if json {
        println!(
            "{}",
            serde_json::to_string(&sessions).expect("serializable")
        );
        return;
    }
    if sessions.is_empty() {
        println!("Nenhuma sessão ativa (os hooks estão instalados? `ia-usage hooks status`)");
        return;
    }
    for s in &sessions {
        let state = match s.state {
            SessionState::NeedsInput => "⚠ precisa de você",
            SessionState::Done => "✓ concluída",
            SessionState::Working => "… trabalhando",
            SessionState::Idle => "· ociosa",
        };
        let since = format_duration((Utc::now() - s.updated_at).num_seconds());
        let detail = s
            .detail
            .as_deref()
            .map(|d| format!(" — {d}"))
            .unwrap_or_default();
        println!(
            "{:<7} {:<20} {state} há {since}{detail}",
            s.provider, s.project
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
    println!("Atualizado em {}\n", Local::now().format("%H:%M:%S"));
    for p in usage {
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
            ProviderStatus::NoCredentials => println!("  sem login (CLI não encontrada)"),
            ProviderStatus::TokenExpired => {
                println!("  token expirado — rode a CLI do provedor (ex.: `claude`, `codex`) para renovar")
            }
            ProviderStatus::Error { message } => println!("  erro: {message}"),
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
    format!(
        "reinicia em {left} ({})",
        at.with_timezone(&Local).format("%a %H:%M")
    )
}
