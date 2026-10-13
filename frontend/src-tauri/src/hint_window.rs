//! Окно подсказки (метка `hint`): карточка поверх всех окон. Показывается без
//! активации — фокус остаётся в звонке или чате, где печатает владелец.
//! Окно только прячется, а не закрывается, поэтому до конца запуска стоит там,
//! куда его перетащили; при создании — справа по центру.

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

pub const HINT_LABEL: &str = "hint";
const WIDTH: f64 = 380.0;
const START_HEIGHT: f64 = 200.0;
const MIN_HEIGHT: f64 = 120.0;
const MAX_HEIGHT: f64 = 560.0;
const EDGE: f64 = 24.0;

pub fn show<R: Runtime>(app: &AppHandle<R>) {
    let window = match app.get_webview_window(HINT_LABEL) {
        Some(window) => window,
        None => match create(app) {
            Ok(window) => window,
            Err(error) => {
                log::warn!("Окно подсказки не создалось: {error}");
                return;
            }
        },
    };
    if window.is_visible().unwrap_or(false) {
        return;
    }
    // Без активации: окно собрано с `.focused(false)`, tao запоминает это
    // (MARKER_DONT_FOCUS) и каждое `show()` делает через SW_SHOWNOACTIVATE.
    // Именно `show()`, а не ShowWindow напрямую: иначе tao считает окно
    // скрытым, и `hide()` ничего не делает.
    if let Err(error) = window.show() {
        log::warn!("Окно подсказки не показалось: {error}");
    }
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(HINT_LABEL) {
        let _ = window.hide();
    }
}

fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<tauri::WebviewWindow<R>> {
    let window = WebviewWindowBuilder::new(app, HINT_LABEL, WebviewUrl::App("hint".into()))
        .title("Подсказка")
        .inner_size(WIDTH, START_HEIGHT)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .focused(false)
        .visible(false)
        .build()?;
    if let Ok(Some(monitor)) = window.primary_monitor() {
        let scale = monitor.scale_factor();
        let size = monitor.size();
        let x = size.width as f64 / scale - WIDTH - EDGE;
        let y = (size.height as f64 / scale - START_HEIGHT) / 2.0;
        let _ = window.set_position(tauri::LogicalPosition::new(x.max(0.0), y.max(0.0)));
    }
    Ok(window)
}

#[tauri::command]
pub fn hint_window_hide<R: Runtime>(app: AppHandle<R>) {
    hide(&app);
}

#[tauri::command]
pub fn hint_window_fit<R: Runtime>(app: AppHandle<R>, height: f64) {
    if let Some(window) = app.get_webview_window(HINT_LABEL) {
        let height = height.clamp(MIN_HEIGHT, MAX_HEIGHT);
        let _ = window.set_size(tauri::LogicalSize::new(WIDTH, height));
    }
}
