//! Opening a PDF and walking its pages.
//!
//! `/MediaBox`, `/Rotate` and `/Resources` are *inheritable*: a page may declare
//! none of them and take them from an ancestor in the page tree. lopdf resolves
//! resources only as a list of ancestor dictionaries, so the merge — nearest
//! ancestor wins, per sub-dictionary — happens here.

use std::rc::Rc;

use lopdf::{Dictionary, Document, Object, ObjectId};

use super::content::{Extractor, Glyph, PageContent};
use super::geom::{self, Matrix};

/// US Letter, used when a page declares no `/MediaBox` anywhere up the tree.
const DEFAULT_MEDIA_BOX: [f32; 4] = [0.0, 0.0, 612.0, 792.0];

/// The resource categories a content stream can name. Merged individually so a
/// page that overrides `/Font` still inherits its parent's `/XObject`.
const RESOURCE_KEYS: [&[u8]; 5] = [b"Font", b"XObject", b"ExtGState", b"ColorSpace", b"Pattern"];

pub(crate) struct Page {
    pub content: PageContent,
    // Page geometry captured from /MediaBox; not yet consumed downstream but
    // only recoverable here.
    #[allow(dead_code)]
    pub width: f32,
    #[allow(dead_code)]
    pub height: f32,
}

pub(crate) fn open(bytes: &[u8]) -> Result<Document, String> {
    Document::load_mem(bytes).map_err(|e| format!("Failed to parse PDF: {e}"))
}

/// Read one page's glyphs and drawn images, already in upright page space.
pub(crate) fn read_page(extractor: &mut Extractor, doc: &Document, page_id: ObjectId) -> Page {
    let media_box = inherited(doc, page_id, b"MediaBox")
        .and_then(|o| rect(doc, &o))
        .unwrap_or(DEFAULT_MEDIA_BOX);
    let rotate = inherited(doc, page_id, b"Rotate")
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let (base, width, height) = geom::page_transform(media_box, rotate);

    let mut content = PageContent::default();
    if let Ok(data) = doc.get_page_content(page_id) {
        let resources = resources_for(doc, page_id);
        extractor.run(doc, &data, &resources, base, &mut content);
    }
    apply_links(&mut content.glyphs, &links(doc, page_id, base));
    Page { content, width, height }
}

/// A page's effective resource dictionary: its own entries, then any category
/// its ancestors declare that it does not.
fn resources_for(doc: &Document, page_id: ObjectId) -> Dictionary {
    let mut merged = Dictionary::new();
    let mut chain: Vec<Dictionary> = Vec::new();
    if let Ok((own, ancestors)) = doc.get_page_resources(page_id) {
        if let Some(d) = own {
            chain.push(d.clone());
        }
        for id in ancestors {
            if let Ok(d) = doc.get_dictionary(id) {
                chain.push(d.clone());
            }
        }
    }
    for key in RESOURCE_KEYS {
        let mut category = Dictionary::new();
        // Reverse order so the nearest dictionary's entries are written last.
        for dict in chain.iter().rev() {
            if let Ok(sub) = dict.get_deref(key, doc).and_then(Object::as_dict) {
                for (name, value) in sub.iter() {
                    category.set(name.clone(), value.clone());
                }
            }
        }
        if !category.is_empty() {
            merged.set(key.to_vec(), Object::Dictionary(category));
        }
    }
    merged
}

/// Look up an inheritable page attribute, walking `/Parent` until it is found.
fn inherited(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = page_id;
    let mut seen = Vec::new();
    loop {
        let dict = doc.get_dictionary(current).ok()?;
        if let Ok(value) = dict.get_deref(key, doc) {
            return Some(value.clone());
        }
        let parent = dict.get(b"Parent").and_then(Object::as_reference).ok()?;
        if seen.contains(&parent) {
            return None;
        }
        seen.push(parent);
        current = parent;
    }
}

fn rect(doc: &Document, object: &Object) -> Option<[f32; 4]> {
    let array = object.as_array().ok()?;
    if array.len() < 4 {
        return None;
    }
    let mut out = [0.0f32; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = doc.dereference(&array[i]).ok()?.1.as_float().ok()?;
    }
    Some(out)
}

/// A hyperlink's target and the area of the page it covers.
struct Link {
    rect: [f32; 4],
    uri: Rc<str>,
}

/// Read a page's `/Link` annotations, in upright page space.
///
/// A PDF keeps hyperlink targets in `/Annots`, never in the content stream, so
/// a reference list's DOIs exist nowhere in the text — dropping annotations
/// drops the URLs entirely.
fn links(doc: &Document, page_id: ObjectId, transform: Matrix) -> Vec<Link> {
    let Ok(annots) = doc
        .get_dictionary(page_id)
        .and_then(|d| d.get_deref(b"Annots", doc))
        .and_then(Object::as_array)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in annots {
        let Ok((_, object)) = doc.dereference(entry) else { continue };
        let Ok(annot) = object.as_dict() else { continue };
        if annot.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"") != b"Link" {
            continue;
        }
        let uri = annot
            .get_deref(b"A", doc)
            .and_then(Object::as_dict)
            .and_then(|action| action.get_deref(b"URI", doc))
            .and_then(Object::as_str)
            .map(|b| String::from_utf8_lossy(b).to_string())
            .ok();
        let area = annot.get_deref(b"Rect", doc).ok().and_then(|o| rect(doc, o));
        let (Some(uri), Some(area)) = (uri, area) else { continue };
        let rect = area;
        if uri.trim().is_empty() {
            continue;
        }
        let (x0, y0) = transform.apply(rect[0], rect[1]);
        let (x1, y1) = transform.apply(rect[2], rect[3]);
        out.push(Link {
            rect: [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)],
            uri: Rc::from(uri.trim()),
        });
    }
    out
}

/// Tag each glyph with the link covering it. A glyph is inside a link when its
/// origin is, which is steadier than its full box: an annotation's rectangle is
/// drawn around the *rendered* text and often clips the last glyph's advance.
fn apply_links(glyphs: &mut [Glyph], links: &[Link]) {
    if links.is_empty() {
        return;
    }
    for glyph in glyphs {
        // Sideways text was rotated into its own frame and no longer shares
        // coordinates with the annotation rectangles.
        if glyph.turn != 0 {
            continue;
        }
        let (x, y) = (glyph.x + glyph.width / 2.0, glyph.y);
        if let Some(link) = links
            .iter()
            .find(|l| x >= l.rect[0] && x <= l.rect[2] && y >= l.rect[1] && y <= l.rect[3])
        {
            glyph.link = Some(link.uri.clone());
        }
    }
}
