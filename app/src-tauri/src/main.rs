// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod api;
mod bar;
mod connect;
mod migrate;
mod sound;
mod updates;
mod usage_alerts;
mod widget;
#[cfg(target_os = "linux")]
mod xapp_tray;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ai_usage_core::i18n::{self, t, tf};
use ai_usage_core::{list_sessions, Poller, ProviderUsage, Session};
use serde::{Deserialize, Serialize};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

use alerts::{AlertKind, AlertTracker};
use bar::{BarFormat, BarItem, BarReset, ProviderBar};
use sound::Sound;
use updates::UpdateInfo;
use usage_alerts::ThresholdTracker;
use widget::WidgetSettings;

/// Session files are tiny and local, so they can be scanned often.
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const TRAY_PREFIX: &str = "bar-";
/// Passed by the system autostart entry: start in the panel, window hidden.
const HIDDEN_ARG: &str = "--hidden";
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

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
    /// Alert levels (%) for the 5-hour session window, ascending.
    #[serde(default = "default_thresholds")]
    alert_session: Vec<u8>,
    /// Alert levels (%) for weekly windows (including per-model ones).
    #[serde(default = "default_thresholds")]
    alert_weekly: Vec<u8>,
    #[serde(default = "enabled")]
    sound_needs_input: bool,
    #[serde(default = "enabled")]
    sound_done: bool,
    #[serde(default = "enabled")]
    sound_threshold: bool,
    #[serde(default)]
    widget: WidgetSettings,
    /// Minutes between usage polls. The usage endpoints rate-limit
    /// aggressively; 1 minute triggers HTTP 429 on Claude.
    #[serde(default = "default_poll_minutes")]
    poll_minutes: u64,
    #[serde(default)]
    theme: ThemeChoice,
    /// Set once autostart has been turned on by default on the first run,
    /// so a user who turns it off isn't overridden on the next start.
    #[serde(default)]
    autostart_configured: bool,
    #[serde(default = "enabled")]
    check_updates: bool,
    #[serde(default)]
    sound_tones: sound::Tones,
    /// Alert sound volume, 0–100.
    #[serde(default = "full_volume")]
    sound_volume: u8,
    /// Focus mode: notifications and sounds held back while set.
    #[serde(default)]
    mute: Mute,
    /// Local HTTP API (see api.rs), off by default.
    #[serde(default)]
    api_enabled: bool,
    #[serde(default = "default_api_port")]
    api_port: u16,
    /// Providers the user disconnected in the app (their own sign-in, e.g.
    /// `gh` for Copilot, is left alone); they aren't polled.
    #[serde(default)]
    disabled_providers: Vec<String>,
    /// Interface language: a code from `i18n::LOCALES`, or "auto".
    #[serde(default = "auto_language")]
    language: String,
    /// Last version announced, so each release notifies only once.
    #[serde(default)]
    update_notified: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    fn window_theme(self) -> Option<tauri::Theme> {
        match self {
            ThemeChoice::System => None,
            ThemeChoice::Light => Some(tauri::Theme::Light),
            ThemeChoice::Dark => Some(tauri::Theme::Dark),
        }
    }
}

fn default_poll_minutes() -> u64 {
    5
}

fn default_api_port() -> u16 {
    api::DEFAULT_PORT
}

fn full_volume() -> u8 {
    100
}

/// Focus / meeting mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum Mute {
    #[default]
    Off,
    Until {
        until: chrono::DateTime<chrono::Utc>,
    },
    /// Until turned off.
    Always,
}

impl Mute {
    fn active(self) -> bool {
        match self {
            Mute::Off => false,
            Mute::Until { until } => until > chrono::Utc::now(),
            Mute::Always => true,
        }
    }
}

