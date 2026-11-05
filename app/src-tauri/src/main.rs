// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod bar;
mod sound;
mod usage_alerts;
#[cfg(target_os = "linux")]
mod xapp_tray;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ia_usage_core::{list_sessions, Poller, ProviderUsage, Session};
use serde::{Deserialize, Serialize};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use alerts::{AlertKind, AlertTracker};
use bar::{BarFormat, BarItem, BarReset, ProviderBar};
use sound::Sound;
use usage_alerts::ThresholdTracker;

const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Session files are tiny and local, so they can be scanned often.
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const TRAY_PREFIX: &str = "bar-";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    bar_format: BarFormat,
    /// Panel options per provider id; missing entries use the defaults.
    #[serde(default)]
    providers: BTreeMap<String, ProviderBar>,
    #[serde(default)]
    bar_reset: BarReset,
    /// Usage levels (%) that trigger an alert, ascending.
    #[serde(default = "default_thresholds")]
    alert_thresholds: Vec<u8>,
    #[serde(default = "enabled")]
    sound_needs_input: bool,
    #[serde(default = "enabled")]
    sound_done: bool,
    #[serde(default = "enabled")]
    sound_threshold: bool,
}

fn default_thresholds() -> Vec<u8> {
    vec![80, 95]
}

fn enabled() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bar_format: BarFormat::default(),
            providers: BTreeMap::new(),
            bar_reset: BarReset::default(),
            alert_thresholds: default_thresholds(),
            sound_needs_input: true,
            sound_done: true,
            sound_threshold: true,
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
    poller: Mutex<Poller>,
    thresholds: Mutex<ThresholdTracker>,
    /// Tray icons currently shown by the Tauri backend: id -> icon bytes ptr.
    tray_icons: Mutex<Vec<(String, Option<usize>)>>,
    menu: Mutex<Option<Menu<tauri::Wry>>>,
}

/// Settings as the window sees them: every known provider is filled in.
#[derive(Serialize)]
struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    default_names: BTreeMap<String, String>,
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
fn get_settings(state: State<Snapshot>) -> SettingsView {
    let mut settings = state.settings.lock().unwrap().clone();
    let mut default_names = BTreeMap::new();
    let mut ids: Vec<String> = vec!["claude".into(), "codex".into()];
    ids.extend(
        state
            .usage
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.provider.clone()),
    );
    for id in ids {
        settings.providers.entry(id.clone()).or_default();
        default_names.insert(id.clone(), bar::default_name(&id));
    }
    SettingsView {
        settings,
        default_names,
    }
}

#[tauri::command]
fn set_provider_bar(
    app: AppHandle,
    state: State<Snapshot>,
    provider: String,
    config: ProviderBar,
) -> Result<(), String> {
    let settings = {
        let mut s = state.settings.lock().unwrap();
        s.providers.insert(provider, config);
        s.clone()
    };
    update_tray(&app);
    settings.save(&app).map_err(|e| e.to_string())
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
            Sound::Threshold => s.sound_threshold = enabled,
        }
        s.clone()
    };
    settings.save(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_bar_reset(app: AppHandle, state: State<Snapshot>, reset: BarReset) -> Result<(), String> {
    let settings = {
        let mut s = state.settings.lock().unwrap();
        s.bar_reset = reset;
        s.clone()
    };
    update_tray(&app);
    settings.save(&app).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_alert_thresholds(
    app: AppHandle,
    state: State<Snapshot>,
    thresholds: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let mut clean: Vec<u8> = thresholds
        .into_iter()
        .filter(|t| (1..=100).contains(t))
        .collect();
    clean.sort_unstable();
    clean.dedup();
    clean.truncate(5);
    let settings = {
        let mut s = state.settings.lock().unwrap();
        s.alert_thresholds = clean.clone();
        s.clone()
    };
    let _ = app.emit("settings-updated", &settings);
    settings.save(&app).map_err(|e| e.to_string())?;
    Ok(clean)
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
    let snapshot = app.state::<Snapshot>();
    let usage = snapshot.poller.lock().unwrap().poll();
    *snapshot.usage.lock().unwrap() = usage.clone();

    let settings = snapshot.settings.lock().unwrap().clone();
    let alerts = snapshot
        .thresholds
        .lock()
        .unwrap()
        .update(&usage, &settings.alert_thresholds);
    if settings.sound_threshold && !alerts.is_empty() {
        sound::play(Sound::Threshold);
    }
    for alert in alerts {
        let _ = app
            .notification()
            .builder()
            .title(alert.title)
            .body(alert.body)
            .show();
    }

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
    let items = {
        let usage = snapshot.usage.lock().unwrap();
        let sessions = snapshot.sessions.lock().unwrap();
        let settings = snapshot.settings.lock().unwrap();
        bar::items(
            &usage,
            &sessions,
            settings.bar_format,
            settings.bar_reset,
            &settings.providers,
            chrono::Utc::now(),
        )
    };
    // Tray icons are created and changed on the UI thread on every platform.
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        #[cfg(target_os = "linux")]
        if xapp_tray::sync(&items) {
            return;
        }
        sync_tauri_tray(&handle, &items);
    });
}

