// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use std::time::Duration;

use ia_usage_core::{fetch_all, ProviderUsage};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

const POLL_INTERVAL: Duration = Duration::from_secs(60);
const TRAY_ID: &str = "main";

/// Latest poll result, so a freshly opened window can render immediately.
#[derive(Default)]
struct Snapshot(Mutex<Vec<ProviderUsage>>);

#[tauri::command]
fn get_usage(state: State<Snapshot>) -> Vec<ProviderUsage> {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
fn refresh_now(app: AppHandle) {
    std::thread::spawn(move || poll_once(&app));
}

fn poll_once(app: &AppHandle) {
    let usage = fetch_all();
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip(&usage)));
        // Shown next to the icon on macOS and on Linux AppIndicator panels.
        let _ = tray.set_title(Some(short_title(&usage)));
    }
    *app.state::<Snapshot>().0.lock().unwrap() = usage.clone();
    let _ = app.emit("usage-updated", &usage);
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
        .manage(Snapshot::default())
        .invoke_handler(tauri::generate_handler![get_usage, refresh_now])
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
                poll_once(&handle);
                std::thread::sleep(POLL_INTERVAL);
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
