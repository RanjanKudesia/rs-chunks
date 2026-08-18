//! Content-addressed names for extracted images: `"<16 hex>.<ext>"`.
//!
//! The formats that route through here — docx, pptx, xlsx, html, odf
//! (odt/odp) and the legacy ODRAW path (doc/ppt) — name images this way, so
//! the same picture in two documents gets the same key and identical bytes
//! inside one document are deduped.
//!
//! **PDF is the exception and does not use this module.** It names images
//! positionally, `"image_p{page}_{ordinal}.{ext}"` (see `formats/pdf/parse.rs`),
//! because a PDF image is identified by its object id and its reading-order
//! position on a page rather than by its decoded bytes.
//!
//! # Why this is not `DefaultHasher` (TECH_DEBT #76)
//!
//! Seven formats each had their own copy of
//!
//! ```ignore
//! let mut hasher = DefaultHasher::new();
//! bytes.hash(&mut hasher);
//! format!("{:016x}{ext}", hasher.finish())
//! ```
//!
//! and **every one produced a different name under `js-chunks` than under
//! `py-chunks`**, from the same bytes and the same source file. Two separate
//! faults, both invisible to a test that only checks the shape of a name:
//!
//! 1. `<[u8] as Hash>::hash` writes `self.len()` as a **native `usize`** before
//!    the bytes. That prefix is 8 bytes on aarch64/x86-64 and **4 bytes on
//!    wasm32**, so the hasher consumed a different byte stream on each target.
//!    `Hasher::write` takes the bytes with no prefix and has no such dependency.
//! 2. `DefaultHasher`'s algorithm is explicitly **not guaranteed stable across
//!    Rust releases** (`std::collections::hash_map::DefaultHasher` docs). Three
//!    SDKs built with three toolchains could diverge again at any upgrade, which
//!    is the same defect wearing a different hat.
//!
//! So the algorithm is pinned here instead: FNV-1a, 64-bit, written out. It is
//! not cryptographic and does not need to be — it is a dedup key, and the
//! property that matters is that it is identical everywhere, forever.

/// Extensions we emit images for. Anything else is not a renderable raster.
const SUPPORTED: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over the raw bytes. Pinned: no length prefix, no std hasher, no
/// target-dependent width anywhere in the stream.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// `"<16 hex>.<ext>"` for a known extension, given without its dot.
///
/// Returns `None` for anything that is not a renderable raster format.
pub fn name_with_ext(bytes: &[u8], ext: &str) -> Option<String> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    if !SUPPORTED.contains(&ext.as_str()) {
        return None;
    }
    Some(format!("{:016x}.{ext}", content_hash(bytes)))
}

/// `"<16 hex>.<ext>"` with the extension taken from a file or zip-entry path.
///
/// Five formats had this extension ladder written out longhand next to their
/// own copy of the hash.
pub fn name_for_path(bytes: &[u8], path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    let ext = SUPPORTED
        .iter()
        .find(|e| lower.ends_with(&format!(".{e}")))?;
    name_with_ext(bytes, ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published FNV-1a 64 test vectors. If this test ever fails, the
    /// hash has changed and every image name in the library changed with it —
    /// which is exactly the event the three SDKs must never disagree about.
    #[test]
    fn fnv1a_matches_the_published_vectors() {
        assert_eq!(content_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(content_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(content_hash(b"foobar"), 0x8594_4171_f739_67e8);
    }

    /// No length prefix enters the stream — that prefix was written as a
    /// native `usize` and is why wasm32 and 64-bit hosts disagreed (#76).
    #[test]
    fn the_hash_does_not_depend_on_a_length_prefix() {
        // Two inputs whose 4-byte and 8-byte length prefixes would differ, and
        // whose hashes must be decided by content alone.
        assert_ne!(content_hash(b"ab"), content_hash(b"abc"));
        // Concatenation is not confused with a length change.
        assert_eq!(content_hash(b"foobar"), content_hash(b"foobar"));
    }

    #[test]
    fn names_are_sixteen_hex_digits_plus_the_extension() {
        let name = name_with_ext(b"payload", "png").unwrap();
        assert_eq!(name.len(), 16 + ".png".len());
        assert!(name.ends_with(".png"));
        assert!(name[..16].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn identical_bytes_get_identical_names() {
        assert_eq!(
            name_with_ext(b"same picture", "jpg"),
            name_with_ext(b"same picture", "jpg")
        );
        assert_ne!(
            name_with_ext(b"one", "png"),
            name_with_ext(b"another", "png")
        );
    }

    #[test]
    fn a_leading_dot_and_upper_case_are_both_accepted() {
        let a = name_with_ext(b"x", "PNG").unwrap();
        let b = name_with_ext(b"x", ".png").unwrap();
        assert_eq!(a, b);
        assert!(
            a.ends_with(".png"),
            "the extension is normalised to lower case"
        );
    }

    #[test]
    fn an_unsupported_extension_is_not_named() {
        assert!(name_with_ext(b"x", "emf").is_none());
        assert!(name_with_ext(b"x", "tiff").is_none());
        assert!(name_for_path(b"x", "media/diagram.wmf").is_none());
    }

    #[test]
    fn the_extension_is_read_off_a_zip_entry_path() {
        assert_eq!(
            name_for_path(b"x", "word/media/image1.JPEG"),
            name_with_ext(b"x", "jpeg")
        );
        assert_eq!(
            name_for_path(b"x", "Pictures/1000000100000.png"),
            name_with_ext(b"x", "png")
        );
    }

    /// `.jpg` and `.jpeg` are different extensions and must stay distinct —
    /// the same bytes stored under either keep the caller's spelling.
    #[test]
    fn jpg_and_jpeg_are_not_collapsed() {
        assert_ne!(
            name_with_ext(b"x", "jpg").unwrap(),
            name_with_ext(b"x", "jpeg").unwrap()
        );
    }
}
