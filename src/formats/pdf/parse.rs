//! The PDF parser's front door: bytes in, markdown and images out.
//!
//! Heading levels are ranked over the *whole* document, so lines are collected
//! for every page before any of them is rendered. That is also why an image is
//! only decoded when the caller asks for bytes — the markdown's `![](…)`
//! references are decided from the image's filter name alone, so `get_markdown`
//! and `get_markdown(list_images)` cannot disagree about what a page contains.

use std::collections::HashMap;

use lopdf::ObjectId;

use super::blocks::{self, Style};
use super::content::{Extractor, PlacedImage};
use super::doc;
use super::images;
use super::lines::Line;
use super::markdown::{self, Item, PAGE_SEPARATOR};
use super::regions;

pub(crate) struct Parsed {
    pub markdown: String,
    pub images: Vec<(String, Vec<u8>)>,
    pub total_pages: usize,
    /// Whether any *text* was extracted. A document whose pages hold nothing but
    /// pictures still renders `![](…)` references, so the markdown being
    /// non-empty is not the same question as the PDF having text.
    pub has_text: bool,
    /// Images present on a page but left out of `images`, with the reason.
    pub skipped: Vec<String>,
}

struct PageLayout {
    regions: Vec<Vec<Line>>,
    images: Vec<PlacedImage>,
}

pub(crate) fn parse(bytes: &[u8], want_images: bool) -> Result<Parsed, String> {
    let document = doc::open(bytes)?;
    let page_ids: Vec<ObjectId> = document.get_pages().into_values().collect();
    let total_pages = page_ids.len();

    let mut extractor = Extractor::new(&document);
    let mut layouts: Vec<PageLayout> = Vec::with_capacity(total_pages);
    for id in &page_ids {
        let page = doc::read_page(&mut extractor, &document, *id);
        layouts.push(PageLayout { regions: regions::split(&page.content.glyphs), images: page.content.images });
    }

    let all: Vec<Line> = layouts.iter().flat_map(|p| p.regions.iter().flatten().cloned()).collect();
    let style = Style::of(&all);
    drop(all);

    let mut names: HashMap<ObjectId, String> = HashMap::new();
    let mut collected: Vec<(String, Vec<u8>)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut pages: Vec<String> = Vec::with_capacity(total_pages);

    let mut has_text = false;
    for (index, layout) in layouts.into_iter().enumerate() {
        let mut items: Vec<(f32, Item)> = Vec::new();
        for region in &layout.regions {
            let top = region.first().map(|l| l.baseline).unwrap_or(0.0);
            for block in blocks::build(region, &style) {
                has_text = true;
                items.push((top, Item::Block(block)));
            }
        }
        place_images(&mut items, layout.images, index + 1, &document, &mut names, &mut skipped);
        let rendered = markdown::page(&items.into_iter().map(|(_, item)| item).collect::<Vec<_>>());
        if !rendered.trim().is_empty() {
            pages.push(rendered);
        }
    }

    if want_images {
        for (id, name) in &names {
            match images::extract(&document, *id) {
                Ok(image) => collected.push((name.clone(), image.bytes)),
                Err(reason) => skipped.push(format!("{name}: {reason}")),
            }
        }
        // Object ids iterate in hash order; the markdown's own order is the
        // document's, and the two must agree run to run.
        collected.sort_by(|a, b| a.0.cmp(&b.0));
    }

    Ok(Parsed { markdown: pages.join(PAGE_SEPARATOR), images: collected, total_pages, has_text, skipped })
}

/// Insert each image into the block stream at the height it is drawn.
fn place_images(
    items: &mut Vec<(f32, Item)>,
    mut placed: Vec<PlacedImage>,
    page_number: usize,
    document: &lopdf::Document,
    names: &mut HashMap<ObjectId, String>,
    skipped: &mut Vec<String>,
) {
    placed.sort_by(|a, b| {
        b.top.partial_cmp(&a.top).unwrap_or(std::cmp::Ordering::Equal).then(
            a.left.partial_cmp(&b.left).unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    let mut ordinal = 0usize;
    for image in placed {
        // A codec we cannot decode gets no reference either, so the markdown
        // never promises a file that `list_images` will not return.
        if let Some(reason) = images::undecodable(document, image.id) {
            skipped.push(format!("page {page_number}: {reason}"));
            continue;
        }
        ordinal += 1;
        let name = names.entry(image.id).or_insert_with(|| {
            let extension = images::extension_of(document, image.id);
            format!("image_p{page_number}_{ordinal}.{extension}")
        });
        let position = items
            .iter()
            .position(|(top, _)| *top < image.top)
            .unwrap_or(items.len());
        items.insert(position, (image.top, Item::Image(name.clone())));
    }
}
