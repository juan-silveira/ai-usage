//! Native Cinnamon / XApp status icon, used instead of the regular tray icon
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
use std::ffi::{c_char, c_int, c_void, CString};

use gtk::glib::translate::{FromGlibPtrFull, ToGlibPtr};
use gtk::glib::{self, ObjectExt};
use gtk::prelude::*;
use libloading::{Library, Symbol};
use tauri::AppHandle;

type NewFn = unsafe extern "C" fn() -> *mut c_void;
type SetStrFn = unsafe extern "C" fn(*mut c_void, *const c_char);
type SetMenuFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type AnyMonitorsFn = unsafe extern "C" fn() -> c_int;

struct XAppTray {
    // Keeps libxapp loaded for as long as the icon exists.
    _lib: Library,
    icon: glib::Object,
    _menu: gtk::Menu,
    set_label: SetStrFn,
    set_tooltip: SetStrFn,
}

thread_local! {
    static TRAY: RefCell<Option<XAppTray>> = const { RefCell::new(None) };
}

pub struct Actions {
    pub toggle: fn(&AppHandle),
    pub refresh: fn(&AppHandle),
}

/// Creates the XApp icon if an XApp status applet is listening. Returns false
/// when the caller should fall back to the regular tray icon.
pub fn try_init(app: &AppHandle, icon_png: &[u8], actions: Actions) -> bool {
    match unsafe { init(app, icon_png, actions) } {
        Ok(tray) => {
            TRAY.with(|t| *t.borrow_mut() = Some(tray));
            true
        }
        Err(_) => false,
    }
}

unsafe fn init(
    app: &AppHandle,
    icon_png: &[u8],
    actions: Actions,
) -> Result<XAppTray, Box<dyn std::error::Error>> {
    let lib = Library::new("libxapp.so.1")?;
    let any_monitors: Symbol<AnyMonitorsFn> = lib.get(b"xapp_status_icon_any_monitors\0")?;
    if any_monitors() == 0 {
        return Err("no XApp status applet running".into());
    }
    let new: Symbol<NewFn> = lib.get(b"xapp_status_icon_new\0")?;
    let set_icon_name: Symbol<SetStrFn> = lib.get(b"xapp_status_icon_set_icon_name\0")?;
    let set_name: Symbol<SetStrFn> = lib.get(b"xapp_status_icon_set_name\0")?;
    let set_label: SetStrFn = *lib.get(b"xapp_status_icon_set_label\0")?;
    let set_tooltip: SetStrFn = *lib.get(b"xapp_status_icon_set_tooltip_text\0")?;
    let set_secondary_menu: Symbol<SetMenuFn> =
        lib.get(b"xapp_status_icon_set_secondary_menu\0")?;

    let raw = new();
    if raw.is_null() {
        return Err("xapp_status_icon_new failed".into());
    }
    let icon = glib::Object::from_glib_full(raw as *mut glib::gobject_ffi::GObject);

    // XApp takes an icon name or an absolute file path.
    let icon_path = std::env::temp_dir().join(format!("ia-usage-{}.png", std::process::id()));
    std::fs::write(&icon_path, icon_png)?;
    let path = CString::new(icon_path.to_string_lossy().as_bytes())?;
    set_icon_name(raw, path.as_ptr());
    set_name(raw, c"ia-usage".as_ptr());

    let menu = gtk::Menu::new();
    for (label, action) in [
        ("Mostrar / ocultar", actions.toggle),
        ("Atualizar agora", actions.refresh),
        ("Sair", |app: &AppHandle| app.exit(0)),
    ] {
        let item = gtk::MenuItem::with_label(label);
        let app = app.clone();
        item.connect_activate(move |_| action(&app));
        menu.append(&item);
    }
    menu.show_all();
    let menu_ptr: *mut gtk::ffi::GtkMenu = menu.to_glib_none().0;
    set_secondary_menu(raw, menu_ptr as *mut c_void);

    // Left click (no primary menu set) emits "activate".
    let app = app.clone();
    let toggle = actions.toggle;
    icon.connect_local("activate", false, move |_| {
        toggle(&app);
        None
    });

    Ok(XAppTray {
        _lib: lib,
        icon,
        _menu: menu,
        set_label,
        set_tooltip,
    })
}

/// Returns false if no XApp icon is active.
pub fn update(label: &str, tooltip: &str) -> bool {
    TRAY.with(|t| {
        let Some(tray) = t
            .borrow()
            .as_ref()
            .map(|t| (t.icon.as_ptr(), t.set_label, t.set_tooltip))
        else {
            return false;
        };
        let (raw, set_label, set_tooltip) = tray;
        let (Ok(label), Ok(tooltip)) = (CString::new(label), CString::new(tooltip)) else {
            return true;
        };
        unsafe {
            set_label(raw as *mut c_void, label.as_ptr());
            set_tooltip(raw as *mut c_void, tooltip.as_ptr());
        }
        true
    })
}
