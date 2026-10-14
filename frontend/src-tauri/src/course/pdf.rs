//! PDF через PDFium: текст страниц и PNG страниц без текста.

use std::path::PathBuf;
use std::sync::Mutex;

use pdfium_render::prelude::*;

static RESOURCE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Папка ресурсов приложения; задаётся в `setup`.
pub fn set_resource_dir(dir: PathBuf) {
    *RESOURCE_DIR.lock().unwrap_or_else(|p| p.into_inner()) = Some(dir);
}

pub fn library_path() -> PathBuf {
    let base = RESOURCE_DIR
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        // Тесты и dev без `setup`: DLL кладёт build.rs в исходники.
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    base.join("resources").join("pdfium").join("pdfium.dll")
}

/// Привязки PDFium глобальные (once_cell в 0.9): второй `bind_to_library` отвечает
/// `AlreadyInitialized` — тогда достаточно лёгкой ручки `Pdfium::default()`.
pub fn open() -> Result<Pdfium, String> {
    match Pdfium::bind_to_library(library_path()) {
        Ok(bindings) => Ok(Pdfium::new(bindings)),
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Ok(Pdfium::default()),
        Err(error) => Err(format!("Не удалось загрузить PDFium: {error}")),
    }
}

#[cfg(all(test, windows))]
pub(crate) mod tests {
    use super::*;

    /// PDF с текстом шрифтом Arial (кириллица) — генерируется в тесте, без файлов-фикстур.
    pub(crate) fn pdf_with_text(lines: &[&str]) -> Vec<u8> {
        let pdfium = open().unwrap();
        let mut document = pdfium.create_new_pdf().unwrap();
        let font = document.fonts_mut().load_true_type_from_file("C:\\Windows\\Fonts\\arial.ttf", true).unwrap();
        for text in lines {
            let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).unwrap();
            page.objects_mut()
                .create_text_object(PdfPoints::new(50.0), PdfPoints::new(700.0), *text, font, PdfPoints::new(14.0))
                .unwrap();
        }
        document.save_to_bytes().unwrap()
    }

    #[test]
    fn pdfium_reads_back_cyrillic() {
        let bytes = pdf_with_text(&["Третья нормальная форма"]);
        let pdfium = open().unwrap();
        let document = pdfium.load_pdf_from_byte_slice(&bytes, None).unwrap();
        let text = document.pages().get(0).unwrap().text().unwrap().all();
        assert!(text.contains("Третья нормальная форма"), "{text:?}");
    }
}
