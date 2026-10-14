//! Текст из PPTX, DOCX, TXT и MD. Разбор XML — событиями `quick_xml`, без DOM.

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{quality, PageOut, IMAGE_LONG_SIDE, MAX_PAGES, MAX_UNZIPPED_BYTES};

/// Картинка слайда больше этого числа пикселей не декодируется (защита от «картинки-бомбы»).
const IMAGE_MAX_PIXELS: u64 = 40_000_000;

type Archive<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

fn read_zip(bytes: &[u8]) -> Result<Archive<'_>, String> {
    zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Не удалось открыть файл: это не PPTX/DOCX".to_string())
}

fn is_safe_name(name: &str) -> bool {
    !name.starts_with('/') && !name.starts_with('\\') && !name.split(['/', '\\']).any(|part| part == "..")
}

/// Читает запись архива, списывая распакованный размер с общего бюджета.
/// Заявленному в заголовке размеру не верим: читаем не больше `budget + 1` байт.
fn read_entry(archive: &mut Archive<'_>, name: &str, budget: &mut u64) -> Result<Option<Vec<u8>>, String> {
    if !is_safe_name(name) {
        return Ok(None);
    }
    let Ok(file) = archive.by_name(name) else {
        return Ok(None);
    };
    let mut data = Vec::new();
    file.take(budget.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|_| "Не удалось прочитать файл: он повреждён".to_string())?;
    let len = data.len() as u64;
    if len > *budget {
        return Err("Файл слишком большой после распаковки".to_string());
    }
    *budget -= len;
    Ok(Some(data))
}

/// Путь `target` относительно каталога `base` (`ppt/slides`) без `..`; выход за корень — `None`.
fn resolve(base: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').filter(|p| !p.is_empty()).collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn text_of(t: &quick_xml::events::BytesText<'_>) -> String {
    match t.unescape() {
        Ok(text) => text.into_owned(),
        Err(_) => String::from_utf8_lossy(t).into_owned(), // неизвестная сущность вроде &nbsp;
    }
}

#[derive(Default)]
struct Parsed {
    /// Непустые строки: абзацы и строки таблиц.
    lines: Vec<String>,
    /// `r:embed` из `a:blip` по порядку.
    blips: Vec<String>,
}

fn flush(para: &mut String, cell: &mut Option<Vec<String>>, lines: &mut Vec<String>) {
    let line = para.trim().to_string();
    para.clear();
    if line.is_empty() {
        return;
    }
    match cell {
        Some(parts) => parts.push(line),
        None => lines.push(line),
    }
}

/// Общий разбор слайда и документа: `p` — абзац, `t` — текст, `tbl/tr/tc` — таблица.
/// Названия у PPTX (`a:`) и DOCX (`w:`) совпадают, поэтому смотрим на локальное имя.
fn parse_text(xml: &[u8]) -> Result<Parsed, String> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Parsed::default();
    let (mut in_t, mut p_depth, mut tbl_depth) = (false, 0usize, 0usize);
    let mut para = String::new();
    let mut cell: Option<Vec<String>> = None;
    let mut row: Option<Vec<String>> = None;

    loop {
        match reader.read_event_into(&mut buf).map_err(|_| "Не удалось разобрать содержимое файла".to_string())? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"p" => p_depth += 1,
                b"t" => in_t = true,
                b"tbl" => tbl_depth += 1,
                b"tr" if tbl_depth == 1 => row = Some(Vec::new()),
                b"tc" if tbl_depth == 1 => cell = Some(Vec::new()),
                _ => {}
            },
            Event::Empty(e) => {
                if e.local_name().as_ref() == b"blip" {
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"embed" {
                            if let Ok(id) = attr.unescape_value() {
                                out.blips.push(id.into_owned());
                            }
                        }
                    }
                }
            }
            Event::Text(t) if in_t => para.push_str(&text_of(&t)),
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => {
                    in_t = false;
                    if p_depth == 0 {
                        flush(&mut para, &mut cell, &mut out.lines);
                    }
                }
                b"p" => {
                    p_depth = p_depth.saturating_sub(1);
                    flush(&mut para, &mut cell, &mut out.lines);
                }
                b"tc" if tbl_depth == 1 => {
                    flush(&mut para, &mut cell, &mut out.lines);
                    let text = cell.take().unwrap_or_default().join(" ");
                    if let Some(cells) = row.as_mut() {
                        cells.push(text);
                    }
                }
                b"tr" if tbl_depth == 1 => {
                    if let Some(cells) = row.take() {
                        if cells.iter().any(|c| !c.is_empty()) {
                            out.lines.push(cells.join(" | "));
                        }
                    }
                }
                b"tbl" => tbl_depth = tbl_depth.saturating_sub(1),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    flush(&mut para, &mut cell, &mut out.lines);
    Ok(out)
}

