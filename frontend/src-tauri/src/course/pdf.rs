//! PDF через PDFium: текст страниц и PNG страниц без текста.

use std::path::PathBuf;
use std::sync::Mutex;

use pdfium_render::prelude::*;

use super::{quality, PageOut, IMAGE_LONG_SIDE, MAX_PAGES};

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
    let library = library_path();
    match Pdfium::bind_to_library(&library) {
        Ok(bindings) => Ok(Pdfium::new(bindings)),
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Ok(Pdfium::default()),
        Err(error) => Err(library_failure(&error.to_string(), &library)),
    }
}

/// Причина для колонки `error` (открытая, без путей) — общая; подробность — только в лог,
/// и путь к DLL (в нём профиль пользователя) вырезан и оттуда.
const PDF_OPEN_FAILED: &str = "Не удалось открыть PDF";

fn library_failure(detail: &str, library: &std::path::Path) -> String {
    log::warn!("PDFium не загрузился: {}", crate::claude_cli::error_for_log(&without_paths(detail, library)));
    PDF_OPEN_FAILED.to_string()
}

/// Подробность ошибки без пути к DLL и к её папке.
fn without_paths(detail: &str, library: &std::path::Path) -> String {
    let mut detail = detail.to_string();
    for path in [Some(library), library.parent()].into_iter().flatten() {
        let text = path.to_string_lossy();
        if !text.is_empty() {
            detail = detail.replace(text.as_ref(), "<pdfium>");
        }
    }
    detail
}

pub fn extract(bytes: &[u8]) -> Result<Vec<PageOut>, String> {
    let pdfium = open()?;
    let document = pdfium.load_pdf_from_byte_slice(bytes, None).map_err(|error| match error {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            "PDF защищён паролем — снимите пароль и добавьте снова".to_string()
        }
        other => format!("Не удалось открыть PDF: {other}"),
    })?;
    let count = document.pages().len() as usize;
    if count > MAX_PAGES {
        return Err(format!("В PDF {count} страниц — больше {MAX_PAGES}. Разделите файл."));
    }
    let mut out = Vec::with_capacity(count);
    for (index, page) in document.pages().iter().enumerate() {
        let text = page.text().map(|t| t.all()).unwrap_or_default();
        let image_png = if quality::needs_reading(&text) { Some(render_png(&page)?) } else { None };
        out.push(PageOut { number: index as u32 + 1, text: text.trim().to_string(), image_png });
    }
    Ok(out)
}

fn render_png(page: &PdfPage) -> Result<Vec<u8>, String> {
    let (w, h) = (page.width().value, page.height().value);
    let config = if w >= h {
        PdfRenderConfig::new().set_target_width(IMAGE_LONG_SIDE as i32)
    } else {
        PdfRenderConfig::new().set_target_height(IMAGE_LONG_SIDE as i32)
    };
    let image = page
        .render_with_config(&config)
        .map_err(|error| format!("Не удалось отрисовать страницу PDF: {error}"))?
        .as_image()
        .map_err(|error| format!("Не удалось отрисовать страницу PDF: {error}"))?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("Не удалось сохранить страницу картинкой: {error}"))?;
    Ok(png)
}

#[cfg(test)]
mod library_tests {
    use super::*;

    #[test]
    fn a_library_failure_is_the_spec_wording_and_the_log_has_no_path() {
        let library = PathBuf::from("C:\\Users\\Иван\\AppData\\Konspekt\\resources\\pdfium\\pdfium.dll");
        let detail = format!("LoadLibraryExW failed for {}: module not found", library.display());
        assert_eq!(library_failure(&detail, &library), "Не удалось открыть PDF");
        let scrubbed = without_paths(&detail, &library);
        assert!(!scrubbed.contains("Иван"), "{scrubbed}");
        assert!(scrubbed.contains("module not found"));
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

    /// PDF: страница 1 — текст, страница 2 — только картинка (как скан).
    pub(crate) fn pdf_text_then_scan() -> Vec<u8> {
        let pdfium = open().unwrap();
        let mut document = pdfium.create_new_pdf().unwrap();
        let font = document.fonts_mut().load_true_type_from_file("C:\\Windows\\Fonts\\arial.ttf", true).unwrap();
        let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).unwrap();
        let text = "Вторая нормальная форма: каждый неключевой атрибут полностью зависит от первичного ключа отношения.";
        page.objects_mut().create_text_object(PdfPoints::new(30.0), PdfPoints::new(700.0), text, font, PdfPoints::new(9.0)).unwrap();
        let mut scan = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).unwrap();
        let picture = image::DynamicImage::new_rgb8(400, 300);
        scan.objects_mut()
            .create_image_object(PdfPoints::new(50.0), PdfPoints::new(300.0), &picture, Some(PdfPoints::new(400.0)), Some(PdfPoints::new(300.0)))
            .unwrap();
        document.save_to_bytes().unwrap()
    }

    #[test]
    fn text_pages_keep_text_and_scans_get_a_png() {
        let pages = extract(&pdf_text_then_scan()).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].number, 1);
        assert!(pages[0].text.contains("Вторая нормальная форма"));
        assert_eq!(pages[0].image_png, None);
        let png = pages[1].image_png.as_ref().expect("скан → картинка");
        assert_eq!(&png[1..4], b"PNG");
        let decoded = image::load_from_memory(png).unwrap();
        let long = decoded.width().max(decoded.height());
        assert!((crate::course::IMAGE_LONG_SIDE - 1..=crate::course::IMAGE_LONG_SIDE).contains(&long), "{long}");
    }

    #[test]
    fn broken_pdf_is_an_error_not_a_panic() {
        let error = extract(b"%PDF-1.7 not really").unwrap_err();
        assert!(error.contains("PDF"), "{error}");
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
