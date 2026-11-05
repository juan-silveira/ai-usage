// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use ia_usage_core::{fetch_all, list_sessions, ProviderUsage, Session, SessionState};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use alerts::AlertTracker;

const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Session files are tiny and local, so they can be scanned often.
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const TRAY_ID: &str = "main";

/// Latest results, so a freshly opened window can render immediately.
#[derive(Default)]
struct Snapshot {
    usage: Mutex<Vec<ProviderUsage>>,
    sessions: Mutex<Vec<Session>>,
    alerts: Mutex<AlertTracker>,
}

#[tauri::command]
fn get_usage(state: State<Snapshot>) -> Vec<ProviderUsage> {
    state.usage.lock().unwrap().clone()
}

#[tauri::command]
fn get_sessions(state: State<Snapshot>) -> Vec<Session> {
    state.sessions.lock().unwrap().clone()
}

#[tauri::command]
fn acknowledge_session(state: State<Snapshot>, key: String) {
    state.alerts.lock().unwrap().acknowledge(&key);
}

#[tauri::command]
fn refresh_now(app: AppHandle) {
    std::thread::spawn(move || poll_usage(&app));
}

fn poll_usage(app: &AppHandle) {
    let usage = fetch_all();
    *app.state::<Snapshot>().usage.lock().unwrap() = usage.clone();
    update_tray(app);
    let _ = app.emit("usage-updated", &usage);
}

fn poll_sessions(app: &AppHandle) {
    let sessions = list_sessions();
    let snapshot = app.state::<Snapshot>();
    let alerts = snapshot
        .alerts
        .lock()
        .unwrap()
        .update(&sessions, Instant::now());
    for alert in alerts {
        let _ = app
            .notification()
            .builder()
            .title(alert.title)
            .body(alert.body)
            .show();
    }

    let changed = {
        let mut current = snapshot.sessions.lock().unwrap();
        let changed = serde_json::to_value(&*current).ok() != serde_json::to_value(&sessions).ok();
        *current = sessions.clone();
        changed
    };
    if changed {
        update_tray(app);
        let _ = app.emit("sessions-updated", &sessions);
    }
}

fn update_tray(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let snapshot = app.state::<Snapshot>();
    let usage = snapshot.usage.lock().unwrap().clone();
    let waiting = snapshot
        .sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.state == SessionState::NeedsInput)
        .count();

    let mut title = short_title(&usage);
    let mut tip = tooltip(&usage);
    if waiting > 0 {
        title = format!("⚠{waiting} {title}");
        tip = format!("{tip}\n{waiting} sessão(ões) esperando você");
    }
    let _ = tray.set_tooltip(Some(tip));
    // Shown next to the icon on macOS and on Linux AppIndicator panels.
    let _ = tray.set_title(Some(title));
}

fn short_title(usage: &[ProviderUsage]) -> String {
    usage
        .iter()
        .filter_map(|p| {
            let initial = p.display_name.chars().next()?;
            Some(format!("{initial} {:.0}%", p.peak_percent()?))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn tooltip(usage: &[ProviderUsage]) -> String {
    let lines: Vec<String> = usage
        .iter()
        .map(|p| match p.peak_percent() {
            Some(pct) => format!("{}: {pct:.0}%", p.display_name),
            None => format!("{}: —", p.display_name),
        })
        .collect();
    format!("IA Usage\n{}", lines.join("\n"))
}

fn toggle_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(Snapshot::default())
        .invoke_handler(tauri::generate_handler![
            get_usage,
            get_sessions,
            acknowledge_session,
            refresh_now
        ])
        .setup(|app| {
            let show = MenuItem::with_id(app, "show", "Mostrar / ocultar", true, None::<&str>)?;
            let refresh = MenuItem::with_id(app, "refresh", "Atualizar agora", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Sair", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &refresh, &quit])?;

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("IA Usage")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => toggle_window(app),
                    "refresh" => refresh_now(app.clone()),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_window(tray.app_handle());
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                poll_usage(&handle);
                std::thread::sleep(USAGE_POLL_INTERVAL);
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                poll_sessions(&handle);
                std::thread::sleep(SESSION_POLL_INTERVAL);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the app alive in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running IA Usage");
}
