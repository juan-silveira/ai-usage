//! Native Cinnamon / XApp status icons, used instead of the regular tray icon
//! when an XApp status applet is running (Linux Mint, Cinnamon, MATE/Xfce
//! with the XApp applet).
//!
//! Why: Cinnamon shows StatusNotifier (AppIndicator) icons through
//! `xapp-sn-watcher`, which drops the indicator label, so usage text set with
//! `TrayIcon::set_title` never reaches the panel. An XAppStatusIcon supports a
//! label natively, like the weather applet.
//!
//! libxapp is loaded at runtime so the app still starts on systems without it.
//! Everything here must run on the GTK main thread.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;

use gtk::glib::translate::{FromGlibPtrFull, ToGlibPtr};
use gtk::glib::{self, ObjectExt};
use gtk::prelude::*;
use libloading::Library;
use tauri::AppHandle;

use crate::bar::BarItem;

type NewFn = unsafe extern "C" fn() -> *mut c_void;
type SetStrFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type SetMenuFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type AnyMonitorsFn = unsafe extern "C" fn() -> c_int;

pub struct Actions {
    pub toggle: fn(&AppHandle),
    pub refresh: fn(&AppHandle),
}

struct Api {
    // Keeps libxapp loaded for as long as icons exist.
    _lib: Library,
    new: NewFn,
    set_icon_name: SetStrFn,
    set_name: SetStrFn,
    set_label: SetStrFn,
    set_tooltip: SetStrFn,
    set_secondary_menu: SetMenuFn,
}

struct Icon {
    // Dropping the object unregisters the icon from the panel.
    obj: glib::Object,
    _menu: gtk::Menu,
    current: Option<BarItem>,
}

struct State {
    api: Api,
    app: AppHandle,
    actions: Actions,
    icons: Vec<(String, Icon)>,
    icon_files: HashMap<usize, CString>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Loads libxapp if an XApp status applet is listening. Returns false when
/// the caller should use the regular tray icon instead.
pub fn try_init(app: &AppHandle, actions: Actions) -> bool {
    let Ok(api) = (unsafe { load() }) else {
        return false;
    };
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            api,
            app: app.clone(),
            actions,
            icons: Vec::new(),
            icon_files: HashMap::new(),
        })
    });
    true
}

unsafe fn load() -> Result<Api, Box<dyn std::error::Error>> {
    let lib = Library::new("libxapp.so.1")?;
    let any_monitors: AnyMonitorsFn = *lib.get(b"xapp_status_icon_any_monitors\0")?;
    if any_monitors() == 0 {
        return Err("no XApp status applet running".into());
    }
    Ok(Api {
        new: *lib.get(b"xapp_status_icon_new\0")?,
        set_icon_name: *lib.get(b"xapp_status_icon_set_icon_name\0")?,
        set_name: *lib.get(b"xapp_status_icon_set_name\0")?,
        set_label: *lib.get(b"xapp_status_icon_set_label\0")?,
        set_tooltip: *lib.get(b"xapp_status_icon_set_tooltip_text\0")?,
        set_secondary_menu: *lib.get(b"xapp_status_icon_set_secondary_menu\0")?,
        _lib: lib,
    })
}

/// Makes the panel show exactly `items`, creating, updating and removing
/// icons as needed. Returns false if XApp is not in use.
pub fn sync(items: &[BarItem]) -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(state) = s.as_mut() else {
            return false;
        };
        // Recreate everything when the set or order of items changes, so the
        // panel order follows the item order; otherwise update in place.
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        let current: Vec<&str> = state.icons.iter().map(|(id, _)| id.as_str()).collect();
        if ids != current {
            state.icons.clear();
            for item in items {
                if let Some(icon) = unsafe { state.create(&item.id) } {
                    state.icons.push((item.id.clone(), icon));
                }
            }
        }
        for item in items {
            unsafe { state.update(item) };
        }
        true
    })
}

impl State {
    unsafe fn create(&self, id: &str) -> Option<Icon> {
        let raw = (self.api.new)();
        if raw.is_null() {
            return None;
        }
        let obj = glib::Object::from_glib_full(raw as *mut glib::gobject_ffi::GObject);
        let name = CString::new(format!("ia-usage-{id}")).ok()?;
        (self.api.set_name)(raw, name.as_ptr());

        let menu = gtk::Menu::new();
        for (label, action) in [
            ("Mostrar / ocultar", self.actions.toggle),
            ("Atualizar agora", self.actions.refresh),
            ("Sair", |app: &AppHandle| app.exit(0)),
        ] {
            let item = gtk::MenuItem::with_label(label);
            let app = self.app.clone();
            item.connect_activate(move |_| action(&app));
            menu.append(&item);
        }
        menu.show_all();
        let menu_ptr: *mut gtk::ffi::GtkMenu = menu.to_glib_none().0;
        (self.api.set_secondary_menu)(raw, menu_ptr as *mut c_void);

        // Left click (no primary menu set) emits "activate".
        let app = self.app.clone();
        let toggle = self.actions.toggle;
        obj.connect_local("activate", false, move |_| {
            toggle(&app);
            None
        });

        Some(Icon {
            obj,
            _menu: menu,
            current: None,
        })
    }

    unsafe fn update(&mut self, item: &BarItem) {
        let icon_path = item.icon.and_then(|bytes| self.icon_file(bytes));
        let Some((_, icon)) = self.icons.iter_mut().find(|(id, _)| *id == item.id) else {
            return;
        };
        if icon.current.as_ref() == Some(item) {
            return;
        }
        let raw = icon.obj.as_ptr() as *mut c_void;
        let empty = CString::default();
        // XApp takes an icon name or an absolute file path; empty hides it.
        (self.api.set_icon_name)(raw, icon_path.as_ref().unwrap_or(&empty).as_ptr());
        if let Ok(label) = CString::new(item.label.as_str()) {
            (self.api.set_label)(raw, label.as_ptr());
        }
        if let Ok(tooltip) = CString::new(item.tooltip.as_str()) {
            (self.api.set_tooltip)(raw, tooltip.as_ptr());
        }
        icon.current = Some(item.clone());
    }

    /// Writes an embedded PNG to a temp file once and returns its path.
    fn icon_file(&mut self, bytes: &'static [u8]) -> Option<CString> {
        let key = bytes.as_ptr() as usize;
        if let Some(path) = self.icon_files.get(&key) {
            return Some(path.clone());
        }
        let path: PathBuf = std::env::temp_dir().join(format!(
            "ia-usage-{}-{}.png",
            std::process::id(),
            self.icon_files.len()
        ));
        std::fs::write(&path, bytes).ok()?;
        let path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
        self.icon_files.insert(key, path.clone());
        Some(path)
    }
}
