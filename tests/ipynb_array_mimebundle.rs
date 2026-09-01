//! An array-form mimebundle payload must yield its image.
//!
//! nbformat's `multiline_string` union means every text-ish value — cell
//! sources AND mimebundle data — may be a string or an array of strings, the
//! array form being base64 split across lines, which is exactly how Jupyter
//! writes large plots. The extractor called `.as_str()` on the payload, which
//! returns `None` for an array, so the image was silently dropped: a 9,216-byte
//! PNG in `nbf_invalid.ipynb` produced nothing.
//!
//! `join_source` already existed for cell sources; the fix routes mimebundle
//! payloads through it too. `decode_b64_image` strips the embedded newlines.

use base64::Engine;
use chunks_rs::formats::ipynb;

const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R',
];

fn notebook(payload: serde_json::Value) -> Vec<u8> {
    serde_json::json!({
        "nbformat": 4, "nbformat_minor": 5, "metadata": {},
        "cells": [{
            "cell_type": "code", "id": "a", "source": ["1+1"],
            "execution_count": 1, "metadata": {},
            "outputs": [{
                "output_type": "display_data", "metadata": {},
                "data": { "image/png": payload }
            }]
        }]
    })
    .to_string()
    .into_bytes()
}

#[test]
fn an_array_form_png_is_extracted() {
    let b64 = base64::engine::general_purpose::STANDARD.encode(PNG);
    let (head, tail) = b64.split_at(10);
    // Array form: the encoded payload split across two "lines".
    let nb = notebook(serde_json::json!([format!("{head}\n"), tail]));
    let (_md, images) =
        ipynb::to_markdown_with_images_from_bytes(&nb).expect("notebook must parse");
    assert_eq!(images.len(), 1, "array-form payload was dropped");
    assert_eq!(images[0].1, PNG, "decoded bytes differ");
}

/// Control: the string form must keep working identically.
#[test]
fn the_string_form_still_works() {
    let b64 = base64::engine::general_purpose::STANDARD.encode(PNG);
    let nb = notebook(serde_json::json!(b64));
    let (_md, images) =
        ipynb::to_markdown_with_images_from_bytes(&nb).expect("notebook must parse");
    assert_eq!(images.len(), 1, "string form regressed");
    assert_eq!(images[0].1, PNG);
}
