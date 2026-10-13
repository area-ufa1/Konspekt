//! Окно подсказки (метка `hint`): карточка поверх всех окон. Показывается без
//! активации — фокус остаётся в звонке или чате, где печатает владелец.
//! Место, куда владелец перетащил окно, сохраняется в `hint-window.json`
//! (tauri-plugin-store) и переживает перезапуск. Если сохранённого места нет
//! или оно ушло за все подключённые мониторы — справа по центру основного.

use tauri::{AppHandle, Manager, PhysicalPosition, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_store::StoreExt;

pub const HINT_LABEL: &str = "hint";
const WIDTH: f64 = 380.0;
const START_HEIGHT: f64 = 200.0;
const MIN_HEIGHT: f64 = 120.0;
const MAX_HEIGHT: f64 = 560.0;
const EDGE: f64 = 24.0;
const POSITION_STORE: &str = "hint-window.json";
const POSITION_KEY: &str = "position";
/// Сколько окна (в физических пикселях) должно остаться на мониторе, чтобы
/// сохранённое место считалось годным: верхнюю строку должно быть за что взять.
const MIN_VISIBLE_W: i64 = 64;
const MIN_VISIBLE_H: i64 = 32;

/// Прямоугольник в физических пикселях виртуального экрана.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

/// Где поставить окно размера `size`: на сохранённое место, если его верхняя
/// строка видна на каком-то мониторе, иначе справа по центру `primary` (или
/// первого монитора). Всё в физических пикселях. `None` — мониторов не знаем.
fn placement(saved: Option<(i32, i32)>, size: (u32, u32), monitors: &[Rect], primary: Option<Rect>, edge: i32) -> Option<(i32, i32)> {
    let (w, h) = (size.0 as i64, size.1 as i64);
    if let Some((x, y)) = saved {
        let (sx, sy) = (x as i64, y as i64);
        let reachable = monitors.iter().any(|m| {
            let (mx, my, mw, mh) = (m.x as i64, m.y as i64, m.w as i64, m.h as i64);
            let overlap_w = (sx + w).min(mx + mw) - sx.max(mx);
            // Верхний край окна внутри монитора, и под ним хватает места на строку.
            overlap_w >= MIN_VISIBLE_W.min(w) && sy >= my && sy + MIN_VISIBLE_H.min(h) <= my + mh
        });
        if reachable {
            return Some((x, y));
        }
    }
    let m = primary.or_else(|| monitors.first().copied())?;
    let (mx, my) = (m.x as i64, m.y as i64);
    let x = (mx + m.w as i64 - w - edge as i64).max(mx);
    let y = (my + (m.h as i64 - h) / 2).max(my);
    Some((x as i32, y as i32))
}

fn monitor_rect(monitor: &tauri::Monitor) -> Rect {
    let (position, size) = (monitor.position(), monitor.size());
    Rect { x: position.x, y: position.y, w: size.width, h: size.height }
}

fn saved_position<R: Runtime>(app: &AppHandle<R>) -> Option<(i32, i32)> {
    let value = app.store(POSITION_STORE).ok()?.get(POSITION_KEY)?;
    Some((value.get("x")?.as_i64()? as i32, value.get("y")?.as_i64()? as i32))
}

/// Пишет место в store. Store сбрасывает файл на диск через 100 мс после
/// последнего изменения (auto_save по умолчанию, каждый `set` перезапускает
/// таймер), так что поток Moved при перетаскивании — одна запись после отпускания.
fn remember_position<R: Runtime>(app: &AppHandle<R>, position: PhysicalPosition<i32>) {
    match app.store(POSITION_STORE) {
        Ok(store) => store.set(POSITION_KEY, serde_json::json!({ "x": position.x, "y": position.y })),
        Err(error) => log::warn!("Место окна подсказки не сохранилось: {error}"),
    }
}

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
    // Место храним и ставим в физических пикселях: так его отдают Moved и
    // мониторы, а логические координаты при мониторах с разным масштабом
    // неоднозначны (у каждого свой scale factor).
    let monitors: Vec<Rect> = window.available_monitors().unwrap_or_default().iter().map(monitor_rect).collect();
    let primary = window.primary_monitor().ok().flatten();
    let scale = primary.as_ref().map(|m| m.scale_factor()).unwrap_or(1.0);
    let size = window
        .outer_size()
        .map(|s| (s.width, s.height))
        .unwrap_or(((WIDTH * scale) as u32, (START_HEIGHT * scale) as u32));
    let saved = saved_position(app);
    if let Some((x, y)) = placement(saved, size, &monitors, primary.as_ref().map(monitor_rect), (EDGE * scale) as i32) {
        if saved.is_some_and(|s| s != (x, y)) {
            log::info!("Окно подсказки: сохранённое место {saved:?} вне мониторов, ставлю в ({x}, {y})");
        }
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    // Запоминаем только то, что сделал владелец: пока окно скрыто, Moved — это
    // наша собственная расстановка выше, её не сохраняем.
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(position) = event {
            let visible = handle.get_webview_window(HINT_LABEL).and_then(|w| w.is_visible().ok()).unwrap_or(false);
            if visible {
                remember_position(&handle, *position);
            }
        }
    });
    Ok(window)
}

