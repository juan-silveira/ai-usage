//! One-off migration from the project's old name ("IA Usage", `ia-usage`)
//! and keeping hooks pointed at the current executable.

use std::path::Path;

use ai_usage_core::hook_install;
use tauri::{AppHandle, Manager};

const LEGACY_IDENTIFIER: &str = "io.github.ia-usage";
const LEGACY_DIR: &str = "ia-usage";

/// Runs before settings are loaded. Every step is best effort.
pub fn legacy_name(app: &AppHandle) {
    // Settings: ~/.config/io.github.ia-usage/settings.json and friends.
    if let Ok(new_dir) = app.path().app_config_dir() {
        let new_file = new_dir.join("settings.json");
        let old_file = new_dir
            .parent()
            .map(|p| p.join(LEGACY_IDENTIFIER).join("settings.json"));
        if let Some(old_file) = old_file.filter(|f| f.is_file() && !new_file.exists()) {
            if let Ok(text) = std::fs::read_to_string(&old_file) {
                let text = if remove_legacy_autostart() {
                    // Re-enable autostart under the new name on this start.
                    reset_autostart_flag(&text)
                } else {
                    text
                };
                let _ = std::fs::create_dir_all(&new_dir);
                let _ = std::fs::write(&new_file, text);
            }
        }
    }

    // Usage cache and live sessions: ~/.local/state/ia-usage/...
    if let Some(new_sessions) = ai_usage_core::sessions::state_dir() {
        if let Some(new_root) = new_sessions.parent() {
            if let Some(old_root) = new_root.parent().map(|p| p.join(LEGACY_DIR)) {
                // Moved, not copied: a copy left behind was copied again on
                // every start, bringing back sessions that had been removed
                // or had expired.
                move_missing(
                    &old_root.join("usage-cache.json"),
                    &new_root.join("usage-cache.json"),
                );
                let old_sessions = old_root.join("sessions");
                if let Ok(entries) = std::fs::read_dir(&old_sessions) {
                    for entry in entries.flatten() {
                        move_missing(&entry.path(), &new_sessions.join(entry.file_name()));
                    }
                }
                // Gone once empty (remove_dir refuses non-empty ones).
                let _ = std::fs::remove_dir(&old_sessions);
                let _ = std::fs::remove_dir(&old_root);
            }
        }
    }
}

/// Hooks run `<app> hook <provider>`. If they point at another executable
/// (the old name, a moved or updated install, or the separate CLI), point
/// them at this one so live sessions keep working.
pub fn repoint_hooks() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.canonicalize().unwrap_or(exe);
    for target in hook_install::targets() {
        let Some(current) = hook_install::installed_command(&target) else {
            continue;
        };
        if current != hook_install::hook_command(&exe, target.provider) {
            let _ = hook_install::install(&target, &exe);
        }
    }
}

/// Moves `from` to `to` unless `to` already exists (the new location wins);
/// either way `from` is removed, so it is migrated only once.
fn move_missing(from: &Path, to: &Path) {
    if !from.is_file() {
        return;
    }
    if !to.exists() {
        if let Some(dir) = to.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::rename(from, to).is_ok() {
            return;
        }
        if std::fs::copy(from, to).is_err() {
            return; // keep the original if it couldn't be carried over
        }
    }
    let _ = std::fs::remove_file(from);
}

/// Linux autostart entry written under the old product name. Returns true
/// if there was one.
fn remove_legacy_autostart() -> bool {
    #[cfg(target_os = "linux")]
    if let Some(file) = dirs_config().map(|d| d.join("autostart").join("IA Usage.desktop")) {
        return std::fs::remove_file(file).is_ok();
    }
    false
}

#[cfg(target_os = "linux")]
fn dirs_config() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
}

fn reset_autostart_flag(settings: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(settings) {
        Ok(mut v) => {
            v["autostart_configured"] = serde_json::Value::Bool(false);
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| settings.to_string())
        }
        Err(_) => settings.to_string(),
    }
}
