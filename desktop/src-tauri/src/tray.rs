//! 托盘常驻 + 全局快捷键。
//!
//! - 托盘：应用常驻菜单栏，关掉窗口不退出；菜单里能重新打开、去扫描、退出。
//! - 全局快捷键：默认 `Cmd/Ctrl + Shift + C` 唤起主窗口；
//!   `Cmd/Ctrl + Shift + F` 打开搜索框。
//!
//! 快捷键注册失败（被别的应用占用、系统不支持）**不能把应用搞崩**，
//! 失败就降级成「只能用托盘/窗口」，并在日志里说明。

use std::str::FromStr;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime, Window,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

/// 全局快捷键的默认绑定
pub const TOGGLE_WINDOW: &str = "CmdOrCtrl+Shift+C";
pub const FOCUS_SEARCH: &str = "CmdOrCtrl+Shift+F";

/// 显示主窗口（已经有就聚焦，没有就新建）
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 让前端跳到某个页面（托盘菜单 / 快捷键共用）
pub fn goto<R: Runtime>(app: &AppHandle<R>, page: &str) {
    show_main(app);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.eval(format!("window.__goto && window.__goto('{page}')"));
    }
}

/// 在窗口里聚焦搜索框
pub fn focus_search<R: Runtime>(app: &AppHandle<R>) {
    show_main(app);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.eval("window.__focusSearch && window.__focusSearch()");
    }
}

/// 建托盘图标 + 菜单
pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let show_i = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let dict_i = MenuItem::with_id(app, "dict", "配置字典", true, None::<&str>)?;
    let scan_i = MenuItem::with_id(app, "scan", "扫描磁盘", true, None::<&str>)?;
    let edit_i = MenuItem::with_id(app, "edit", "编辑器", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "退出 AI 配置字典", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&show_i, &dict_i, &scan_i, &edit_i, &quit_i])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("AI CLI 配置字典")
        .menu(&menu)
        // 左键点图标直接唤起窗口（macOS 习惯）
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "dict" => goto(app, "dict"),
            "scan" => goto(app, "scan"),
            "edit" => goto(app, "edit"),
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 注册全局快捷键
pub fn register_shortcuts<R: Runtime>(app: &AppHandle<R>) {
    let handler = |app: &AppHandle<R>, _sc: &Shortcut, event: ShortcutEvent| {
        if event.state() == ShortcutState::Pressed {
            focus_search(app);
        }
    };
    let toggle = match Shortcut::from_str(TOGGLE_WINDOW) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("全局快捷键解析失败（{TOGGLE_WINDOW}）：{e}");
            return;
        }
    };
    let search = match Shortcut::from_str(FOCUS_SEARCH) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("全局快捷键解析失败（{FOCUS_SEARCH}）：{e}");
            return;
        }
    };

    let gs = app.global_shortcut();
    // 唤起窗口：Windows/Linux 上 CmdOrCtrl 就是 Ctrl
    if let Err(e) = gs.on_shortcut(toggle, |app, _sc, event| {
        if event.state() == ShortcutState::Pressed {
            show_main(app);
        }
    }) {
        eprintln!("注册全局快捷键 {TOGGLE_WINDOW} 失败：{e}");
    }
    if let Err(e) = gs.on_shortcut(search, handler) {
        eprintln!("注册全局快捷键 {FOCUS_SEARCH} 失败：{e}");
    }
}

/// 窗口关闭时藏到托盘，而不是退出
pub fn handle_window_event<R: Runtime>(window: &Window<R>, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.hide();
        #[cfg(target_os = "macos")]
        {
            // macOS 上藏起窗口后，dock 图标还在；这里不主动退到后台，
            // 以免用户找不到托盘图标。
        }
    }
}
