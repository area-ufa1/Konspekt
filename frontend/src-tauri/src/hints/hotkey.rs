//! Горячая клавиша «ответить на последний вопрос»: регистрируется на время
//! записи с включёнными подсказками, снимается с её концом.

use tauri::{AppHandle, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use super::service::Request;

pub fn register<R: Runtime>(app: &AppHandle<R>, text: &str) -> Result<(), String> {
    let shortcut = super::parse_hotkey(text)?;
    unregister_all(app);
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                let at = crate::audio::recording_commands::recorded_seconds().unwrap_or(0.0);
                super::commands::submit(app, Request::Hotkey { at });
            }
        })
        .map_err(|error| format!("Сочетание {} занято другой программой или не поддерживается: {error}", text.trim()))
}

pub fn unregister_all<R: Runtime>(app: &AppHandle<R>) {
    if let Err(error) = app.global_shortcut().unregister_all() {
        log::warn!("Горячая клавиша подсказок не снялась: {error}");
    }
}
