//! Материалы курса (Konspekt, подпроект 3): текст файлов предмета, хранение,
//! сборка блока для Claude.

pub mod office;
pub mod pdf;
pub mod quality;
pub mod store;

/// Одна страница/слайд материала: текст и, если годного текста нет, картинка страницы.
#[derive(Debug, Clone, PartialEq)]
pub struct PageOut {
    pub number: u32,
    pub text: String,
    pub image_png: Option<Vec<u8>>,
}

pub const MAX_PAGES: usize = 300;
pub const IMAGE_LONG_SIDE: u32 = 1568;

pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_UNZIPPED_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Pdf,
    Pptx,
    Docx,
    Txt,
    Md,
}

impl FileKind {
    /// Вид файла по расширению; прочие форматы (.doc/.ppt/.xls/.xlsx…) — отказ с подсказкой.
    pub fn from_name(name: &str) -> Result<FileKind, String> {
        let extension = name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase()).unwrap_or_default();
        match extension.as_str() {
            "pdf" => Ok(FileKind::Pdf),
            "pptx" => Ok(FileKind::Pptx),
            "docx" => Ok(FileKind::Docx),
            "txt" => Ok(FileKind::Txt),
            "md" => Ok(FileKind::Md),
            _ => Err("Формат не поддерживается: сохраните как PDF или DOCX/PPTX".to_string()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Pdf => "pdf",
            FileKind::Pptx => "pptx",
            FileKind::Docx => "docx",
            FileKind::Txt => "txt",
            FileKind::Md => "md",
        }
    }
}

/// Текст (и картинки страниц без текста) файла материала.
pub fn extract(kind: FileKind, bytes: &[u8]) -> Result<Vec<PageOut>, String> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("Файл больше 50 МБ".to_string());
    }
    match kind {
        FileKind::Pdf => pdf::extract(bytes),
        FileKind::Pptx => office::extract_pptx(bytes),
        FileKind::Docx => office::extract_docx(bytes),
        FileKind::Txt | FileKind::Md => Ok(office::extract_plain(bytes)),
    }
}
