#![recursion_limit = "256"]

mod cats;
mod config;
mod input_hook;
mod mcp;
mod tray;
mod updater;

use cats::{all_cats, cat_exists, cats_path, sanitize_cat_id, CatSkinConfig};
use config::{config_path, load_settings, save_settings, set_windows_autostart, AppSettings};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Size, State};

pub const BASE_WIDTH: f64 = 420.0;
pub const BASE_HEIGHT: f64 = 440.0;

pub fn run_mcp_stdio() {
    mcp::run_stdio_mcp();
}

pub struct AppState {
    pub settings: Arc<Mutex<AppSettings>>,
    pub menu_open: Arc<AtomicBool>,
    pub dragging: Arc<AtomicBool>,
    pub mcp_enabled: Arc<AtomicBool>,
    pub update_banner_open: Arc<AtomicBool>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowCursorInfo {
    rel_x: f64,
    rel_y: f64,
    inside_window: bool,
    over_cat: bool,
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> AppSettings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn get_cats() -> Vec<CatSkinConfig> {
    all_cats()
}

#[tauri::command]
fn get_mcp_config(state: State<'_, AppState>) -> String {
    let port = state.settings.lock().unwrap().mcp_port;
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Desktop_Cat.exe".to_string());
    serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "kurocat": {
                "command": exe,
                "args": ["--mcp"],
                "url": format!("http://127.0.0.1:{}/mcp", port)
            }
        }
    }))
    .unwrap_or_default()
}

#[tauri::command]
fn update_settings(app: AppHandle, state: State<'_, AppState>, mut patch: AppSettings) -> AppSettings {
    patch.scale = patch.scale.clamp(0.6, 1.8);
    let clean_breed = sanitize_cat_id(&patch.cat_breed);
    patch.cat_breed = if cat_exists(&clean_breed) {
        clean_breed
    } else {
        "black".to_string()
    };
    if patch.mcp_port == 0 {
        patch.mcp_port = 27420;
    }

    let prev_autostart = state.settings.lock().unwrap().autostart;
    if patch.autostart != prev_autostart {
        let _ = set_windows_autostart(patch.autostart);
    }

    state.mcp_enabled.store(patch.mcp_enabled, Ordering::Relaxed);

    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_always_on_top(patch.always_on_top);
        let w = (BASE_WIDTH * patch.scale).round();
        let h = (BASE_HEIGHT * patch.scale).round();
        let _ = win.set_size(Size::Logical(LogicalSize::new(w, h)));
    }

    {
        let mut s = state.settings.lock().unwrap();
        *s = patch.clone();
        save_settings(&s);
    }
    let _ = app.emit("kuro-settings", patch.clone());
    patch
}

#[tauri::command]
fn set_menu_open(state: State<'_, AppState>, open: bool) {
    state.menu_open.store(open, Ordering::Relaxed);
}

#[tauri::command]
fn set_dragging(state: State<'_, AppState>, dragging: bool) {
    state.dragging.store(dragging, Ordering::Relaxed);
}

#[tauri::command]
fn set_update_banner_open(state: State<'_, AppState>, open: bool) {
    state.update_banner_open.store(open, Ordering::Relaxed);
}

#[tauri::command]
fn get_app_version() -> String {
    updater::CURRENT_VERSION.to_string()
}

#[tauri::command]
async fn check_for_update() -> Result<updater::UpdateInfo, String> {
    tauri::async_runtime::spawn_blocking(updater::check_latest_update)
        .await
        .map_err(|e| format!("检查更新任务异常: {}", e))?
}

#[tauri::command]
async fn install_update(app: AppHandle, info: updater::UpdateInfo) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || updater::perform_update_install(&app, info))
        .await
        .map_err(|e| format!("安装更新任务异常: {}", e))?
}

#[tauri::command]
fn open_release_page(url: String) {
    updater::open_url_in_browser(&url);
}