/// Все `t` файла через пробел (заметки докладчика).
fn all_text(xml: &[u8]) -> Result<String, String> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut in_t = false;
    let mut pieces: Vec<String> = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).map_err(|_| "Не удалось разобрать содержимое файла".to_string())? {
            Event::Start(e) if e.local_name().as_ref() == b"t" => in_t = true,
            Event::End(e) if e.local_name().as_ref() == b"t" => in_t = false,
            Event::Text(t) if in_t => {
                let piece = text_of(&t);
                if !piece.trim().is_empty() {
                    pieces.push(piece.trim().to_string());
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(pieces.join(" "))
}

/// Связи из `.rels`: (Id, Target).
fn parse_rels(xml: &[u8]) -> Vec<(String, String)> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut rels = Vec::new();
    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                let (mut id, mut target) = (None, None);
                for attr in e.attributes().flatten() {
                    let Ok(value) = attr.unescape_value() else { continue };
                    match attr.key.local_name().as_ref() {
                        b"Id" => id = Some(value.into_owned()),
                        b"Target" => target = Some(value.into_owned()),
                        _ => {}
                    }
                }
                if let (Some(id), Some(target)) = (id, target) {
                    rels.push((id, target));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    rels
}

/// Декодирует картинку слайда, если в ней не больше `max_pixels`, и приводит к PNG
/// с длинной стороной не больше `IMAGE_LONG_SIDE`. Негодную или слишком большую — пропускает.
fn picture_to_png(data: &[u8], max_pixels: u64) -> Option<Vec<u8>> {
    let guess = |d: &[u8]| image::ImageReader::new(Cursor::new(d.to_vec())).with_guessed_format().ok();
    // Размеры — по заголовку, до выделения памяти под пиксели.
    let (width, height) = guess(data)?.into_dimensions().ok()?;
    if u64::from(width) * u64::from(height) > max_pixels {
        return None;
    }
    let mut reader = guess(data)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(max_pixels.saturating_mul(8));
    reader.limits(limits);
    let mut picture = reader.decode().ok()?;
    if picture.width().max(picture.height()) > IMAGE_LONG_SIDE {
        picture = picture.resize(IMAGE_LONG_SIDE, IMAGE_LONG_SIDE, image::imageops::FilterType::Triangle);
    }
    let mut png = Vec::new();
    picture.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

/// Номера слайдов `ppt/slides/slideN.xml`, по возрастанию N.
fn slide_numbers(archive: &Archive<'_>) -> Vec<u64> {
    let mut numbers: Vec<u64> = archive
        .file_names()
        .filter(|name| is_safe_name(name))
        .filter_map(|name| name.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok())
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

pub fn extract_pptx(bytes: &[u8]) -> Result<Vec<PageOut>, String> {
    extract_pptx_with_limit(bytes, MAX_UNZIPPED_BYTES)
}

fn extract_pptx_with_limit(bytes: &[u8], limit: u64) -> Result<Vec<PageOut>, String> {
    let mut archive = read_zip(bytes)?;
    let numbers = slide_numbers(&archive);
    if numbers.is_empty() {
        return Err("В файле нет слайдов: это не PPTX".to_string());
    }
    if numbers.len() > MAX_PAGES {
        return Err(format!("В презентации {} слайдов — больше {MAX_PAGES}. Разделите файл.", numbers.len()));
    }
    let mut budget = limit;
    let mut pages = Vec::with_capacity(numbers.len());
    for (index, n) in numbers.iter().enumerate() {
        let xml = read_entry(&mut archive, &format!("ppt/slides/slide{n}.xml"), &mut budget)?.unwrap_or_default();
        let parsed = parse_text(&xml)?;
        let rels = read_entry(&mut archive, &format!("ppt/slides/_rels/slide{n}.xml.rels"), &mut budget)?
            .map(|data| parse_rels(&data))
            .unwrap_or_default();

        let mut lines = parsed.lines;
        if let Some(notes_path) = rels
            .iter()
            .find(|(_, target)| target.contains("notesSlide"))
            .and_then(|(_, target)| resolve("ppt/slides", target))
        {
            if let Some(notes_xml) = read_entry(&mut archive, &notes_path, &mut budget)? {
                let notes = all_text(&notes_xml)?;
                if !notes.is_empty() {
                    lines.push(format!("Заметки: {notes}"));
                }
            }
        }
        let text = lines.join("\n");

        let mut image_png = None;
        if quality::needs_reading(&text) {
            for id in &parsed.blips {
                let Some(path) = rels
                    .iter()
                    .find(|(rid, _)| rid == id)
                    .and_then(|(_, target)| resolve("ppt/slides", target))
                else {
                    continue;
                };
                let Some(data) = read_entry(&mut archive, &path, &mut budget)? else { continue };
                if let Some(png) = picture_to_png(&data, IMAGE_MAX_PIXELS) {
                    image_png = Some(png);
                    break;
                }
            }
        }
        pages.push(PageOut { number: index as u32 + 1, text, image_png });
    }
    Ok(pages)
}

pub fn extract_docx(bytes: &[u8]) -> Result<Vec<PageOut>, String> {
    extract_docx_with_limit(bytes, MAX_UNZIPPED_BYTES)
}

fn extract_docx_with_limit(bytes: &[u8], limit: u64) -> Result<Vec<PageOut>, String> {
    let mut archive = read_zip(bytes)?;
    let mut budget = limit;
    let xml = read_entry(&mut archive, "word/document.xml", &mut budget)?
        .ok_or_else(|| "В файле нет текста документа: это не DOCX".to_string())?;
    let text = parse_text(&xml)?.lines.join("\n");
    Ok(vec![PageOut { number: 1, text, image_png: None }])
}

/// Кодировка TXT/MD: UTF-8 (в т.ч. с BOM), иначе cp1251, если это в основном кириллица.
fn decode_text(raw: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(raw) {
        return text.strip_prefix('\u{feff}').unwrap_or(text).to_string();
    }
    let (alt, _) = encoding_rs::WINDOWS_1251.decode_without_bom_handling(raw);
    let letters = alt.chars().filter(|c| c.is_alphabetic()).count();
    let cyrillic = alt.chars().filter(|c| c.is_alphabetic() && ('\u{0400}'..='\u{04FF}').contains(c)).count();
    if letters > 0 && cyrillic * 2 >= letters {
        return alt.into_owned();
    }
    String::from_utf8_lossy(raw).into_owned()
}

pub fn extract_plain(bytes: &[u8]) -> Vec<PageOut> {
    vec![PageOut { number: 1, text: decode_text(bytes).trim().to_string(), image_png: None }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::course::FileKind;
    use std::io::Write;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut buffer);
        for (name, content) in files {
            writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
        buffer.into_inner()
    }

    const SLIDE1: &str = r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
        <p:sp><p:txBody><a:p><a:r><a:t>Соединения в SQL</a:t></a:r></a:p></p:txBody></p:sp>
        <p:graphicFrame><a:tbl><a:tr><a:tc><a:txBody><a:p><a:r><a:t>INNER</a:t></a:r></a:p></a:txBody></a:tc>
        <a:tc><a:txBody><a:p><a:r><a:t>пересечение</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></p:graphicFrame>
        </p:spTree></p:cSld></p:sld>"#;
    const NOTES1: &str = r#"<p:notes xmlns:p="p" xmlns:a="a"><a:t>Сказать про NULL</a:t></p:notes>"#;
    const SLIDE2: &str = r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:pic><a:blip r:embed="rId2"/></p:pic></p:spTree></p:cSld></p:sld>"#;
    const SLIDE2_RELS: &str = r#"<Relationships><Relationship Id="rId2" Target="../media/image1.png"/></Relationships>"#;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::new_rgb8(w, h).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn pptx_slides_tables_and_notes_in_order() {
        let picture = png(3000, 1500);
        let bytes = zip_of(&[
            ("ppt/slides/slide1.xml", SLIDE1.as_bytes()),
            ("ppt/notesSlides/notesSlide1.xml", NOTES1.as_bytes()),
            ("ppt/slides/_rels/slide1.xml.rels", br#"<Relationships><Relationship Id="rIdN" Target="../notesSlides/notesSlide1.xml"/></Relationships>"#),
            ("ppt/slides/slide2.xml", SLIDE2.as_bytes()),
            ("ppt/slides/_rels/slide2.xml.rels", SLIDE2_RELS.as_bytes()),
            ("ppt/media/image1.png", &picture),
        ]);
        let pages = extract_pptx(&bytes).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].text, "Соединения в SQL\nINNER | пересечение\nЗаметки: Сказать про NULL");
        assert_eq!(pages[0].image_png, None);
        let shrunk = image::load_from_memory(pages[1].image_png.as_ref().unwrap()).unwrap();
        assert_eq!(shrunk.width(), 1568, "длинная сторона ужата");
    }

    #[test]
    fn slide_order_is_numeric_not_lexical() {
        let mut files = Vec::new();
        let slides: Vec<String> = (1..=11).map(|n| format!(r#"<p:sld xmlns:p="p" xmlns:a="a"><a:t>Слайд номер {n} с достаточно длинным текстом, чтобы не считаться пустым: нормальные формы, ключи, зависимости, транзакции и индексы.</a:t></p:sld>"#)).collect();
        let names: Vec<String> = (1..=11).map(|n| format!("ppt/slides/slide{n}.xml")).collect();
        for (name, slide) in names.iter().zip(&slides) {
            files.push((name.as_str(), slide.as_bytes()));
        }
        let pages = extract_pptx(&zip_of(&files)).unwrap();
        assert!(pages[1].text.contains("номер 2"), "slide10 не должен встать вторым");
        assert_eq!(pages[10].number, 11);
    }

    #[test]
    fn slide_with_text_keeps_no_picture() {
        let slide = r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><a:t>Нормальные формы, ключи, зависимости, транзакции и индексы: длинный осмысленный текст слайда, где букв заведомо больше восьмидесяти.</a:t><a:blip r:embed="rId2"/></p:sld>"#;
        let bytes = zip_of(&[
            ("ppt/slides/slide1.xml", slide.as_bytes()),
            ("ppt/slides/_rels/slide1.xml.rels", SLIDE2_RELS.as_bytes()),
            ("ppt/media/image1.png", &png(10, 10)),
        ]);
        assert_eq!(extract_pptx(&bytes).unwrap()[0].image_png, None);
    }

    #[test]
    fn unknown_entity_does_not_break_parsing() {
        let slide = r#"<p:sld xmlns:p="p" xmlns:a="a"><a:t>Цена&nbsp;100</a:t></p:sld>"#;
        let pages = extract_pptx(&zip_of(&[("ppt/slides/slide1.xml", slide.as_bytes())])).unwrap();
        assert!(pages[0].text.contains("Цена"), "{}", pages[0].text);
    }

    #[test]
    fn docx_paragraphs_and_tables() {
        let document = r#"<w:document xmlns:w="w"><w:body>
            <w:p><w:r><w:t>Лабораторная 1. </w:t></w:r><w:r><w:t>Создание таблиц</w:t></w:r></w:p>
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Поле</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Тип</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            </w:body></w:document>"#;
        let pages = extract_docx(&zip_of(&[("word/document.xml", document.as_bytes())])).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].text, "Лабораторная 1. Создание таблиц\nПоле | Тип");
    }

    #[test]
    fn zip_bomb_is_refused() {
        let bytes = zip_of(&[("word/document.xml", &vec![b'a'; 5000])]);
        let error = extract_docx_with_limit(&bytes, 1000).unwrap_err();
        assert!(error.contains("слишком большой"), "{error}");
        let slide = vec![b'a'; 5000];
        let error = extract_pptx_with_limit(&zip_of(&[("ppt/slides/slide1.xml", &slide)]), 1000).unwrap_err();
        assert!(error.contains("слишком большой"), "{error}");
    }

    #[test]
    fn budget_is_shared_between_entries_and_enforced_in_read_entry() {
        let bytes = zip_of(&[("a.xml", &[b'x'; 600]), ("b.xml", &[b'x'; 600])]);
        let mut archive = read_zip(&bytes).unwrap();
        let mut budget = 1000;
        assert_eq!(read_entry(&mut archive, "a.xml", &mut budget).unwrap().unwrap().len(), 600);
        assert_eq!(budget, 400);
        assert!(read_entry(&mut archive, "b.xml", &mut budget).is_err());
        assert_eq!(read_entry(&mut archive, "missing.xml", &mut budget).unwrap(), None);
        assert_eq!(read_entry(&mut archive, "../a.xml", &mut budget).unwrap(), None);
        // публичный бюджет — тот же код с MAX_UNZIPPED_BYTES
        assert_eq!(extract_docx(&zip_of(&[("word/document.xml", b"<w:p/>")])).unwrap().len(), 1);
    }

    #[test]
    fn huge_picture_is_skipped_but_slide_stays() {
        let picture = png(100, 100);
        assert!(picture_to_png(&picture, 100 * 100).is_some());
        assert!(picture_to_png(&picture, 100 * 100 - 1).is_none(), "больше предела пикселей — пропуск");
        assert!(picture_to_png(b"not an image", IMAGE_MAX_PIXELS).is_none());
    }

    #[test]
    fn plain_text_guesses_cp1251() {
        let (encoded, _, _) = encoding_rs::WINDOWS_1251.encode("Индексы и ключи");
        assert_eq!(extract_plain(&encoded)[0].text, "Индексы и ключи");
        assert_eq!(extract_plain("Индексы".as_bytes())[0].text, "Индексы");
        assert_eq!(extract_plain("\u{feff}Ключи\n".as_bytes())[0].text, "Ключи");
    }

    #[test]
    fn old_formats_get_a_clear_refusal() {
        let error = FileKind::from_name("Программа.doc").unwrap_err();
        assert!(error.contains("DOCX"), "{error}");
        assert!(FileKind::from_name("Таблица.xlsx").is_err());
        assert_eq!(FileKind::from_name("Лекция.PDF").unwrap(), FileKind::Pdf);
        assert_eq!(FileKind::from_name("заметки.md").unwrap(), FileKind::Md);
    }

    #[test]
    fn oversized_file_is_refused_before_parsing() {
        let big = vec![0u8; (crate::course::MAX_FILE_BYTES + 1) as usize];
        assert_eq!(crate::course::extract(FileKind::Txt, &big).unwrap_err(), "Файл больше 50 МБ");
    }
}
