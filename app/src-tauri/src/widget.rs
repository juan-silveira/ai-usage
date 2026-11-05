//! Floating desktop widget: a small transparent window that stays on top of
//! everything and lets clicks pass through to whatever is underneath.
//!
//! Because it ignores the mouse, it is positioned through a "move mode"
//! (started from the preferences): the widget then accepts the mouse so it
//! can be dragged, and a double click (or the preferences button) pins it
//! again and saves its position.

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

pub const LABEL: &str = "widget";
const DEFAULT_MARGIN: i32 = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Background opacity, 0 (fully transparent) to 1.
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    /// Saved top-left corner in physical pixels.
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    /// Custom background colour (`#rrggbb`); `None` follows the theme.
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub sessions: SessionsMode,
}

/// How live sessions appear in the widget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionsMode {
    /// Counts: "⚠ 1 esperando você · ● 2 trabalhando".
    #[default]
    Summary,
    /// One line per session with project, state and time.
    List,
    Hidden,
}

/// Accepts `#rrggbb` only, normalised to lowercase.
pub fn valid_color(color: &str) -> Option<String> {
    let hex = color.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_lowercase()))
}

fn default_opacity() -> f64 {
    0.55
}

impl Default for WidgetSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            opacity: default_opacity(),
            x: None,
            y: None,
            color: None,
            sessions: SessionsMode::default(),
        }
    }
}

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

pub fn open(app: &AppHandle, settings: &WidgetSettings) -> tauri::Result<()> {
    if let Some(w) = window(app) {
        return w.show();
    }
    let w = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("widget.html".into()))
        .title("AI Usage — widget")
        .inner_size(230.0, 90.0)
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .build()?;

    let position = match (settings.x, settings.y) {
        (Some(x), Some(y)) => PhysicalPosition::new(x, y),
        _ => default_position(&w),
    };
    w.set_position(position)?;
    w.show()?;
    // Must come after show(): on Linux the input shape needs a realized
    // GDK window, and tao panics if there is none yet.
    w.set_ignore_cursor_events(true)
}

/// Top-right corner of the primary monitor, below a typical top panel.
fn default_position(w: &WebviewWindow) -> PhysicalPosition<i32> {
    let monitor = w.primary_monitor().ok().flatten();
    let width = w.outer_size().map(|s| s.width as i32).unwrap_or(230);
    match monitor {
        Some(m) => PhysicalPosition::new(
            m.position().x + m.size().width as i32 - width - DEFAULT_MARGIN,
            m.position().y + DEFAULT_MARGIN * 2,
        ),
        None => PhysicalPosition::new(DEFAULT_MARGIN, DEFAULT_MARGIN * 2),
    }
}

pub fn close(app: &AppHandle) {
    if let Some(w) = window(app) {
        let _ = w.destroy();
    }
}

/// Enters or leaves move mode. Leaving returns the position to save.
pub fn set_moving(app: &AppHandle, moving: bool) -> Option<PhysicalPosition<i32>> {
    let w = window(app)?;
    let _ = w.set_ignore_cursor_events(!moving);
    let _ = app.emit_to(LABEL, "widget-moving", moving);
    if moving {
        let _ = w.set_focus();
        None
    } else {
        w.outer_position().ok()
    }
}

/// The page reports its content size so the window hugs it.
pub fn fit(app: &AppHandle, width: f64, height: f64) {
    if let Some(w) = window(app) {
        let _ = w.set_size(LogicalSize::new(
            width.clamp(120.0, 600.0),
            height.clamp(40.0, 600.0),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_hex_colors() {
        assert_eq!(valid_color("#1A2B3C").as_deref(), Some("#1a2b3c"));
        assert_eq!(valid_color("1a2b3c"), None);
        assert_eq!(valid_color("#12345"), None);
        assert_eq!(valid_color("#12345g"), None);
    }
}
