//! The desktop async question tool returns as soon as it shows the form.
//! Match accepted calls and later user replies in local Codex JSONL logs.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::providers::Codex;
use crate::sessions::{sort_sessions, PendingQuestion, Session};

const ASYNC_TOOL: &str = "request_user_input_async";
const REPLY_TAG: &str = "<send_user_message_question_reply>";
const REPLY_END: &str = "</send_user_message_question_reply>";
const MAX_AGE: Duration = Duration::from_secs(12 * 60 * 60);

pub(crate) fn has_options(input: &Value) -> bool {
    option_question_count(input).is_some()
}

fn option_question_count(input: &Value) -> Option<usize> {
    let items = input.get("questions")?.as_array()?;
    (!items.is_empty()
        && items.iter().all(|q| {
            q["options"]
                .as_array()
                .is_some_and(|options| !options.is_empty())
        }))
    .then_some(items.len())
}

#[derive(Default)]
pub struct CodexQuestions {
    logs: HashMap<PathBuf, Log>,
    dismissed: HashSet<String>,
}

impl CodexQuestions {
    pub fn update(&mut self, sessions: &mut Vec<Session>) {
        let enabled = crate::hook_install::target("codex")
            .is_some_and(|target| crate::hook_install::is_installed(&target));
        if !enabled {
            self.logs.clear();
            return;
        }
        self.update_from(
            sessions,
            Codex::ACCOUNTS.into_iter().filter_map(Codex::home),
            Utc::now(),
        );
    }

    fn update_from(
        &mut self,
        sessions: &mut Vec<Session>,
        homes: impl Iterator<Item = PathBuf>,
        now: DateTime<Utc>,
    ) {
        let cutoff = SystemTime::now() - MAX_AGE;
        let mut found = HashSet::new();
        for home in homes {
            scan_logs(
                &home.join("sessions"),
                3,
                cutoff,
                &mut |path, len, modified| {
                    if found.insert(path.to_owned()) {
                        self.logs
                            .entry(path.to_owned())
                            .or_default()
                            .read(path, len, modified);
                    }
                },
            );
        }
        self.logs.retain(|path, _| found.contains(path));
        let mut active = HashSet::new();
        for log in self.logs.values() {
            let mut questions: Vec<_> = log
                .calls
                .iter()
                .filter(|(_, call)| call.accepted && call.at >= now - MAX_AGE)
                .map(|(id, call)| PendingQuestion {
                    request_id: id.clone(),
                    requested_at: call.at,
                })
                .collect();
            let Some(id) = log.session_id.as_deref() else {
                continue;
            };
            if questions.is_empty() {
                continue;
            }
            questions.retain(|q| {
                active.insert(q.request_id.clone());
                !self.dismissed.contains(&q.request_id)
            });
            if questions.is_empty() {
                continue;
            }
            let session = if let Some(index) = sessions
                .iter()
                .position(|s| s.provider == "codex" && s.session_id == id)
            {
                &mut sessions[index]
            } else {
                sessions.push(Session::new("codex", id, &log.cwd, now));
                sessions.last_mut().unwrap()
            };
            session.pending_questions.extend(questions);
            session
                .pending_questions
                .sort_by(|a, b| a.request_id.cmp(&b.request_id));
            session
                .pending_questions
                .dedup_by(|a, b| a.request_id == b.request_id);
            session.show_pending_questions();
        }
        self.dismissed.retain(|id| active.contains(id));
        sort_sessions(sessions);
    }

    pub fn dismiss(&mut self, session: &Session) {
        self.dismissed.extend(
            session
                .pending_questions
                .iter()
                .map(|q| q.request_id.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn async_question_waits_for_replies_not_for_the_tool_acknowledgment() {
        let home =
            std::env::temp_dir().join(format!("ai-usage-async-questions-{}", std::process::id()));
        let logs = home.join("sessions/2026/11/05");
        fs::create_dir_all(&logs).unwrap();
        let mut file = File::create(logs.join("rollout.jsonl")).unwrap();
        let now = Utc::now();
        let meta =
            json!({"type": "session_meta", "payload": {"id": "thread-1", "cwd": "/test/project"}});
        let record =
            |payload: Value| json!({"timestamp": now, "type": "response_item", "payload": payload});
        let question = json!({"options": [{"label": "A"}, {"label": "B"}]});
        let call = record(
            json!({"type": "function_call", "name": ASYNC_TOOL, "call_id": "call-1",
            "arguments": json!({"questions": [question, question]}).to_string()}),
        );
        let accepted = record(json!({"type": "function_call_output", "call_id": "call-1",
            "output": "{\"accepted\":true}"}));
        let reply = |index| {
            let items =
                json!([{"questionItemId": json!([ASYNC_TOOL, "call-1", index]).to_string()}]);
            let text = format!("{REPLY_TAG}\n{items}\n{REPLY_END}");
            record(
                json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}),
            )
        };
        let mut tracker = CodexQuestions::default();
        let mut scan = || {
            let mut sessions = Vec::new();
            tracker.update_from(&mut sessions, std::iter::once(home.clone()), now);
            sessions
        };

        writeln!(file, "{meta}\n{call}").unwrap();
        assert!(scan().is_empty());
        writeln!(file, "{accepted}").unwrap();
        let pending = scan();
        assert_eq!(pending[0].session_id, "thread-1");
        assert_eq!(pending[0].pending_questions[0].request_id, "call-1");
        writeln!(file, "{}", reply(0)).unwrap();
        assert_eq!(scan(), pending);
        write!(file, "{}", reply(1)).unwrap();
        assert_eq!(
            scan(),
            pending,
            "a partially written record must wait for its newline"
        );
        writeln!(file).unwrap();
        assert!(scan().is_empty());
        fs::remove_dir_all(home).unwrap();
    }
}

fn scan_logs(
    dir: &Path,
    depth: usize,
    cutoff: SystemTime,
    visit: &mut impl FnMut(&Path, u64, SystemTime),
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if depth > 0 && kind.is_dir() {
            scan_logs(&path, depth - 1, cutoff, visit);
        } else if kind.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
            let Ok(meta) = entry.metadata() else { continue };
            if let Ok(modified) = meta.modified() {
                if modified >= cutoff {
                    visit(&path, meta.len(), modified);
                }
            }
        }
    }
}