#[tauri::command]
fn save_window_pos(app: AppHandle, state: State<'_, AppState>) {
    if let Some(win) = app.get_webview_window("main") {
        if let Ok(pos) = win.outer_position() {
            let mut s = state.settings.lock().unwrap();
            s.x = Some(pos.x);
            s.y = Some(pos.y);
            save_settings(&s);
        }
    }
}

#[tauri::command]
fn reset_position(app: AppHandle) {
    reset_window_position(&app);
}

#[tauri::command]
fn create_desktop_shortcut() -> Result<String, String> {
    config::create_desktop_shortcut()
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

pub fn apply_breed_and_save(app: &AppHandle, breed: &str) {
    let clean = sanitize_cat_id(breed);
    let normalized = if cat_exists(&clean) {
        clean
    } else {
        "black".to_string()
    };
    let state = app.state::<AppState>();
    let updated = {
        let mut s = state.settings.lock().unwrap();
        s.cat_breed = normalized;
        save_settings(&s);
        s.clone()
    };
    let _ = app.emit("kuro-settings", updated);
}

pub fn reload_and_emit_all(app: &AppHandle) {
    let disk_settings = load_settings();
    let cats = all_cats();
    let state = app.state::<AppState>();

    let (scale_changed, top_changed) = {
        let mut s = state.settings.lock().unwrap();
        let sc = (s.scale - disk_settings.scale).abs() > 0.01;
        let tc = s.always_on_top != disk_settings.always_on_top;
        *s = disk_settings.clone();
        (sc, tc)
    };
    state
        .mcp_enabled
        .store(disk_settings.mcp_enabled, Ordering::Relaxed);

    if let Some(win) = app.get_webview_window("main") {
        if top_changed {
            let _ = win.set_always_on_top(disk_settings.always_on_top);
        }
        if scale_changed {
            let _ = win.set_size(Size::Logical(LogicalSize::new(
                (BASE_WIDTH * disk_settings.scale).round(),
                (BASE_HEIGHT * disk_settings.scale).round(),
            )));
        }
    }

    let _ = app.emit("kuro-cats", cats);
    let _ = app.emit("kuro-settings", disk_settings);
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn start_storage_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let c_path = cats_path();
        let s_path = config_path();
        let mut last_cats_m = file_mtime(&c_path);
        let mut last_settings_m = file_mtime(&s_path);

        loop {
            std::thread::sleep(Duration::from_millis(400));
            let cur_c = file_mtime(&c_path);
            let cur_s = file_mtime(&s_path);
            if cur_c != last_cats_m || cur_s != last_settings_m {
                last_cats_m = cur_c;
                last_settings_m = cur_s;
                reload_and_emit_all(&app);
            }
        }
    });
}

pub fn apply_scale_and_save(app: &AppHandle, scale: f64) {
    let clamped = scale.clamp(0.6, 1.8);
    let state = app.state::<AppState>();
    let updated = {
        let mut s = state.settings.lock().unwrap();
        s.scale = clamped;
        save_settings(&s);
        s.clone()
    };
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_size(Size::Logical(LogicalSize::new(
            (BASE_WIDTH * clamped).round(),
            (BASE_HEIGHT * clamped).round(),
        )));
    }
    let _ = app.emit("kuro-settings", updated);
}

pub fn reset_window_position(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let size = win.outer_size().unwrap_or(tauri::PhysicalSize::new(420, 440));
        let sf = win.scale_factor().unwrap_or(1.0);
        let (target_x, target_y) = default_bottom_right_position(size.width as i32, size.height as i32, sf);
        let _ = win.set_position(PhysicalPosition::new(target_x, target_y));
        let state = app.state::<AppState>();
        let mut s = state.settings.lock().unwrap();
        s.x = Some(target_x);
        s.y = Some(target_y);
        save_settings(&s);
    }
}

#[cfg(windows)]
fn default_bottom_right_position(win_w: i32, win_h: i32, _scale_factor: f64) -> (i32, i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe {
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        ((sw - win_w - 28).max(20), (sh - win_h - 68).max(20))
    }
}

