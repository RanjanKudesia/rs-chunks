//! EPUB images must arrive in manifest document order, deterministically.
//!
//! `ExtractedImages`' own doc comment promises "every image extracted from one
//! document, in document order" — and the epub path violated it: the manifest
//! was iterated via `HashMap::values()`, whose order is seeded per process, so
//! image chunks arrived in a different order on different runs (measured: 6
//! distinct orders across 8 processes). Output that is not byte-identical to
//! ITSELF cannot be byte-identical across three language bindings, which is
//! this project's product claim.
//!
//! Eight images make an accidental pass vanishingly unlikely without the fix:
//! a random HashMap order matches document order with probability 1/8! ≈ 2.5e-5.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;

/// Manifest order is deliberately neither alphabetical nor ZIP order.
const IMAGE_IDS: [&str; 8] = [
    "img-echo", "img-alpha", "img-hotel", "img-charlie",
    "img-golf", "img-bravo", "img-foxtrot", "img-delta",
];

fn build_epub() -> Vec<u8> {
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zw.start_file("mimetype", stored).unwrap();
    zw.write_all(b"application/epub+zip").unwrap();
    let d = SimpleFileOptions::default();

    zw.start_file("META-INF/container.xml", d).unwrap();
    zw.write_all(
        br#"<?xml version="1.0"?><container version="1.0"
 xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>
 <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
 </rootfiles></container>"#,
    )
    .unwrap();

    let mut items = String::new();
    for id in IMAGE_IDS {
        items.push_str(&format!(
            r#"<item id="{id}" href="{id}.png" media-type="image/png"/>"#
        ));
    }
    zw.start_file("OEBPS/content.opf", d).unwrap();
    zw.write_all(
        format!(
            r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf"
 version="3.0" unique-identifier="uid">
 <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:identifier id="uid">t</dc:identifier><dc:title>Order</dc:title>
 </metadata>
 <manifest>
  <item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/>
  {items}
 </manifest>
 <spine><itemref idref="c1"/></spine></package>"#
        )
        .as_bytes(),
    )
    .unwrap();

    zw.start_file("OEBPS/c1.xhtml", d).unwrap();
    zw.write_all(
        b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><p>Enough prose to make one paragraph of ordinary chunkable text for the spine.</p></body></html>",
    )
    .unwrap();

    // A minimal valid PNG header per image; content is irrelevant to ordering.
    // Written to the ZIP in ALPHABETICAL order so ZIP order != manifest order.
    let mut sorted = IMAGE_IDS;
    sorted.sort();
    for id in sorted {
        zw.start_file(format!("OEBPS/{id}.png"), d).unwrap();
        zw.write_all(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        zw.write_all(id.as_bytes()).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

#[test]
fn images_arrive_in_manifest_document_order() {
    let bytes = build_epub();
    let (_chunks, images) =
        chunks_rs::formats::epub::chunk_with_images_from_bytes(&bytes, "default", 3, 1, 5, 3)
            .expect("epub must parse");
    assert_eq!(images.len(), IMAGE_IDS.len(), "wrong image count");
    // Join on the payload (each PNG embeds its manifest id) rather than the
    // engine-assigned name, so the assertion survives any renaming scheme.
    let order: Vec<&str> = images
        .iter()
        .map(|(_, data)| {
            IMAGE_IDS
                .iter()
                .copied()
                .find(|id| data.ends_with(id.as_bytes()))
                .expect("payload carries its id")
        })
        .collect();
    assert_eq!(
        order,
        IMAGE_IDS.to_vec(),
        "images not in manifest document order"
    );
}
