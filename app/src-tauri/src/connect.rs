//! "Connect" buttons: sign in through each agent's own CLI and turn live
//! session hooks on or off.
//!
//! Signing in is delegated to the official CLIs (`claude auth login`,
//! `codex login`): they open the browser, run the OAuth flow and write the
//! credentials the usage providers read. The app streams their output to the
//! window (so a link can be shown if no browser opened) and forwards a pasted
//! code to their stdin when the CLI asks for one.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

use ai_usage_core::i18n::tf;
use ai_usage_core::{cli_locate, hook_install};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

struct Agent {
    id: &'static str,
    name: &'static str,
    cli: &'static str,
    login_args: &'static [&'static str],
    logout_args: &'static [&'static str],
    install: &'static str,
}

const AGENTS: &[Agent] = &[
    Agent {
        id: "claude",
        name: "Claude Code",
        cli: "claude",
        login_args: &["auth", "login", "--claudeai"],
        logout_args: &["auth", "logout"],
        install: "npm install -g @anthropic-ai/claude-code",
    },
    Agent {
        id: "codex",
        name: "Codex",
        cli: "codex",
        login_args: &["login"],
        logout_args: &["logout"],
        install: "npm install -g @openai/codex",
    },
];

fn agent(id: &str) -> Result<&'static Agent, String> {
    AGENTS
        .iter()
        .find(|a| a.id == id)
        .ok_or_else(|| tf("err.unknown_provider", &[("id", &id)]))
}

#[derive(Serialize)]
pub struct Connection {
    provider: &'static str,
    name: &'static str,
    /// "cli": signs in through the agent's CLI and can install hooks;
    /// "app": uses another app's sign-in (nothing to do here);
    /// "key": needs an API key typed in the app.
    kind: &'static str,
    cli_path: Option<PathBuf>,
    install_command: &'static str,
    hooks_installed: bool,
    login_running: bool,
    /// For "key": where the key comes from ("env" or "file"), if any.
    key_source: Option<&'static str>,
    /// Turned off in the app ("app" kind only).
    disabled: bool,
}

/// Providers without a CLI sign-in of their own: (id, name, kind).
const OTHERS: &[(&str, &str, &str)] = &[
    ("copilot", "GitHub Copilot", "app"),
    ("cursor", "Cursor", "app"),
    ("openrouter", "OpenRouter", "key"),
];

#[derive(Default)]
pub struct Logins(Mutex<HashMap<String, (Child, ChildStdin)>>);

#[tauri::command]
pub fn get_connections(app: AppHandle) -> Vec<Connection> {
    let logins = app.state::<Logins>();
    let running = logins.0.lock().unwrap();
    let disabled = app
        .state::<crate::Snapshot>()
        .settings
        .lock()
        .unwrap()
        .disabled_providers
        .clone();
    let agents = AGENTS.iter().map(|a| Connection {
        provider: a.id,
        name: a.name,
        kind: "cli",
        cli_path: cli_locate::find(a.cli),
        install_command: a.install,
        hooks_installed: hook_install::target(a.id).is_some_and(|t| hook_install::is_installed(&t)),
        login_running: running.contains_key(a.id),
        key_source: None,
        disabled: false,
    });
    let others = OTHERS.iter().map(|&(id, name, kind)| Connection {
        provider: id,
        name,
        kind,
        cli_path: None,
        install_command: "",
        hooks_installed: false,
        login_running: false,
        key_source: (id == "openrouter")
            .then(ai_usage_core::providers::openrouter::key_source)
            .flatten(),
        disabled: disabled.iter().any(|d| d == id),
    });
    agents.chain(others).collect()
}

#[derive(Clone, Serialize)]
struct LoginEvent {
    provider: String,
    /// Output line from the CLI, if any.
    line: Option<String>,
    /// A sign-in URL found in the output.
    url: Option<String>,
    /// Set when the CLI exited.
    finished: Option<bool>,
}

#[tauri::command]
pub fn start_login(app: AppHandle, provider: String) -> Result<(), String> {
    let agent = agent(&provider)?;
    let cli = cli_locate::find(agent.cli).ok_or_else(|| {
        tf(
            "err.cli_not_found_install",
            &[("cli", &agent.cli), ("install", &agent.install)],
        )
    })?;

    let logins = app.state::<Logins>();
    let mut running = logins.0.lock().unwrap();
    if running.contains_key(&provider) {
        return Ok(());
    }
    let mut child = Command::new(&cli)
        .args(agent.login_args)
        .env("PATH", cli_locate::child_path(&cli))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| tf("err.cli_start", &[("cli", &agent.cli), ("error", &e)]))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    running.insert(provider.clone(), (child, stdin));
    drop(running);

    for stream in [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr)] {
        let app = app.clone();
        let provider = provider.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let line = strip_ansi(&line);
                if line.trim().is_empty() {
                    continue;
                }
                let _ = app.emit(
                    "login-event",
                    LoginEvent {
                        provider: provider.clone(),
                        url: find_url(&line),
                        line: Some(line),
                        finished: None,
                    },
                );
            }
        });
    }

    // Wait for the CLI to exit, then refresh usage with the new credentials.
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let logins = app.state::<Logins>();
        let mut running = logins.0.lock().unwrap();
        let Some((child, _)) = running.get_mut(&provider) else {
            return; // cancelled
        };
        if let Ok(Some(status)) = child.try_wait() {
            running.remove(&provider);
            drop(running);
            let _ = app.emit(
                "login-event",
                LoginEvent {
                    provider: provider.clone(),
                    line: None,
                    url: None,
                    finished: Some(status.success()),
                },
            );
            crate::refresh_now(app.clone());
            return;
        }
    });
    Ok(())
}

