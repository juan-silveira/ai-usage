// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod sound;
#[cfg(target_os = "linux")]
mod xapp_tray;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ia_usage_core::{
    fetch_all, list_sessions, ProviderStatus, ProviderUsage, Session, SessionState,
};
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use alerts::{AlertKind, AlertTracker};
use sound::Sound;

const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Session files are tiny and local, so they can be scanned often.
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const TRAY_ID: &str = "main";

/// What the panel text next to the tray icon shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BarFormat {
    Off,
    /// Highest usage among the provider's windows.
    #[default]
    Peak,
    /// The 5-hour session window.
    Session,
    /// Session and weekly, e.g. "CL 5/17%".
    SessionWeekly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    bar_format: BarFormat,
    #[serde(default = "enabled")]
    sound_needs_input: bool,
    #[serde(default = "enabled")]
    sound_done: bool,
}

fn enabled() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bar_format: BarFormat::default(),
            sound_needs_input: true,
            sound_done: true,
        }
    }
}

impl Settings {
    fn path(app: &AppHandle) -> Option<PathBuf> {
        app.path()
            .app_config_dir()
            .ok()
            .map(|d| d.join("settings.json"))
    }

    fn load(app: &AppHandle) -> Self {
        Self::path(app)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, app: &AppHandle) -> std::io::Result<()> {
        let path = Self::path(app).ok_or_else(|| std::io::Error::other("no config dir"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }
}

/// Latest results, so a freshly opened window can render immediately.
#[derive(Default)]
struct Snapshot {
    usage: Mutex<Vec<ProviderUsage>>,
    sessions: Mutex<Vec<Session>>,
    alerts: Mutex<AlertTracker>,
    settings: Mutex<Settings>,
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
fn get_settings(state: State<Snapshot>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn set_bar_format(app: AppHandle, state: State<Snapshot>, format: BarFormat) -> Result<(), String> {
    let settings = {
        let mut s = state.settings.lock().unwrap();
        s.bar_format = format;
        s.clone()
    };
    update_tray(&app);
    settings.save(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_sound_enabled(
    app: AppHandle,
    state: State<Snapshot>,
    sound: Sound,
    enabled: bool,
) -> Result<(), String> {
    let settings = {
        let mut s = state.settings.lock().unwrap();
        match sound {
            Sound::NeedsInput => s.sound_needs_input = enabled,
            Sound::Done => s.sound_done = enabled,
        }
        s.clone()
    };
    settings.save(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_sound(sound: Sound) {
    sound::play(sound);
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
    let settings = snapshot.settings.lock().unwrap().clone();
    // One sound per scan even if several sessions changed at once;
    // needing the user wins over a finished turn.
    let sound =
        if settings.sound_needs_input && alerts.iter().any(|a| a.kind == AlertKind::NeedsInput) {
            Some(Sound::NeedsInput)
        } else if settings.sound_done && alerts.iter().any(|a| a.kind == AlertKind::Done) {
            Some(Sound::Done)
        } else {
            None
        };
    if let Some(sound) = sound {
        sound::play(sound);
    }
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
    let snapshot = app.state::<Snapshot>();
    let usage = snapshot.usage.lock().unwrap().clone();
    let format = snapshot.settings.lock().unwrap().bar_format;
    let waiting = snapshot
        .sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.state == SessionState::NeedsInput)
        .count();

    let mut title = bar_text(&usage, format);
    let mut tip = tooltip(&usage);
    if waiting > 0 {
        if format != BarFormat::Off {
            title = format!("⚠{waiting} {title}").trim_end().to_string();
        }
        tip = format!("{tip}\n{waiting} sessão(ões) esperando você");
    }

    #[cfg(target_os = "linux")]
    {
        let (title, tip) = (title.clone(), tip.clone());
        let _ = app.run_on_main_thread(move || {
            xapp_tray::update(&title, &tip);
        });
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tip));
        // Shown next to the icon on macOS and on Linux AppIndicator panels.
        let _ = tray.set_title(Some(title));
    }
}

fn short_name(provider: &str) -> String {
    match provider {
        "claude" => "CL".into(),
        "codex" => "CX".into(),
        other => other.chars().take(2).collect::<String>().to_uppercase(),
    }
}

fn bar_text(usage: &[ProviderUsage], format: BarFormat) -> String {
    if format == BarFormat::Off {
        return String::new();
    }
    usage
        .iter()
        .filter_map(|p| {
            let ProviderStatus::Ok { windows } = &p.status else {
                return None;
            };
            let pct = |key: &str| {
                windows
                    .iter()
                    .find(|w| w.key == key)
                    .map(|w| w.used_percent.round())
            };
            let peak = p.peak_percent()?.round();
            let value = match format {
                BarFormat::Session => format!("{:.0}%", pct("session").unwrap_or(peak)),
                BarFormat::SessionWeekly => match (pct("session"), pct("weekly")) {
                    (Some(s), Some(w)) => format!("{s:.0}/{w:.0}%"),
                    _ => format!("{peak:.0}%"),
                },
                _ => format!("{peak:.0}%"),
            };
            Some(format!("{} {value}", short_name(&p.provider)))
        })
        .collect::<Vec<_>>()
        .join("  ")
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

fn build_tray(app: &mut tauri::App) -> tauri::Result<()> {
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
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(Snapshot::default())
        .invoke_handler(tauri::generate_handler![
            get_usage,
            get_sessions,
            acknowledge_session,
            get_settings,
            set_bar_format,
            set_sound_enabled,
            preview_sound,
            refresh_now
        ])
        .setup(|app| {
            *app.state::<Snapshot>().settings.lock().unwrap() = Settings::load(app.handle());

            #[cfg(target_os = "linux")]
            let native_tray = xapp_tray::try_init(
                app.handle(),
                include_bytes!("../icons/128x128.png"),
                xapp_tray::Actions {
                    toggle: toggle_window,
                    refresh: |app| refresh_now(app.clone()),
                },
            );
            #[cfg(not(target_os = "linux"))]
            let native_tray = false;
            if !native_tray {
                build_tray(app)?;
            }

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
