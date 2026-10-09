//! Постоянный диалог с Claude Code на время записи (Konspekt).

pub mod context;
pub mod dialog;
#[cfg(all(test, windows))]
pub(crate) mod fake_claude;
pub mod protocol;