fn auto_language() -> String {
    "auto".into()
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
            alert_session: default_thresholds(),
            alert_weekly: default_thresholds(),
            sound_needs_input: true,
            sound_done: true,
            sound_threshold: true,
            widget: WidgetSettings::default(),
            poll_minutes: default_poll_minutes(),
            language: auto_language(),
            sound_tones: sound::Tones::default(),
            sound_volume: full_volume(),
            mute: Mute::Off,
            api_enabled: false,
            disabled_providers: Vec::new(),
            api_port: default_api_port(),
            theme: ThemeChoice::default(),
            autostart_configured: false,
            check_updates: true,
            update_notified: None,
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
    update: Mutex<Option<UpdateInfo>>,
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

/// Removes a session from the list (and the widget/tray) right away.
#[tauri::command]
fn dismiss_session(app: AppHandle, provider: String, session_id: String) -> Result<(), String> {
    ai_usage_core::sessions::remove_session(&provider, &session_id).map_err(|e| e.to_string())?;
    poll_sessions(&app);
    Ok(())
}

#[tauri::command]
fn acknowledge_session(state: State<Snapshot>, key: String) {
    state.alerts.lock().unwrap().acknowledge(&key);
}

#[tauri::command]
fn get_settings(state: State<Snapshot>) -> SettingsView {
    let mut settings = state.settings.lock().unwrap().clone();
    let mut default_names = BTreeMap::new();
    let mut ids: Vec<String> = ai_usage_core::all_providers()
        .iter()
        .map(|p| p.id().to_string())
        .collect();
    ids.extend(
        state
            .usage
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.provider.clone()),
    );
    for id in ids {
        settings
            .providers
            .entry(id.clone())
            .or_insert_with(|| bar::ProviderBar::default_for(&id));
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

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AlertWindow {
    Session,
    Weekly,
}

#[tauri::command]
fn set_alert_thresholds(
    app: AppHandle,
    state: State<Snapshot>,
    window: AlertWindow,
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
        match window {
            AlertWindow::Session => s.alert_session = clean.clone(),
            AlertWindow::Weekly => s.alert_weekly = clean.clone(),
        }
        s.clone()
    };
    let _ = app.emit("settings-updated", &settings);
    settings.save(&app).map_err(|e| e.to_string())?;
    Ok(clean)
}

/// Applies a change to the settings, saves them and tells every window.
fn change_settings(
    app: &AppHandle,
    change: impl FnOnce(&mut Settings),
) -> Result<Settings, String> {
    let settings = {
        let snapshot = app.state::<Snapshot>();
        let mut s = snapshot.settings.lock().unwrap();
        change(&mut s);
        s.clone()
    };
    let _ = app.emit("settings-updated", &settings);
    settings.save(app).map_err(|e| e.to_string())?;
    Ok(settings)
}

/// Async on purpose: creating a window from a synchronous command deadlocks
/// on Windows (WebView2 needs the main thread the command would block), so
/// the widget only appeared after restarting the app.
#[tauri::command]
async fn set_widget_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let settings = change_settings(&app, |s| s.widget.enabled = enabled)?;
    if enabled {
        widget::open(&app, &settings.widget).map_err(|e| e.to_string())
    } else {
        widget::close(&app);
        Ok(())
    }
}

#[tauri::command]
fn set_widget_opacity(app: AppHandle, opacity: f64) -> Result<(), String> {
    change_settings(&app, |s| s.widget.opacity = opacity.clamp(0.0, 1.0)).map(|_| ())
}

/// `None` (or an invalid value) returns to the theme's background.
#[tauri::command]
fn set_widget_color(app: AppHandle, color: Option<String>) -> Result<(), String> {
    let color = color.as_deref().and_then(widget::valid_color);
    change_settings(&app, |s| s.widget.color = color).map(|_| ())
}

#[tauri::command]
fn set_widget_sessions(app: AppHandle, mode: widget::SessionsMode) -> Result<(), String> {
    change_settings(&app, |s| s.widget.sessions = mode).map(|_| ())
}

#[tauri::command]
fn set_widget_provider(app: AppHandle, provider: String, show: bool) -> Result<(), String> {
    change_settings(&app, |s| {
        let shown = &mut s.widget.providers;
        shown.retain(|p| *p != provider);
        if show {
            shown.push(provider);
        }
    })
    .map(|_| ())
}

#[tauri::command]
fn set_widget_moving(app: AppHandle, moving: bool) -> Result<(), String> {
    if let Some(pos) = widget::set_moving(&app, moving) {
        change_settings(&app, |s| {
            s.widget.x = Some(pos.x);
            s.widget.y = Some(pos.y);
        })?;
    }
    let _ = app.emit("widget-moving", moving);
    Ok(())
}

#[tauri::command]
fn widget_fit(app: AppHandle, width: f64, height: f64) {
    widget::fit(&app, width, height);
}