struct Call {
    at: DateTime<Utc>,
    unanswered: HashSet<usize>,
    accepted: bool,
}

#[derive(Default)]
struct Log {
    offset: u64,
    file_len: u64,
    modified: Option<SystemTime>,
    session_id: Option<String>,
    cwd: String,
    calls: HashMap<String, Call>,
}

impl Log {
    fn read(&mut self, path: &Path, len: u64, modified: SystemTime) {
        if len == self.file_len && self.modified == Some(modified) {
            return;
        }
        if len <= self.file_len {
            *self = Self::default();
        }
        let Ok(file) = File::open(path) else { return };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut line = Vec::new();
        while let Ok(size) = reader.read_until(b'\n', &mut line) {
            if size == 0 || !line.ends_with(b"\n") {
                break;
            }
            self.offset += size as u64;
            // Skip other transcript contents without parsing their JSON.
            if let Ok(text) = std::str::from_utf8(&line) {
                let candidate = text.contains("session_meta")
                    || text.contains(ASYNC_TOOL)
                    || (!self.calls.is_empty() && text.contains("function_call_output"));
                if candidate {
                    if let Ok(record) = serde_json::from_str(text) {
                        self.record(&record);
                    }
                }
            }
            line.clear();
        }
        self.file_len = len;
        self.modified = Some(modified);
    }

    fn record(&mut self, record: &Value) {
        let p = &record["payload"];
        match record["type"].as_str() {
            Some("session_meta") => {
                self.session_id = p["id"].as_str().map(str::to_owned);
                self.cwd = p["cwd"].as_str().unwrap_or_default().to_owned();
            }
            Some("response_item") => match p["type"].as_str() {
                Some("function_call") if p["name"] == ASYNC_TOOL => {
                    let Some(id) = p["call_id"].as_str() else {
                        return;
                    };
                    let at = record["timestamp"].as_str().and_then(|s| s.parse().ok());
                    let count = p["arguments"]
                        .as_str()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .as_ref()
                        .and_then(option_question_count);
                    if let (Some(at), Some(count)) = (at, count) {
                        self.calls.entry(id.to_owned()).or_insert_with(|| Call {
                            at,
                            unanswered: (0..count).collect(),
                            accepted: false,
                        });
                    }
                }
                Some("function_call_output") => {
                    let Some(id) = p["call_id"].as_str() else {
                        return;
                    };
                    let Some(call) = self.calls.get_mut(id) else {
                        return;
                    };
                    call.accepted = p["output"]
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .is_some_and(|result| result["accepted"] == true);
                    if !call.accepted {
                        self.calls.remove(id);
                    }
                }
                Some("message") if p["role"] == "user" => {
                    if let Some(content) = p["content"].as_array() {
                        for text in content.iter().filter_map(|c| c["text"].as_str()) {
                            self.reply(text);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn reply(&mut self, text: &str) {
        let Some(body) = text
            .trim()
            .strip_prefix(REPLY_TAG)
            .and_then(|s| s.strip_suffix(REPLY_END))
        else {
            return;
        };
        let Ok(replies) = serde_json::from_str::<Vec<Value>>(body) else {
            return;
        };
        for reply in replies {
            let Some(item) = reply["questionItemId"].as_str() else {
                continue;
            };
            let Ok((tool, id, index)) = serde_json::from_str::<(String, String, usize)>(item)
            else {
                continue;
            };
            if tool != ASYNC_TOOL {
                continue;
            }
            if let Some(call) = self.calls.get_mut(&id) {
                call.unanswered.remove(&index);
                if call.unanswered.is_empty() {
                    self.calls.remove(&id);
                }
            }
        }
    }
}
