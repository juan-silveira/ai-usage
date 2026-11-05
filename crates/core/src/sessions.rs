//! Live state of coding-agent sessions, fed by Claude Code / Codex hooks.
//!
//! Each hook invocation (`ia-usage hook <provider>`) updates one small JSON
//! file per session in the state directory. The desktop app only reads these
//! files, so hooks keep working (and never block the agent) while it is closed.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Sessions untouched for longer than this are considered abandoned (the
/// agent crashed or the terminal was closed without a SessionEnd).
const STALE_AFTER_HOURS: i64 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Session open, waiting for the first prompt.
    Idle,
    /// The agent is working on a turn.
    Working,
    /// Blocked on the user: permission prompt, question, dialog.
    NeedsInput,
    /// Turn finished; the agent is waiting for the next prompt.
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub provider: String,
    pub session_id: String,
    pub cwd: String,
    /// Last path component of `cwd`, for display.
    pub project: String,
    pub state: SessionState,
    /// Tool being run, permission being asked, or notification text.
    pub detail: Option<String>,
    pub model: Option<String>,
    pub turn_started_at: Option<DateTime<Utc>>,
    /// Length of the last completed turn, in seconds.
    pub last_turn_secs: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IA_USAGE_STATE_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("ia-usage").join("sessions"))
}

fn session_path(dir: &Path, provider: &str, session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{provider}-{safe}.json"))
}

/// Applies one hook payload (the JSON the agent writes to the hook's stdin).
/// Unknown events are ignored.
pub fn apply_hook_event(provider: &str, payload: &Value) -> std::io::Result<()> {
    let dir = state_dir().ok_or_else(|| std::io::Error::other("no state directory"))?;
    apply_hook_event_in(&dir, provider, payload, Utc::now())
}

fn apply_hook_event_in(
    dir: &Path,
    provider: &str,
    payload: &Value,
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    let str_field = |key: &str| payload.get(key).and_then(Value::as_str);
    let (Some(event), Some(session_id)) = (str_field("hook_event_name"), str_field("session_id"))
    else {
        return Ok(());
    };
    let path = session_path(dir, provider, session_id);

    if event == "SessionEnd" {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }

    let existing: Option<Session> = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let cwd = str_field("cwd").unwrap_or_default().to_string();
    let mut session = existing.unwrap_or_else(|| Session {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project: project_name(&cwd),
        cwd: cwd.clone(),
        state: SessionState::Idle,
        detail: None,
        model: None,
        turn_started_at: None,
        last_turn_secs: None,
        updated_at: now,
    });
    if !cwd.is_empty() && cwd != session.cwd {
        session.project = project_name(&cwd);
        session.cwd = cwd;
    }
    if let Some(model) = str_field("model") {
        session.model = Some(model.to_string());
    }

    match event {
        "SessionStart" => {
            session.state = SessionState::Idle;
            session.detail = None;
        }
        "UserPromptSubmit" => {
            session.state = SessionState::Working;
            session.detail = None;
            session.turn_started_at = Some(now);
        }
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "SubagentStart" | "PreCompact" => {
            session.state = SessionState::Working;
            session.detail = str_field("tool_name").map(str::to_string);
            session.turn_started_at.get_or_insert(now);
        }
        "PermissionRequest" => {
            session.state = SessionState::NeedsInput;
            session.detail = Some(match str_field("tool_name") {
                Some(tool) => format!("Permissão: {tool}"),
                None => "Pedido de permissão".to_string(),
            });
        }
        "Notification" => match str_field("notification_type") {
            Some(
                "permission_prompt"
                | "elicitation_dialog"
                | "elicitation_url_dialog"
                | "agent_needs_input",
            ) => {
                session.state = SessionState::NeedsInput;
                session.detail = str_field("message").map(str::to_string);
            }
            // idle_prompt and the rest are reminders/info, not state changes.
            _ => return Ok(()),
        },
        "Stop" | "StopFailure" => {
            session.state = SessionState::Done;
            session.last_turn_secs = session
                .turn_started_at
                .take()
                .map(|start| (now - start).num_seconds());
            session.detail = (event == "StopFailure").then(|| "Terminou com erro".to_string());
        }
        "Interrupt" => {
            session.state = SessionState::Idle;
            session.turn_started_at = None;
            session.detail = Some("Interrompida".to_string());
        }
        _ => return Ok(()),
    }
    session.updated_at = now;

    std::fs::create_dir_all(dir)?;
    // Write-then-rename so readers never see a half-written file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&session)?)?;
    std::fs::rename(&tmp, &path)
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

/// Lists live sessions, most urgent first, deleting abandoned ones.
pub fn list_sessions() -> Vec<Session> {
    state_dir()
        .map(|d| list_sessions_in(&d, Utc::now()))
        .unwrap_or_default()
}

fn list_sessions_in(dir: &Path, now: DateTime<Utc>) -> Vec<Session> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut sessions: Vec<Session> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let session: Session = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            if now - session.updated_at > Duration::hours(STALE_AFTER_HOURS) {
                let _ = std::fs::remove_file(e.path());
                return None;
            }
            Some(session)
        })
        .collect();
    sessions.sort_by(|a, b| {
        urgency(a.state)
            .cmp(&urgency(b.state))
            .then(b.updated_at.cmp(&a.updated_at))
    });
    sessions
}

