//! Optional local HTTP API for external widgets, status bars and displays.
//!
//! Read-only, on 127.0.0.1 only, off by default:
//!
//!   GET /v1/usage      usage per provider (same JSON as `ai-usage --json`)
//!   GET /v1/sessions   live agent sessions
//!   GET /v1/status     both, plus whether focus mode is on
//!   GET /v1/events     Server-Sent Events: `usage`, `sessions` and `mute`
//!                      events with the same JSON, sent when they change
//!
//! Requests whose `Host` isn't a loopback name are refused, so a web page
//! can't reach the API through DNS rebinding. No CORS headers are sent:
//! browser pages on other origins can't read the responses.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};

pub const DEFAULT_PORT: u16 = 47821;

#[derive(Default)]
pub struct Api {
    /// Open `/v1/events` connections.
    subscribers: Mutex<Vec<TcpStream>>,
    /// Port and stop flag of the running server.
    running: Mutex<Option<(u16, Arc<AtomicBool>)>>,
}

/// Starts, stops or moves the server to match the settings.
pub fn configure(app: &AppHandle, enabled: bool, port: u16) -> Result<(), String> {
    let api = app.state::<Api>();
    let mut running = api.running.lock().unwrap();
    if enabled && running.as_ref().is_some_and(|(p, _)| *p == port) {
        return Ok(());
    }
    if let Some((_, stop)) = running.take() {
        stop.store(true, Ordering::Relaxed);
        api.subscribers.lock().unwrap().clear();
    }
    if !enabled {
        return Ok(());
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    *running = Some((port, stop.clone()));
    let app = app.clone();
    std::thread::spawn(move || {
        // Non-blocking accept so a stop request is noticed promptly.
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let app = app.clone();
                    std::thread::spawn(move || handle(&app, stream, port));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(150));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    });
    Ok(())
}

/// Sends an event to every `/v1/events` client, dropping closed ones.
pub fn broadcast(app: &AppHandle, event: &str, data: &impl Serialize) {
    let api = app.state::<Api>();
    let mut subs = api.subscribers.lock().unwrap();
    if subs.is_empty() {
        return;
    }
    let Ok(json) = serde_json::to_string(data) else {
        return;
    };
    let message = format!("event: {event}\ndata: {json}\n\n");
    subs.retain_mut(|s| {
        s.write_all(message.as_bytes())
            .and_then(|_| s.flush())
            .is_ok()
    });
}

fn handle(app: &AppHandle, mut stream: TcpStream, port: u16) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Some((method, path, host)) = read_request(&stream) else {
        return;
    };
    if !is_loopback_host(host.as_deref(), port) {
        return respond(&mut stream, 403, "text/plain", "forbidden host");
    }
    if method != "GET" {
        return respond(&mut stream, 405, "text/plain", "only GET");
    }
    let path = path.split('?').next().unwrap_or_default();
    let snapshot = app.state::<crate::Snapshot>();
    let usage = || snapshot.usage.lock().unwrap().clone();
    let sessions = || snapshot.sessions.lock().unwrap().clone();
    let muted = || snapshot.settings.lock().unwrap().mute.active();
    let body = match path {
        "/v1/usage" => serde_json::to_string(&usage()),
        "/v1/sessions" => serde_json::to_string(&sessions()),
        "/v1/status" => serde_json::to_string(&serde_json::json!({
            "usage": usage(),
            "sessions": sessions(),
            "muted": muted(),
        })),
        "/v1/events" => return subscribe(app, stream),
        _ => return respond(&mut stream, 404, "text/plain", "not found"),
    };
    match body {
        Ok(json) => respond(&mut stream, 200, "application/json", &json),
        Err(e) => respond(&mut stream, 500, "text/plain", &e.to_string()),
    }
}

/// Method, path and Host header of the request; the rest is ignored.
fn read_request(stream: &TcpStream) -> Option<(String, String, Option<String>)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut host = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("host") {
                host = Some(value.trim().to_ascii_lowercase());
            }
        }
    }
    Some((method, path, host))
}

fn is_loopback_host(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else {
        return false;
    };
    let name = host.strip_suffix(&format!(":{port}")).unwrap_or(host);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &str) {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Keeps the connection open as an event stream, starting with the
/// current state so clients don't need a separate request.
fn subscribe(app: &AppHandle, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(None);
    // A stuck client must not hold up the others.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let snapshot = app.state::<crate::Snapshot>();
    let usage = serde_json::to_string(&*snapshot.usage.lock().unwrap()).unwrap_or_default();
    let sessions = serde_json::to_string(&*snapshot.sessions.lock().unwrap()).unwrap_or_default();
    let muted = snapshot.settings.lock().unwrap().mute;
    let mute = serde_json::to_string(&muted).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
         Connection: keep-alive\r\n\r\n\
         event: usage\ndata: {usage}\n\nevent: sessions\ndata: {sessions}\n\nevent: mute\ndata: {mute}\n\n"
    );
    if stream.write_all(head.as_bytes()).is_ok() {
        app.state::<Api>().subscribers.lock().unwrap().push(stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_hosts_are_accepted() {
        assert!(is_loopback_host(Some("127.0.0.1:47821"), 47821));
        assert!(is_loopback_host(Some("localhost:47821"), 47821));
        assert!(is_loopback_host(Some("localhost"), 47821));
        assert!(is_loopback_host(Some("[::1]:47821"), 47821));
        assert!(!is_loopback_host(Some("evil.example:47821"), 47821));
        assert!(!is_loopback_host(Some("127.0.0.1.evil.example"), 47821));
        assert!(!is_loopback_host(None, 47821));
    }
}
