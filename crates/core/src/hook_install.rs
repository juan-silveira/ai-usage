//! Registers `ai-usage hook <provider>` in the agents' hook configs:
//! `~/.claude/settings.json` (Claude Code) and `~/.codex/hooks.json` (Codex).
//!
//! Edits are merges: other keys and other hooks are left untouched, our own
//! entries are recognised by their command and replaced, and the original file
//! is copied to `<file>.ai-usage.bak` before the first change.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

pub struct Target {
    pub provider: &'static str,
    pub config_path: PathBuf,
    pub events: &'static [&'static str],
}

const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

/// Hooks must never slow the agent down; the handler only writes a small file.
const HOOK_TIMEOUT_SECS: u64 = 10;

pub fn targets() -> Vec<Target> {
    let home = dirs::home_dir().unwrap_or_default();
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    let codex_dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    vec![
        Target {
            provider: "claude",
            config_path: claude_dir.join("settings.json"),
            events: CLAUDE_EVENTS,
        },
        Target {
            provider: "codex",
            config_path: codex_dir.join("hooks.json"),
            events: CODEX_EVENTS,
        },
    ]
}

pub fn hook_command(exe: &Path, provider: &str) -> String {
    format!("\"{}\" hook {provider}", exe.display())
}

/// Our entries, including ones from before the rename (`ia-usage`).
fn is_ours(hook: &Value, provider: &str) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| {
            (c.contains("ai-usage") || c.contains("ia-usage"))
                && c.ends_with(&format!(" hook {provider}"))
        })
}

/// Removes our entries from every event; drops groups/events left empty.
/// Returns true if anything was removed.
pub fn remove_hooks(config: &mut Value, provider: &str) -> bool {
    let Some(hooks) = config.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|h| !is_ours(h, provider));
                changed |= list.len() != before;
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|l| !l.is_empty())
        });
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    if hooks.is_empty() {
        if let Some(obj) = config.as_object_mut() {
            obj.remove("hooks");
        }
    }
    changed
}

/// Adds one catch-all group per event running `command` (idempotent).
pub fn add_hooks(config: &mut Value, provider: &str, events: &[&str], command: &str) {
    remove_hooks(config, provider);
    if !config.is_object() {
        *config = Value::Object(Map::new());
    }
    let hooks = config
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks = hooks.as_object_mut().unwrap();
    for event in events {
        let groups = hooks
            .entry(*event)
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(groups) = groups.as_array_mut() {
            groups.push(json!({
                "hooks": [{"type": "command", "command": command, "timeout": HOOK_TIMEOUT_SECS}]
            }));
        }
    }
}

fn read_config(path: &Path) -> std::io::Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} is not valid JSON ({e}); not touching it",
                    path.display()
                ),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e),
    }
}

fn write_config(path: &Path, config: &Value) -> std::io::Result<()> {
    if path.exists() {
        let backup = path.with_extension("json.ai-usage.bak");
        if !backup.exists() {
            std::fs::copy(path, &backup)?;
        }
    } else if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(config)?;
    text.push('\n');
    let tmp = path.with_extension("json.ai-usage.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn install(target: &Target, exe: &Path) -> std::io::Result<()> {
    let mut config = read_config(&target.config_path)?;
    add_hooks(
        &mut config,
        target.provider,
        target.events,
        &hook_command(exe, target.provider),
    );
    write_config(&target.config_path, &config)
}

/// Whether the provider's config currently runs an ai-usage hook.
pub fn is_installed(target: &Target) -> bool {
    let Ok(text) = std::fs::read_to_string(&target.config_path) else {
        return false;
    };
    let Ok(config) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    config
        .get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|events| {
            events
                .values()
                .flat_map(|g| g.as_array().into_iter().flatten())
                .any(|group| {
                    group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .is_some_and(|l| l.iter().any(|h| is_ours(h, target.provider)))
                })
        })
}

/// The command of our installed hook, if any (first one found).
pub fn installed_command(target: &Target) -> Option<String> {
    let text = std::fs::read_to_string(&target.config_path).ok()?;
    let config: Value = serde_json::from_str(&text).ok()?;
    config
        .get("hooks")?
        .as_object()?
        .values()
        .flat_map(|g| g.as_array().into_iter().flatten())
        .flat_map(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find(|h| is_ours(h, target.provider))
        .and_then(|h| h.get("command")?.as_str().map(str::to_string))
}

pub fn target(provider: &str) -> Option<Target> {
    targets().into_iter().find(|t| t.provider == provider)
}

/// Returns true if the file had our hooks.
pub fn uninstall(target: &Target) -> std::io::Result<bool> {
    if !target.config_path.exists() {
        return Ok(false);
    }
    let mut config = read_config(&target.config_path)?;
    let changed = remove_hooks(&mut config, target.provider);
    if changed {
        write_config(&target.config_path, &config)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_preserves_existing_and_is_idempotent() {
        let mut config = json!({
            "model": "opus",
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]
            }
        });
        let cmd = hook_command(Path::new("/usr/bin/ai-usage"), "claude");
        add_hooks(&mut config, "claude", &["Stop", "SessionStart"], &cmd);
        add_hooks(&mut config, "claude", &["Stop", "SessionStart"], &cmd);

        assert_eq!(config["model"], "opus");
        let stop = config["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "notify-send done");
        assert_eq!(stop[1]["hooks"][0]["command"], cmd);
        assert_eq!(config["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        // key order of the user's file is kept
        let keys: Vec<_> = config.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks"]);
    }

    #[test]
    fn uninstall_leaves_only_foreign_hooks() {
        let mut config = json!({
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]}
        });
        let cmd = hook_command(Path::new("/opt/ai-usage"), "claude");
        add_hooks(&mut config, "claude", &["Stop", "SessionEnd"], &cmd);
        assert!(remove_hooks(&mut config, "claude"));
        assert_eq!(
            config,
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]}})
        );
        assert!(!remove_hooks(&mut config, "claude"));
    }

    #[test]
    fn legacy_name_entries_are_replaced() {
        let mut config = json!({});
        add_hooks(
            &mut config,
            "claude",
            &["Stop"],
            "\"/old/ia-usage\" hook claude",
        );
        add_hooks(
            &mut config,
            "claude",
            &["Stop"],
            "\"/new/ai-usage-app\" hook claude",
        );
        let stop = config["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(
            stop[0]["hooks"][0]["command"],
            "\"/new/ai-usage-app\" hook claude"
        );
    }

    #[test]
    fn uninstall_removes_empty_hooks_key() {
        let mut config = json!({"theme": "dark"});
        add_hooks(
            &mut config,
            "codex",
            &["Stop"],
            "\"/x/ai-usage\" hook codex",
        );
        remove_hooks(&mut config, "codex");
        assert_eq!(config, json!({"theme": "dark"}));
    }
}