/// Sends a code pasted by the user to a CLI waiting for one.
#[tauri::command]
pub fn send_login_code(app: AppHandle, provider: String, code: String) -> Result<(), String> {
    let logins = app.state::<Logins>();
    let mut running = logins.0.lock().unwrap();
    let (_, stdin) = running.get_mut(&provider).ok_or("no sign-in in progress")?;
    writeln!(stdin, "{}", code.trim()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_login(app: AppHandle, provider: String) {
    let logins = app.state::<Logins>();
    let removed = logins.0.lock().unwrap().remove(&provider);
    if let Some((mut child, _)) = removed {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Signs out through the agent's CLI, which removes the credentials the usage
/// provider reads — and so also signs the CLI itself out.
#[tauri::command]
pub async fn logout(app: AppHandle, provider: String) -> Result<(), String> {
    let agent = agent(&provider)?;
    let cli = cli_locate::find(agent.cli)
        .ok_or_else(|| tf("err.cli_not_found", &[("cli", &agent.cli)]))?;
    let out = Command::new(&cli)
        .args(agent.logout_args)
        .env("PATH", cli_locate::child_path(&cli))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| tf("err.cli_start", &[("cli", &agent.cli), ("error", &e)]))?;
    app.state::<crate::Snapshot>()
        .poller
        .lock()
        .unwrap()
        .forget(&provider);
    crate::refresh_now(app);
    if out.status.success() {
        Ok(())
    } else {
        let err = strip_ansi(String::from_utf8_lossy(&out.stderr).trim());
        Err(if err.is_empty() {
            tf(
                "err.cli_exit",
                &[("cli", &agent.cli), ("status", &out.status)],
            )
        } else {
            err
        })
    }
}

/// Stops or resumes tracking a provider that signs in elsewhere (Copilot,
/// Cursor). Their own sign-in is not touched: signing out of `gh` would
/// break the GitHub CLI, and out of Cursor, the editor.
#[tauri::command]
pub fn set_provider_enabled(app: AppHandle, provider: String, enabled: bool) -> Result<(), String> {
    let disabled = crate::change_settings(&app, |s| {
        s.disabled_providers.retain(|p| *p != provider);
        if !enabled {
            s.disabled_providers.push(provider.clone());
        }
    })?
    .disabled_providers;
    let snapshot = app.state::<crate::Snapshot>();
    let mut poller = snapshot.poller.lock().unwrap();
    poller.set_disabled(disabled);
    poller.forget(&provider);
    drop(poller);
    crate::refresh_now(app);
    Ok(())
}

/// Saves the OpenRouter API key (an empty one removes it) and checks it now.
#[tauri::command]
pub fn set_openrouter_key(app: AppHandle, key: String) -> Result<(), String> {
    ai_usage_core::providers::openrouter::save_key(&key).map_err(|e| e.to_string())?;
    app.state::<crate::Snapshot>()
        .poller
        .lock()
        .unwrap()
        .forget("openrouter");
    crate::refresh_now(app);
    Ok(())
}

/// Turns live session hooks on or off. The hooks run this app's own
/// executable (`<app> hook <provider>`), so no separate CLI is needed.
#[tauri::command]
pub fn set_hooks(provider: String, enabled: bool) -> Result<(), String> {
    let target = hook_install::target(&provider)
        .ok_or_else(|| tf("err.unknown_provider", &[("id", &provider)]))?;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = exe.canonicalize().unwrap_or(exe);
        hook_install::install(&target, &exe).map_err(|e| e.to_string())
    } else {
        hook_install::uninstall(&target)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

fn find_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | ')'))
        .collect();
    Some(url)
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ ... final byte in @..~
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_url_and_strips_colors() {
        let line =
            "\u{1b}[1mOpen\u{1b}[0m https://claude.ai/oauth/authorize?code=true&x=1 to sign in";
        let clean = strip_ansi(line);
        assert!(clean.starts_with("Open https://"));
        assert_eq!(
            find_url(&clean).as_deref(),
            Some("https://claude.ai/oauth/authorize?code=true&x=1")
        );
    }
}
