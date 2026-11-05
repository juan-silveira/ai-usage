//! `ia-usage` — print AI subscription usage once, or every N seconds.
//!
//!   ia-usage            human-readable table
//!   ia-usage --json     JSON array (for scripts, status bars, hardware displays)
//!   ia-usage --watch 60 refresh every 60 seconds

use std::time::Duration;

use chrono::{Local, Utc};
use ia_usage_core::{fetch_all, ProviderStatus, ProviderUsage};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let watch = args
        .iter()
        .position(|a| a == "--watch")
        .map(|i| args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(60u64));

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

fn print_table(usage: &[ProviderUsage]) {
    println!("Atualizado em {}\n", Local::now().format("%H:%M:%S"));
    for p in usage {
        let plan = p.plan.as_deref().map(|s| format!(" ({s})")).unwrap_or_default();
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
    format!("[{}{}]", "█".repeat(filled), "·".repeat(20 - filled.min(20)))
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
