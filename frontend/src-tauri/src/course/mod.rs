//! Материалы курса (Konspekt, подпроект 3): текст файлов предмета, хранение,
//! сборка блока для Claude.

pub mod pdf;
pub mod quality;

/// Одна страница/слайд материала: текст и, если годного текста нет, картинка страницы.
#[derive(Debug, Clone, PartialEq)]
pub struct PageOut {
    pub number: u32,
    pub text: String,
    pub image_png: Option<Vec<u8>>,
}

pub const MAX_PAGES: usize = 300;
pub const IMAGE_LONG_SIDE: u32 = 1568;
