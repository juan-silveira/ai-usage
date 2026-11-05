//! Finds the agents' CLIs (`claude`, `codex`) even when the app was started
//! from the desktop, whose PATH usually lacks version-manager directories
//! (asdf, nvm, volta, npm global prefixes).

use std::path::{Path, PathBuf};

pub fn find(name: &str) -> Option<PathBuf> {
    search_path(name)
        .or_else(|| {
            candidate_dirs()
                .into_iter()
                .find_map(|d| executable_in(&d, name))
        })
        .or_else(|| login_shell_lookup(name))
}

/// PATH for running a found CLI: its own directory first, because npm CLIs
/// are `#!/usr/bin/env node` scripts and `node` lives next to them.
pub fn child_path(cli: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = cli.parent().map(Path::to_path_buf).into_iter().collect();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

fn search_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| executable_in(&d, name))
}

fn executable_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let names: &[String] = if cfg!(windows) {
        &[
            format!("{name}.cmd"),
            format!("{name}.exe"),
            format!("{name}.ps1"),
        ]
    } else {
        &[name.to_string()]
    };
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

fn candidate_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut dirs = vec![
        home.join(".local/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join(".bun/bin"),
        home.join(".asdf/shims"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    // Every installed Node version under asdf / nvm, newest name last.
    for root in [
        home.join(".asdf/installs/nodejs"),
        home.join(".nvm/versions/node"),
    ] {
        if let Ok(entries) = std::fs::read_dir(&root) {
            let mut versions: Vec<PathBuf> =
                entries.flatten().map(|e| e.path().join("bin")).collect();
            versions.sort();
            dirs.extend(versions.into_iter().rev());
        }
    }
    dirs
}

/// Last resort on Unix: ask the user's login shell, which loads their
/// profile (and so their version manager).
#[cfg(unix)]
fn login_shell_lookup(name: &str) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let out = std::process::Command::new(shell)
        .args(["-lc", &format!("command -v {name}")])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (out.status.success() && path.is_file()).then_some(path)
}

#[cfg(not(unix))]
fn login_shell_lookup(_name: &str) -> Option<PathBuf> {
    None
}