#[cfg(target_os = "macos")]
fn default_bottom_right_position(win_w: i32, win_h: i32, scale_factor: f64) -> (i32, i32) {
    let (sw_pt, sh_pt) = input_hook::macos_hook::main_display_size();
    let sw = (sw_pt * scale_factor).round() as i32;
    let sh = (sh_pt * scale_factor).round() as i32;
    let pad_x = (28.0 * scale_factor).round() as i32;
    let pad_y = (84.0 * scale_factor).round() as i32;
    ((sw - win_w - pad_x).max(20), (sh - win_h - pad_y).max(20))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn default_bottom_right_position(_win_w: i32, _win_h: i32, _scale_factor: f64) -> (i32, i32) {
    (900, 500)
}

#[cfg(windows)]
fn get_cursor_pos(_scale_factor: f64) -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) != 0 {
            Some((pt.x, pt.y))
        } else {
            None
        }
    }
}

#[cfg(target_os = "macos")]
fn get_cursor_pos(scale_factor: f64) -> Option<(i32, i32)> {
    input_hook::macos_hook::cursor_pos_points().map(|(x, y)| {
        (
            (x * scale_factor).round() as i32,
            (y * scale_factor).round() as i32,
        )
    })
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn get_cursor_pos(_scale_factor: f64) -> Option<(i32, i32)> {
    None
}

fn is_over_interactive_silhouette(rx: f64, ry: f64) -> (bool, bool) {
    // Cat body/head oval centered around (0.48, 0.49)
    let dx_cat = (rx - 0.48) / 0.25;
    let dy_cat = (ry - 0.49) / 0.27;
    let over_cat = (dx_cat * dx_cat + dy_cat * dy_cat) <= 1.0;

    // Keyboard & mouse lower area
    let over_desk = rx >= 0.06 && rx <= 0.92 && ry >= 0.65 && ry <= 0.91;

    (over_cat || over_desk, over_cat)
}

fn start_hit_test_loop(
    app: AppHandle,
    settings: Arc<Mutex<AppSettings>>,
    menu_open: Arc<AtomicBool>,
    dragging: Arc<AtomicBool>,
    update_banner_open: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut last_ignore: Option<bool> = None;
        let mut last_cursor: (i32, i32) = (i32::MIN, i32::MIN);

        loop {
            std::thread::sleep(Duration::from_millis(25));

            let Some(win) = app.get_webview_window("main") else {
                continue;
            };
            if !win.is_visible().unwrap_or(false) {
                continue;
            }
            let sf = win.scale_factor().unwrap_or(1.0);
            let Some((cx, cy)) = get_cursor_pos(sf) else {
                continue;
            };
            let Ok(pos) = win.outer_position() else {
                continue;
            };
            let Ok(size) = win.outer_size() else {
                continue;
            };
            let w = size.width.max(1) as f64;
            let h = size.height.max(1) as f64;
            let rx = (cx - pos.x) as f64 / w;
            let ry = (cy - pos.y) as f64 / h;
            let inside = rx >= 0.0 && rx <= 1.0 && ry >= 0.0 && ry <= 1.0;
            let (over_interactive, over_cat) = if inside {
                is_over_interactive_silhouette(rx, ry)
            } else {
                (false, false)
            };

            let locked = settings.lock().unwrap().click_through_locked;
            let is_menu = menu_open.load(Ordering::Relaxed);
            let is_drag = dragging.load(Ordering::Relaxed);
            let is_banner = update_banner_open.load(Ordering::Relaxed);
            let over_banner = is_banner && inside && ry <= 0.19;

            let should_ignore = if is_menu || is_drag || over_banner {
                false
            } else if locked {
                true
            } else {
                !over_interactive
            };

            if last_ignore != Some(should_ignore) {
                let _ = win.set_ignore_cursor_events(should_ignore);
                last_ignore = Some(should_ignore);
            }

            if (cx, cy) != last_cursor {
                last_cursor = (cx, cy);
                let _ = app.emit(
                    "kuro-cursor",
                    WindowCursorInfo {
                        rel_x: rx,
                        rel_y: ry,
                        inside_window: inside,
                        over_cat,
                    },
                );
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial_settings = load_settings();
    let settings_arc = Arc::new(Mutex::new(initial_settings.clone()));
    let menu_open = Arc::new(AtomicBool::new(false));
    let dragging = Arc::new(AtomicBool::new(false));
    let mcp_enabled = Arc::new(AtomicBool::new(initial_settings.mcp_enabled));
    let update_banner_open = Arc::new(AtomicBool::new(false));

    let settings_for_setup = settings_arc.clone();
    let menu_for_setup = menu_open.clone();
    let drag_for_setup = dragging.clone();
    let mcp_for_setup = mcp_enabled.clone();
    let banner_for_setup = update_banner_open.clone();

    tauri::Builder::default()
        .manage(AppState {
            settings: settings_arc,
            menu_open,
            dragging,
            mcp_enabled,
            update_banner_open,
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let s = settings_for_setup.lock().unwrap().clone();

            #[cfg(target_os = "macos")]
            {
                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_always_on_top(s.always_on_top);
                #[cfg(target_os = "macos")]
                {
                    let _ = win.set_visible_on_all_workspaces(true);
                }
                let _ = win.set_size(Size::Logical(LogicalSize::new(
                    (BASE_WIDTH * s.scale).round(),
                    (BASE_HEIGHT * s.scale).round(),
                )));
                if let (Some(x), Some(y)) = (s.x, s.y) {
                    let _ = win.set_position(PhysicalPosition::new(x, y));
                } else {
                    let sz = win.outer_size().unwrap_or(tauri::PhysicalSize::new(420, 440));
                    let sf = win.scale_factor().unwrap_or(1.0);
                    let (dx, dy) = default_bottom_right_position(sz.width as i32, sz.height as i32, sf);
                    let _ = win.set_position(PhysicalPosition::new(dx, dy));
                }
            }

            tray::setup_tray(&handle, &s)?;
            input_hook::start_global_hooks(handle.clone());
            start_hit_test_loop(
                handle.clone(),
                settings_for_setup,
                menu_for_setup,
                drag_for_setup,
                banner_for_setup,
            );
            start_storage_watcher(handle.clone());
            updater::start_background_update_checker(handle.clone());

            let http_handle = handle.clone();
            let mcp_port = if s.mcp_port == 0 { 27420 } else { s.mcp_port };
            mcp::start_http_mcp_server(mcp_port, mcp_for_setup, move || {
                reload_and_emit_all(&http_handle);
            });

            if std::env::args().any(|a| a == "--demo") {
                let demo_handle = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1200));
                    let keys: &[u32] = &[0x46, 0x53, 0x41, 0x45, 0x20, 0xA0, 0x4B];
                    for &vk in keys {
                        let _ = demo_handle.emit(
                            "kuro-input",
                            serde_json::json!({ "kind": "keyDown", "vk": vk, "scan": 0, "repeat": false }),
                        );
                        std::thread::sleep(Duration::from_millis(100));
                        let _ = demo_handle.emit(
                            "kuro-input",
                            serde_json::json!({ "kind": "keyUp", "vk": vk, "scan": 0 }),
                        );
                    }
                    let _ = demo_handle.emit(
                        "kuro-input",
                        serde_json::json!({ "kind": "mouseMove", "x": 400, "y": 400, "nx": 0.25, "ny": 0.4 }),
                    );
                    let _ = demo_handle.emit(
                        "kuro-input",
                        serde_json::json!({ "kind": "mouseDown", "button": "left" }),
                    );
                    let _ = demo_handle.emit(
                        "kuro-input",
                        serde_json::json!({ "kind": "keyDown", "vk": 0x4B, "scan": 0, "repeat": false }),
                    );
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_cats,
            get_mcp_config,
            get_app_version,
            update_settings,
            set_menu_open,
            set_dragging,
            set_update_banner_open,
            save_window_pos,
            reset_position,
            create_desktop_shortcut,
            check_for_update,
            install_update,
            open_release_page,
            quit_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Desktop_Cat");
}
