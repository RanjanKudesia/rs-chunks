//! ODF container handling: open the zip, detect kind/encryption, and pull out
//! `content.xml`, `meta.xml`, and the `Pictures/` images. ODF text/presentation
//! files are a zip of XML parts (same container family as `.ods`/`.epub`).

use std::io::{Cursor, Read};

use zip::ZipArchive;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OdfKind {
    Text,
    Presentation,
}

pub struct OdfContainer {
    pub content_xml: String,
    pub meta_xml: Option<String>,
    /// (basename, bytes) for each `Pictures/` image, in archive order.
    pub images: Vec<(String, Vec<u8>)>,
}

fn read_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> Option<Vec<u8>> {
    let mut file = archive.by_name(name).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Load and validate an ODF file for the expected extension.
pub fn load(bytes: &[u8], expected: OdfKind) -> Result<OdfContainer, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("Not a valid ODF (zip) file: {e}"))?;

    // Encryption: encrypted ODF declares per-part encryption in the manifest.
    if let Some(manifest) = read_entry(&mut archive, "META-INF/manifest.xml") {
        let manifest = String::from_utf8_lossy(&manifest);
        if manifest.contains("manifest:encryption-data") {
            return Err("ODF file is encrypted (password-protected); cannot extract".to_string());
        }
    }

    let content_bytes = read_entry(&mut archive, "content.xml")
        .ok_or_else(|| "ODF file has no content.xml".to_string())?;
    let content_xml = String::from_utf8_lossy(&content_bytes).into_owned();

    // Verify the mimetype matches the expected kind when present (some files
    // omit the optional mimetype entry — fall back to the extension's expectation).
    if let Some(mime) = read_entry(&mut archive, "mimetype") {
        let mime = String::from_utf8_lossy(&mime);
        let is_text = mime.contains("opendocument.text");
        let is_pres = mime.contains("opendocument.presentation");
        if (is_text || is_pres)
            && ((expected == OdfKind::Text && !is_text)
                || (expected == OdfKind::Presentation && !is_pres))
        {
            return Err(format!("ODF mimetype mismatch: {}", mime.trim()));
        }
    }

    let meta_xml = read_entry(&mut archive, "meta.xml")
        .map(|b| String::from_utf8_lossy(&b).into_owned());

    // Collect Pictures/ images.
    let mut images: Vec<(String, Vec<u8>)> = Vec::new();
    let names: Vec<String> = archive.file_names().map(|s| s.to_string()).collect();
    for name in names {
        if name.starts_with("Pictures/") && !name.ends_with('/') {
            if let Some(bytes) = read_entry(&mut archive, &name) {
                let base = name.rsplit('/').next().unwrap_or(&name).to_string();
                if !base.is_empty() {
                    images.push((base, bytes));
                }
            }
        }
    }

    Ok(OdfContainer {
        content_xml,
        meta_xml,
        images,
    })
}

/// Extract `dc:title` and `dc:creator` from `meta.xml` if present.
pub fn parse_meta(meta_xml: &str) -> (Option<String>, Option<String>) {
    let title = extract_between(meta_xml, "<dc:title>", "</dc:title>");
    let creator = extract_between(meta_xml, "<dc:creator>", "</dc:creator>");
    (title, creator)
}

fn extract_between(haystack: &str, open: &str, close: &str) -> Option<String> {
    let start = haystack.find(open)? + open.len();
    let end = haystack[start..].find(close)? + start;
    let val = crate::formats::odf::text::decode_entities(&haystack[start..end]);
    let val = val.trim().to_string();
    (!val.is_empty()).then_some(val)
}