#[derive(Serialize)]
struct UpdateStatus {
    current: &'static str,
    update: Option<UpdateInfo>,
    error: Option<String>,
}

/// Checks GitHub now; on a new version, notifies once per release.
fn check_updates(app: &AppHandle) -> UpdateStatus {
    let result = updates::check();
    let snapshot = app.state::<Snapshot>();
    if let Ok(found) = &result {
        *snapshot.update.lock().unwrap() = found.clone();
        let _ = app.emit("update-status", found);
    }
    if let Ok(Some(info)) = &result {
        let already = snapshot.settings.lock().unwrap().update_notified.clone();
        if already.as_deref() != Some(info.version.as_str()) {
            let _ = app
                .notification()
                .builder()
                .title(tf("update.notify_title", &[("version", &info.version)]))
                .body(t("update.notify_body"))
                .show();
            let version = info.version.clone();
            let _ = change_settings(app, |s| s.update_notified = Some(version));
        }
    }
    UpdateStatus {
        current: updates::CURRENT,
        update: result.as_ref().ok().cloned().flatten(),
        error: result.err(),
    }
}

#[tauri::command]
async fn check_updates_now(app: AppHandle) -> UpdateStatus {
    tauri::async_runtime::spawn_blocking(move || check_updates(&app))
        .await
        .unwrap_or(UpdateStatus {
            current: updates::CURRENT,
            update: None,
            error: Some("falha ao verificar".into()),
        })
}

#[tauri::command]
fn get_update(state: State<Snapshot>) -> UpdateStatus {
    UpdateStatus {
        current: updates::CURRENT,
        update: state.update.lock().unwrap().clone(),
        error: None,
    }
}

#[tauri::command]
fn set_poll_minutes(app: AppHandle, minutes: u64) -> Result<(), String> {
    change_settings(&app, |s| s.poll_minutes = minutes.clamp(1, 60)).map(|_| ())
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_theme(app: AppHandle, theme: ThemeChoice) -> Result<(), String> {
    // Native title bar follows too; None means "follow the system".
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_theme(theme.window_theme());
    }
    change_settings(&app, |s| s.theme = theme).map(|_| ())
}

#[derive(Serialize)]
struct Translations {
    /// Setting as chosen ("auto" or a code) and the locale in use.
    setting: String,
    locale: &'static str,
    locales: &'static [(&'static str, &'static str)],
    strings: std::collections::HashMap<String, String>,
}

#[tauri::command]
fn get_translations(state: State<Snapshot>) -> Translations {
    Translations {
        setting: state.settings.lock().unwrap().language.clone(),
        locale: i18n::locale(),
        locales: i18n::LOCALES,
        strings: i18n::strings(),
    }
}

/// Switches the language everywhere: tray menus, panel text, cached labels,
/// and the web views (which reload on "language-changed").
#[tauri::command]
fn set_language(app: AppHandle, language: String) -> Result<(), String> {
    i18n::set_locale(&language);
    change_settings(&app, |s| s.language = language)?;
    let snapshot = app.state::<Snapshot>();
    i18n::localize_usage(&mut snapshot.usage.lock().unwrap());
    relabel_menus(&app);
    update_tray(&app);
    let _ = app.emit("language-changed", ());
    Ok(())
}

#[tauri::command]
fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(t("err.https_only"));
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_check_updates(app: AppHandle, enabled: bool) -> Result<(), String> {
    change_settings(&app, |s| s.check_updates = enabled).map(|_| ())
}

/// Downloads, verifies and installs the new version, then restarts.
/// Progress goes out as "update-progress" ([downloaded, total] bytes).
/// Fails where in-place updates aren't possible (e.g. .deb/.rpm installs):
/// the window then opens the release page instead.
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    // A development build isn't an installed bundle: the updater would
    // replace the build output with the release AppImage.
    if cfg!(debug_assertions) {
        return Err("development build".into());
    }
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| t("update.status_none").replace("{current}", updates::CURRENT))?;
    let handle = app.clone();
    let mut downloaded = 0usize;
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk;
                let _ = handle.emit("update-progress", (downloaded, total));
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}

