// ============================================================================
// PDFium for course materials
// ============================================================================
// pdfium.dll (Chrome's PDF engine, BSD-3/Apache-2.0) reads the text of PDF
// pages and renders pages without text to PNG. Pinned by SHA-256 like FFmpeg:
// nothing is written into the bundle before the archive hash matches, and the
// DLL itself is checked again before it is placed.

use sha2::{Digest, Sha256};

const PDFIUM_URL: &str =
    "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/7881/pdfium-win-x64.tgz";
const PDFIUM_ARCHIVE_SHA256: &str = "73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08";
const PDFIUM_DLL_SHA256: &str = "79d4676b656cfb1abcea88f9ade3b4b0826c5200382db5f4ec72a636c598c118";

pub fn ensure_pdfium() {
    println!("cargo:rerun-if-changed=build/pdfium.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dll = manifest.join("resources").join("pdfium").join("pdfium.dll");
    if sha256_file(&dll).as_deref() == Ok(PDFIUM_DLL_SHA256) {
        return;
    }
    if let Err(e) = download_and_extract_pdfium(&manifest, &dll) {
        panic!("Failed to prepare PDFium: {}", e);
    }
}

fn download_and_extract_pdfium(
    manifest: &std::path::Path,
    dll: &std::path::Path,
) -> Result<(), String> {
    println!("cargo:warning=Downloading PDFium from: {}", PDFIUM_URL);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
    let response = client
        .get(PDFIUM_URL)
        .send()
        .map_err(|e| format!("Failed to download: {}", e))?;
    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }
    let content = response
        .bytes()
        .map_err(|e| format!("Failed to read response: {}", e))?;

    let actual = sha256_hex(&content);
    if actual != PDFIUM_ARCHIVE_SHA256 {
        return Err(format!(
            "PDFium archive SHA-256 mismatch for {}: expected {}, got {}. Refusing to extract.",
            PDFIUM_URL, PDFIUM_ARCHIVE_SHA256, actual
        ));
    }
    println!("cargo:warning=PDFium archive SHA-256 matches pinned value");

    // In memory: bin/pdfium.dll, LICENSE and licenses/* (flat, one folder deep).
    let mut dll_bytes: Option<Vec<u8>> = None;
    let mut notices: Vec<(String, Vec<u8>)> = Vec::new();
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(&content[..]));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive
        .entries()
        .map_err(|e| format!("Failed to read PDFium archive: {}", e))?
    {
        let mut entry = entry.map_err(|e| format!("Failed to read archive entry: {}", e))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| format!("Bad archive path: {}", e))?
            .to_path_buf();
        let rel = path.to_string_lossy().replace('\\', "/");
        let rel = rel.trim_start_matches("./");
        let notice_name = if rel == "LICENSE" {
            Some("LICENSE.txt".to_string())
        } else {
            rel.strip_prefix("licenses/")
                .filter(|name| !name.is_empty() && !name.contains('/') && !name.contains(".."))
                .map(str::to_string)
        };
        if rel == "bin/pdfium.dll" {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes)
                .map_err(|e| format!("Failed to read pdfium.dll: {}", e))?;
            dll_bytes = Some(bytes);
        } else if let Some(name) = notice_name {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes)
                .map_err(|e| format!("Failed to read {}: {}", name, e))?;
            notices.push((name, bytes));
        }
    }
    let dll_bytes = dll_bytes.ok_or("'bin/pdfium.dll' not found in the PDFium archive")?;

    let dll_dir = dll.parent().ok_or("pdfium.dll has no parent folder")?;
    std::fs::create_dir_all(dll_dir)
        .map_err(|e| format!("Failed to create {:?}: {}", dll_dir, e))?;
    write_verified(&dll_bytes, PDFIUM_DLL_SHA256, dll)?;

    // The archive hash is pinned, so its notices are trusted as a whole.
    let notices_dir = manifest.join("licenses").join("pdfium");
    std::fs::create_dir_all(&notices_dir)
        .map_err(|e| format!("Failed to create {:?}: {}", notices_dir, e))?;
    for (name, bytes) in notices {
        let path = notices_dir.join(&name);
        std::fs::write(&path, bytes).map_err(|e| format!("Failed to write {:?}: {}", path, e))?;
    }
    println!("cargo:warning=PDFium ready");
    Ok(())
}

/// Writes `bytes` to `path` only if they are the pinned content, through a
/// temporary name, and checks the file that landed.
fn write_verified(bytes: &[u8], sha256: &str, path: &std::path::Path) -> Result<(), String> {
    let actual = sha256_hex(bytes);
    if actual != sha256 {
        return Err(format!(
            "{:?} from the PDFium archive: SHA-256 mismatch, expected {}, got {}",
            path.file_name().unwrap_or_default(),
            sha256,
            actual
        ));
    }
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes).map_err(|e| format!("Failed to write {:?}: {}", partial, e))?;
    std::fs::rename(&partial, path).map_err(|e| format!("Failed to place {:?}: {}", path, e))?;
    if sha256_file(path)? != sha256 {
        let _ = std::fs::remove_file(path);
        return Err(format!("{:?} changed on its way to disk", path));
    }
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("Failed to open {:?}: {}", path, e))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("Failed to read {:?}: {}", path, e))?;
    Ok(format!("{:x}", hasher.finalize()))
}