fn icon_image(bytes: Option<&'static [u8]>) -> Image<'static> {
    // A provider shown without its logo still needs some icon: use a
    // transparent pixel so only the text (macOS title) remains.
    bytes
        .and_then(|b| Image::from_bytes(b).ok())
        .unwrap_or_else(|| Image::new_owned(vec![0; 4], 1, 1))
}

fn sync_tauri_tray(app: &AppHandle, items: &[BarItem]) {
    let snapshot = app.state::<Snapshot>();
    let mut shown = snapshot.tray_icons.lock().unwrap();
    let key = |item: &BarItem| item.icon.map(|b| b.as_ptr() as usize);

    let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    if ids != shown.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>() {
        for (id, _) in shown.drain(..) {
            app.remove_tray_by_id(&format!("{TRAY_PREFIX}{id}"));
        }
        let menu = snapshot.menu.lock().unwrap().clone();
        for item in items {
            let mut builder = TrayIconBuilder::with_id(format!("{TRAY_PREFIX}{}", item.id))
                .icon(icon_image(item.icon))
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_window(tray.app_handle());
                    }
                });
            if let Some(menu) = &menu {
                builder = builder.menu(menu);
            }
            if builder.build(app).is_ok() {
                shown.push((item.id.clone(), key(item)));
            }
        }
    }

    for item in items {
        let Some(tray) = app.tray_by_id(&format!("{TRAY_PREFIX}{}", item.id)) else {
            continue;
        };
        if let Some(entry) = shown.iter_mut().find(|(id, _)| *id == item.id) {
            if entry.1 != key(item) {
                let _ = tray.set_icon(Some(icon_image(item.icon)));
                entry.1 = key(item);
            }
        }
        let _ = tray.set_tooltip(Some(&item.tooltip));
        // Shown next to the icon on macOS and on Linux AppIndicator panels;
        // Windows trays only have the icon and tooltip.
        let _ = tray.set_title(Some(&item.label));
    }
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

fn build_menu(app: &mut tauri::App) -> tauri::Result<Menu<tauri::Wry>> {
    let show = MenuItem::with_id(app, "show", "Mostrar / ocultar", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Atualizar agora", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Sair", true, None::<&str>)?;
    Menu::with_items(app, &[&show, &refresh, &quit])
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
            set_provider_bar,
            set_bar_reset,
            set_alert_thresholds,
            set_sound_enabled,
            preview_sound,
            refresh_now
        ])
        .setup(|app| {
            *app.state::<Snapshot>().settings.lock().unwrap() = Settings::load(app.handle());

            #[cfg(target_os = "linux")]
            let native_tray = xapp_tray::try_init(
                app.handle(),
                xapp_tray::Actions {
                    toggle: toggle_window,
                    refresh: |app| refresh_now(app.clone()),
                },
            );
            #[cfg(not(target_os = "linux"))]
            let native_tray = false;
            if !native_tray {
                let menu = build_menu(app)?;
                *app.state::<Snapshot>().menu.lock().unwrap() = Some(menu);
            }
            // Menu events from any tray icon built by the Tauri backend.
            app.on_menu_event(|app, event| match event.id.as_ref() {
                "show" => toggle_window(app),
                "refresh" => refresh_now(app.clone()),
                "quit" => app.exit(0),
                _ => {}
            });
            update_tray(app.handle());

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