#[tauri::command]
fn open_update(app: AppHandle, state: State<Snapshot>) -> Result<(), String> {
    let url = state.update.lock().unwrap().as_ref().map(|u| u.url.clone());
    let url = url.unwrap_or_else(|| format!("https://github.com/{}/releases", updates::REPO));
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Plays `sound` with the user's tone and volume, unless focus mode is on.
fn alert_sound(settings: &Settings, sound: Sound) {
    if !settings.mute.active() {
        sound::play(settings.sound_tones.get(sound), settings.sound_volume);
    }
}

#[tauri::command]
fn preview_sound(state: State<Snapshot>, sound: Sound) {
    let settings = state.settings.lock().unwrap();
    sound::play(settings.sound_tones.get(sound), settings.sound_volume);
}

#[tauri::command]
fn set_sound_tone(app: AppHandle, sound: Sound, tone: sound::Tone) -> Result<(), String> {
    change_settings(&app, |s| s.sound_tones.set(sound, tone))?;
    sound::play(
        tone,
        app.state::<Snapshot>()
            .settings
            .lock()
            .unwrap()
            .sound_volume,
    );
    Ok(())
}

#[tauri::command]
fn set_sound_volume(app: AppHandle, volume: u8) -> Result<(), String> {
    change_settings(&app, |s| s.sound_volume = volume.min(100)).map(|_| ())
}

/// Focus mode: "off", minutes as a number, "tomorrow" (next 8:00) or "always".
#[tauri::command]
fn set_mute(app: AppHandle, mode: String) -> Result<Mute, String> {
    let mute = mute_from(&mode)?;
    change_settings(&app, |s| s.mute = mute)?;
    mute_changed(&app);
    Ok(mute)
}

fn mute_from(mode: &str) -> Result<Mute, String> {
    use chrono::{Duration as Span, Local, NaiveTime, Utc};
    Ok(match mode {
        "off" => Mute::Off,
        "always" => Mute::Always,
        "tomorrow" => {
            let morning = (Local::now() + Span::days(1))
                .with_time(NaiveTime::from_hms_opt(8, 0, 0).unwrap())
                .single()
                .ok_or("invalid local time")?;
            Mute::Until {
                until: morning.with_timezone(&Utc),
            }
        }
        minutes => Mute::Until {
            until: Utc::now() + Span::minutes(minutes.parse::<i64>().map_err(|e| e.to_string())?),
        },
    })
}

/// Focus mode state last announced (`poll_sessions` notices it expiring).
static WAS_MUTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tray menus, windows and API clients follow focus mode changes.
fn mute_changed(app: &AppHandle) {
    let mute = app.state::<Snapshot>().settings.lock().unwrap().mute;
    WAS_MUTED.store(mute.active(), std::sync::atomic::Ordering::Relaxed);
    relabel_menus(app);
    update_tray(app);
    let _ = app.emit("mute-updated", mute);
    api::broadcast(app, "mute", &mute);
}

/// Turns the local API on/off or moves it to another port.
#[tauri::command]
fn set_api(app: AppHandle, enabled: bool, port: u16) -> Result<(), String> {
    if port < 1024 {
        return Err(t("api.port_invalid"));
    }
    api::configure(&app, enabled, port)?;
    change_settings(&app, |s| {
        s.api_enabled = enabled;
        s.api_port = port;
    })
    .map(|_| ())
}

/// Label of a tray menu item in the current language and state.
fn menu_text(app: &AppHandle, id: &str) -> String {
    match id {
        "show" => t("tray.toggle"),
        "refresh" => t("tray.refresh"),
        "mute" => {
            if app
                .state::<Snapshot>()
                .settings
                .lock()
                .unwrap()
                .mute
                .active()
            {
                t("tray.unmute")
            } else {
                t("tray.mute_hour")
            }
        }
        _ => t("tray.quit"),
    }
}

/// Tray "mute" item: mutes for an hour, or unmutes when muted.
fn toggle_mute(app: &AppHandle) {
    let muted = app
        .state::<Snapshot>()
        .settings
        .lock()
        .unwrap()
        .mute
        .active();
    let _ = set_mute(app.clone(), if muted { "off" } else { "60" }.into());
}

const MENU_IDS: [&str; 4] = ["show", "refresh", "mute", "quit"];

fn relabel_menus(app: &AppHandle) {
    if let Some(menu) = app.state::<Snapshot>().menu.lock().unwrap().as_ref() {
        for id in MENU_IDS {
            if let Some(item) = menu.get(id).and_then(|i| i.as_menuitem().cloned()) {
                let _ = item.set_text(menu_text(app, id));
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || xapp_tray::relabel(&handle));
    }
}

#[tauri::command]
fn refresh_now(app: AppHandle) {
    std::thread::spawn(move || poll_usage(&app));
}

fn poll_usage(app: &AppHandle) {
    let snapshot = app.state::<Snapshot>();
    let mut usage = snapshot.poller.lock().unwrap().poll();
    i18n::localize_usage(&mut usage);
    *snapshot.usage.lock().unwrap() = usage.clone();

    let settings = snapshot.settings.lock().unwrap().clone();
    let alerts = snapshot.thresholds.lock().unwrap().update(
        &usage,
        &settings.alert_session,
        &settings.alert_weekly,
    );
    if settings.sound_threshold && !alerts.is_empty() {
        alert_sound(&settings, Sound::Threshold);
    }
    // Focus mode drops the alerts (they still count as given, so they
    // don't all arrive at once when it ends).
    let alerts = if settings.mute.active() {
        Vec::new()
    } else {
        alerts
    };
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
    api::broadcast(app, "usage", &usage);
}

fn poll_sessions(app: &AppHandle) {
    // A timed focus mode ends on its own: notice it here (every 2 s) so the
    // tray mark and menu follow.
    let muted = app
        .state::<Snapshot>()
        .settings
        .lock()
        .unwrap()
        .mute
        .active();
    if WAS_MUTED.load(std::sync::atomic::Ordering::Relaxed) != muted {
        mute_changed(app);
    }

    let mut sessions = list_sessions();
    i18n::localize_sessions(&mut sessions);
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
        alert_sound(&settings, sound);
    }
    let alerts = if settings.mute.active() {
        Vec::new()
    } else {
        alerts
    };
    for alert in alerts {
        let mut builder = app
            .notification()
            .builder()
            .title(alert.title)
            .body(alert.body);
        if let Some(icon) = notification_icon(alert.kind) {
            builder = builder.icon(icon);
        }
        let _ = builder.show();
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
        api::broadcast(app, "sessions", &sessions);
    }
}

/// Path of the notification icon for an alert: a warning triangle when a
/// session needs the user, a green check when it finished. Written to the
/// temp dir once, since notification daemons take a file path.
fn notification_icon(kind: AlertKind) -> Option<String> {
    static FILES: std::sync::OnceLock<Option<(String, String)>> = std::sync::OnceLock::new();
    let files = FILES.get_or_init(|| {
        let dir = std::env::temp_dir();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.join(format!("ai-usage-{}-{name}.png", std::process::id()));
            std::fs::write(&path, bytes).ok()?;
            Some(path.to_string_lossy().into_owned())
        };
        Some((
            write("attention", include_bytes!("../icons/notify/attention.png"))?,
            write("done", include_bytes!("../icons/notify/done.png"))?,
        ))
    });
    let (attention, done) = files.as_ref()?;
    Some(match kind {
        AlertKind::NeedsInput => attention.clone(),
        AlertKind::Done => done.clone(),
    })
}