fn urgency(state: SessionState) -> u8 {
    match state {
        SessionState::NeedsInput => 0,
        SessionState::Done => 1,
        SessionState::Working => 2,
        SessionState::Idle => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ia-usage-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn event(name: &str, extra: Value) -> Value {
        let mut v = json!({"hook_event_name": name, "session_id": "s1", "cwd": "/home/u/proj"});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    #[test]
    fn full_turn_lifecycle() {
        let dir = temp_dir("lifecycle");
        let t0 = Utc::now();
        let at = |secs| t0 + Duration::seconds(secs);
        let apply =
            |name, extra, t| apply_hook_event_in(&dir, "claude", &event(name, extra), t).unwrap();

        apply("SessionStart", json!({"model": "opus"}), at(0));
        apply("UserPromptSubmit", json!({"prompt": "hi"}), at(10));
        apply("PreToolUse", json!({"tool_name": "Bash"}), at(20));
        let s = &list_sessions_in(&dir, at(20))[0];
        assert_eq!(s.state, SessionState::Working);
        assert_eq!(s.detail.as_deref(), Some("Bash"));
        assert_eq!(s.project, "proj");

        apply(
            "Notification",
            json!({"notification_type": "permission_prompt", "message": "Allow Bash?"}),
            at(30),
        );
        assert_eq!(
            list_sessions_in(&dir, at(30))[0].state,
            SessionState::NeedsInput
        );

        // idle reminders do not change state
        apply(
            "Notification",
            json!({"notification_type": "idle_prompt"}),
            at(40),
        );
        assert_eq!(
            list_sessions_in(&dir, at(40))[0].state,
            SessionState::NeedsInput
        );

        apply("PostToolUse", json!({"tool_name": "Bash"}), at(50));
        apply("Stop", json!({}), at(370));
        let s = &list_sessions_in(&dir, at(370))[0];
        assert_eq!(s.state, SessionState::Done);
        assert_eq!(s.last_turn_secs, Some(360));
        assert_eq!(s.model.as_deref(), Some("opus"));

        apply("SessionEnd", json!({"reason": "other"}), at(400));
        assert!(list_sessions_in(&dir, at(400)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_sessions_are_dropped_and_urgent_first() {
        let dir = temp_dir("stale");
        let t0 = Utc::now();
        let mk = |id: &str, name: &str, t| {
            let mut e = event(name, json!({}));
            e["session_id"] = json!(id);
            apply_hook_event_in(&dir, "codex", &e, t).unwrap();
        };
        mk("old", "UserPromptSubmit", t0 - Duration::hours(13));
        mk("busy", "UserPromptSubmit", t0);
        mk("ask", "PermissionRequest", t0);
        let ids: Vec<_> = list_sessions_in(&dir, t0)
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        assert_eq!(ids, ["ask", "busy"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_payload_without_session() {
        let dir = temp_dir("nosession");
        apply_hook_event_in(
            &dir,
            "claude",
            &json!({"hook_event_name": "Stop"}),
            Utc::now(),
        )
        .unwrap();
        assert!(!dir.exists());
    }
}
