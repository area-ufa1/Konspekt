//! Окно подсказки (метка `hint`): карточка поверх всех окон. Показывается без
//! активации — фокус остаётся в звонке или чате, где печатает владелец.
//! Позиция — где оставили в этом запуске; первый раз — справа по центру.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, PhysicalPosition, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub const HINT_LABEL: &str = "hint";
const WIDTH: f64 = 380.0;
const START_HEIGHT: f64 = 200.0;
const MIN_HEIGHT: f64 = 120.0;
const MAX_HEIGHT: f64 = 560.0;
const EDGE: f64 = 24.0;

static LAST_POSITION: Mutex<Option<PhysicalPosition<i32>>> = Mutex::new(None);

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
    if let Err(error) = show_without_focus(&window) {
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
    let remembered = *LAST_POSITION.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(position) = remembered {
        let _ = window.set_position(position);
    } else if let Ok(Some(monitor)) = window.primary_monitor() {
        let scale = monitor.scale_factor();
        let size = monitor.size();
        let x = size.width as f64 / scale - WIDTH - EDGE;
        let y = (size.height as f64 / scale - START_HEIGHT) / 2.0;
        let _ = window.set_position(tauri::LogicalPosition::new(x.max(0.0), y.max(0.0)));
    }
    window.on_window_event(|event| {
        if let WindowEvent::Moved(position) = event {
            *LAST_POSITION.lock().unwrap_or_else(|p| p.into_inner()) = Some(*position);
        }
    });
    Ok(window)
}

#[cfg(windows)]
fn show_without_focus<R: Runtime>(window: &tauri::WebviewWindow<R>) -> tauri::Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
    let hwnd = window.hwnd()?;
    // SAFETY: hwnd — живое окно Tauri этого процесса.
    unsafe { ShowWindow(hwnd.0 as _, SW_SHOWNOACTIVATE) };
    window.set_always_on_top(true)
}

#[cfg(not(windows))]
fn show_without_focus<R: Runtime>(window: &tauri::WebviewWindow<R>) -> tauri::Result<()> {
    window.show()
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