fn update_tray(app: &AppHandle) {
    let snapshot = app.state::<Snapshot>();
    let items = {
        let usage = snapshot.usage.lock().unwrap();
        let sessions = snapshot.sessions.lock().unwrap();
        let settings = snapshot.settings.lock().unwrap();
        let mut items = bar::items(
            &usage,
            &sessions,
            settings.bar_format,
            settings.bar_reset,
            &settings.providers,
            chrono::Utc::now(),
        );
        // Focus mode is easy to forget: mark the first item while it's on.
        if settings.mute.active() {
            if let Some(first) = items.first_mut() {
                first.label = format!("🔕{}", first.label).trim_end().to_string();
            }
        }
        items
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
    let items = MENU_IDS
        .iter()
        .map(|id| MenuItem::with_id(app, *id, menu_text(app.handle(), id), true, None::<&str>))
        .collect::<tauri::Result<Vec<_>>>()?;
    let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = items
        .iter()
        .map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>)
        .collect();
    Menu::with_items(app, &refs)
}

fn main() {
    // `<app> hook <provider>`: the agents' hooks call the app binary itself,
    // so installing the app is enough for live sessions. Handle it before
    // any GUI setup; it must be fast, silent and always succeed.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("hook") {
        if let Some(provider @ ("claude" | "codex")) = args.get(2).map(String::as_str) {
            if let Ok(payload) = serde_json::from_reader::<_, serde_json::Value>(std::io::stdin()) {
                let _ = ai_usage_core::sessions::apply_hook_event(provider, &payload);
            }
        }
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .manage(Snapshot::default())
        .manage(connect::Logins::default())
        .manage(api::Api::default())
        .invoke_handler(tauri::generate_handler![
            get_usage,
            get_sessions,
            acknowledge_session,
            dismiss_session,
            get_settings,
            set_bar_format,
            set_provider_bar,
            set_bar_reset,
            set_alert_thresholds,
            set_widget_enabled,
            set_widget_opacity,
            set_widget_color,
            set_widget_sessions,
            set_widget_provider,
            set_widget_moving,
            widget_fit,
            check_updates_now,
            get_update,
            set_check_updates,
            set_poll_minutes,
            set_theme,
            get_translations,
            set_language,
            open_url,
            get_autostart,
            set_autostart,
            connect::get_connections,
            connect::start_login,
            connect::send_login_code,
            connect::cancel_login,
            connect::logout,
            connect::set_openrouter_key,
            connect::set_provider_enabled,
            connect::set_hooks,
            open_update,
            install_update,
            set_sound_enabled,
            preview_sound,
            set_sound_tone,
            set_sound_volume,
            set_mute,
            set_api,
            refresh_now
        ])
        .setup(|app| {
            migrate::legacy_name(app.handle());
            let settings = Settings::load(app.handle());
            i18n::set_locale(&settings.language);
            app.state::<Snapshot>()
                .poller
                .lock()
                .unwrap()
                .set_disabled(settings.disabled_providers.clone());
            if settings.api_enabled {
                if let Err(e) = api::configure(app.handle(), true, settings.api_port) {
                    eprintln!("ai-usage: local API not started: {e}");
                }
            }
            *app.state::<Snapshot>().settings.lock().unwrap() = settings;
            migrate::repoint_hooks();

            #[cfg(target_os = "linux")]
            let native_tray = xapp_tray::try_init(
                app.handle(),
                xapp_tray::Actions {
                    toggle: toggle_window,
                    refresh: |app| refresh_now(app.clone()),
                    mute: toggle_mute,
                    label: menu_text,
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
                "mute" => toggle_mute(app),
                "quit" => app.exit(0),
                _ => {}
            });
            update_tray(app.handle());
            let theme = app.state::<Snapshot>().settings.lock().unwrap().theme;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_theme(theme.window_theme());
                // Started by the system at login: stay in the panel only.
                if !std::env::args().any(|a| a == HIDDEN_ARG) {
                    let _ = w.show();
                }
            }

            // Start with the system by default, decided once on first run.
            let configured = app
                .state::<Snapshot>()
                .settings
                .lock()
                .unwrap()
                .autostart_configured;
            if !configured {
                let _ = app.autolaunch().enable();
                let _ = change_settings(app.handle(), |s| s.autostart_configured = true);
            } else if app.autolaunch().is_enabled().unwrap_or(false) {
                // Rewrite the entry so it points at this executable, in case
                // the app was moved, renamed or updated to another path.
                let _ = app.autolaunch().enable();
            }
            let widget_settings = app
                .state::<Snapshot>()
                .settings
                .lock()
                .unwrap()
                .widget
                .clone();
            if widget_settings.enabled {
                let _ = widget::open(app.handle(), &widget_settings);
            }

            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                poll_usage(&handle);
                // Re-read the interval while waiting so a change in the
                // preferences applies without waiting out the old one.
                let started = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_secs(5));
                    let minutes = handle
                        .state::<Snapshot>()
                        .settings
                        .lock()
                        .unwrap()
                        .poll_minutes;
                    if started.elapsed() >= Duration::from_secs(minutes * 60) {
                        break;
                    }
                }
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                // Give the first usage poll and the window a head start.
                std::thread::sleep(Duration::from_secs(30));
                loop {
                    let enabled = handle
                        .state::<Snapshot>()
                        .settings
                        .lock()
                        .unwrap()
                        .check_updates;
                    if enabled {
                        check_updates(&handle);
                    }
                    std::thread::sleep(UPDATE_CHECK_INTERVAL);
                }
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
                if window.label() == widget::LABEL {
                    return;
                }
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running AI Usage");
}