/// ✕/Esc в окне: прячет его, и дельты текущей подсказки окно уже не открывают.
#[tauri::command]
pub fn hint_window_hide<R: Runtime>(app: AppHandle<R>) {
    app.state::<crate::hints::service::HintService>().dismiss_current();
    hide(&app);
}

#[tauri::command]
pub fn hint_window_fit<R: Runtime>(app: AppHandle<R>, height: f64) {
    if let Some(window) = app.get_webview_window(HINT_LABEL) {
        let height = height.clamp(MIN_HEIGHT, MAX_HEIGHT);
        let _ = window.set_size(tauri::LogicalSize::new(WIDTH, height));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };
    const LEFT: Rect = Rect { x: -1280, y: 0, w: 1280, h: 1024 };
    const SIZE: (u32, u32) = (380, 200);
    const FALLBACK: Option<(i32, i32)> = Some((1920 - 380 - 24, (1080 - 200) / 2));

    #[test]
    fn no_saved_position_goes_right_center_of_primary() {
        assert_eq!(placement(None, SIZE, &[PRIMARY], Some(PRIMARY), 24), FALLBACK);
    }

    #[test]
    fn saved_position_on_any_monitor_is_used() {
        let monitors = [PRIMARY, LEFT];
        assert_eq!(placement(Some((100, 50)), SIZE, &monitors, Some(PRIMARY), 24), Some((100, 50)));
        assert_eq!(placement(Some((-900, 300)), SIZE, &monitors, Some(PRIMARY), 24), Some((-900, 300)));
    }

    #[test]
    fn partly_off_screen_with_reachable_header_is_used() {
        assert_eq!(placement(Some((1800, 1000)), SIZE, &[PRIMARY], Some(PRIMARY), 24), Some((1800, 1000)));
    }

    #[test]
    fn saved_on_unplugged_monitor_falls_back() {
        assert_eq!(placement(Some((-900, 300)), SIZE, &[PRIMARY], Some(PRIMARY), 24), FALLBACK);
    }

    #[test]
    fn unreachable_header_falls_back() {
        // Верхний край выше экрана.
        assert_eq!(placement(Some((100, -50)), SIZE, &[PRIMARY], Some(PRIMARY), 24), FALLBACK);
        // Справа видно меньше 64 px.
        assert_eq!(placement(Some((1880, 100)), SIZE, &[PRIMARY], Some(PRIMARY), 24), FALLBACK);
        // Ниже экрана после смены разрешения.
        assert_eq!(placement(Some((100, 1400)), SIZE, &[PRIMARY], Some(PRIMARY), 24), FALLBACK);
    }

    #[test]
    fn fallback_uses_primary_offset_or_first_monitor() {
        let shifted = Rect { x: 1920, y: 200, w: 2560, h: 1440 };
        assert_eq!(placement(None, SIZE, &[shifted], Some(shifted), 36), Some((1920 + 2560 - 380 - 36, 200 + 620)));
        assert_eq!(placement(None, SIZE, &[shifted], None, 36), Some((1920 + 2560 - 380 - 36, 200 + 620)));
        assert_eq!(placement(Some((5, 5)), SIZE, &[], None, 24), None);
    }
}
