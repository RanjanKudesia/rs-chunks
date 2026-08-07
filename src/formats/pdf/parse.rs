//! The PDF parser's front door: bytes in, markdown and images out.
//!
//! [`Reader`] hands out one page of markdown at a time, which is what the
//! streaming entry point walks.
//!
//! It is not, however, lazy about *parsing*: heading levels come from size,
//! ranked across the whole document, so every page must be read before any can
//! be rendered. Ranking from a prefix instead was tried and measured — it
//! reclassified `pdfjs_freeculture`'s licence page as a run of headings — so
//! the whole-document pass stays, and the streaming profile says so plainly
//! rather than claiming an incrementality the algorithm has not got.
//!
//! An image is only decoded when the caller asks for bytes. The markdown's
//! `![](…)` references are decided from the image's filter name alone, so
//! `get_markdown` and `get_markdown(list_images)` cannot disagree about what a
//! page contains.

use std::collections::{HashMap, VecDeque};

use lopdf::{Document, ObjectId};

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
    /// Populated for diagnostics; no current consumer reads it.
    #[allow(dead_code)]
    pub skipped: Vec<String>,
}

struct PageLayout {
    regions: Vec<Vec<Line>>,
    images: Vec<PlacedImage>,
}

/// How heading levels are decided, which is the whole difference between the
/// `default` and `structural` modes.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Headings {
    /// Rank type sizes across the whole document. Every page must be read
    /// before any can be rendered, so this holds the document in memory.
    Ranked,
    /// Rank within each page as it is read — "minimal font analysis", the
    /// documented `default`. No document-wide pass, so a page is parsed,
    /// rendered and dropped.
    PerPage,
}

/// Reads a PDF a page at a time, rendering each to markdown.
pub(crate) struct Reader {
    document: Document,
    extractor: Extractor,
    page_ids: Vec<ObjectId>,
    /// The document-wide ranking, or `None` when each page ranks its own.
    style: Option<Style>,
    /// Pages parsed while sampling the style, waiting to be rendered.
    sampled: VecDeque<PageLayout>,
    /// Index of the next page to *parse*; the sampled ones are already done.
    next_to_parse: usize,
    /// Index of the next page to *render*, which is its page number − 1.
    next_to_render: usize,
    names: HashMap<ObjectId, String>,
    skipped: Vec<String>,
    has_text: bool,
}

impl Reader {
    pub fn open(bytes: &[u8], headings: Headings) -> Result<Reader, String> {
        let document = doc::open(bytes)?;
        let page_ids: Vec<ObjectId> = document.get_pages().into_values().collect();
        let mut extractor = Extractor::new();

        // Ranked: every page is read before any is rendered, because the
        // ranking is a property of the whole document. Sampling a prefix
        // instead was tried and measured — it reclassified
        // `pdfjs_freeculture`'s licence page as headings — so the memory it
        // saved cost correctness.
        let mut sampled = VecDeque::new();
        let mut style = None;
        let mut parsed = 0;
        if headings == Headings::Ranked {
            sampled.reserve(page_ids.len());
            let mut lines: Vec<Line> = Vec::new();
            for id in &page_ids {
                let page = doc::read_page(&mut extractor, &document, *id);
                let regions = regions::split(&page.content.glyphs);
                lines.extend(regions.iter().flatten().cloned());
                sampled.push_back(PageLayout { regions, images: page.content.images });
            }
            style = Some(Style::of(&lines));
            parsed = page_ids.len();
        }

        Ok(Reader {
            document,
            extractor,
            page_ids,
            style,
            sampled,
            next_to_parse: parsed,
            next_to_render: 0,
            names: HashMap::new(),
            skipped: Vec::new(),
            has_text: false,
        })
    }

    pub fn total_pages(&self) -> usize {
        self.page_ids.len()
    }

    pub fn has_text(&self) -> bool {
        self.has_text
    }

    /// Pages rendered so far — what a streaming caller has actually paid for.
    // Diagnostic accessor for streaming callers; not wired into the public API.
    #[allow(dead_code)]
    pub fn pages_rendered(&self) -> usize {
        self.next_to_render
    }

    /// Render the next page, or `None` once every page has been read. A blank
    /// page yields an empty string rather than being skipped, so the caller
    /// decides what an empty page means.
    pub fn next_page(&mut self) -> Option<String> {
        if self.next_to_render >= self.page_ids.len() {
            return None;
        }
        let layout = match self.sampled.pop_front() {
            Some(layout) => layout,
            None => {
                let id = self.page_ids[self.next_to_parse];
                self.next_to_parse += 1;
                let page = doc::read_page(&mut self.extractor, &self.document, id);
                PageLayout { regions: regions::split(&page.content.glyphs), images: page.content.images }
            }
        };
        let page_number = self.next_to_render + 1;
        self.next_to_render += 1;

        // Without a document-wide ranking, the page ranks its own sizes. That
        // is what makes `default` cheap: no pass over the document, and each
        // page is dropped as soon as it is rendered.
        let page_style = match self.style {
            Some(_) => None,
            None => {
                let lines: Vec<Line> = layout.regions.iter().flatten().cloned().collect();
                Some(Style::of(&lines))
            }
        };
        let style = self.style.as_ref().or(page_style.as_ref()).expect("a ranking");

        let mut items: Vec<(f32, Item)> = Vec::new();
        for region in &layout.regions {
            let top = region.first().map(|l| l.baseline).unwrap_or(0.0);
            for block in blocks::build(region, style) {
                self.has_text = true;
                items.push((top, Item::Block(block)));
            }
        }
        place_images(
            &mut items,
            layout.images,
            page_number,
            &self.document,
            &mut self.names,
            &mut self.skipped,
        );
        Some(markdown::page(&items.into_iter().map(|(_, item)| item).collect::<Vec<_>>()))
    }

    /// Decode every image the markdown referenced. Only meaningful once the
    /// pages have been read, since reading them is what discovers the images.
    pub fn take_images(&mut self) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = Vec::with_capacity(self.names.len());
        for (id, name) in &self.names {
            match images::extract(&self.document, *id) {
                Ok(image) => out.push((name.clone(), image.bytes)),
                Err(reason) => self.skipped.push(format!("{name}: {reason}")),
            }
        }
        // Object ids iterate in hash order; the markdown's own order is the
        // document's, and the two must agree run to run.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn take_skipped(&mut self) -> Vec<String> {
        std::mem::take(&mut self.skipped)
    }
}

pub(crate) fn parse(bytes: &[u8], want_images: bool, headings: Headings) -> Result<Parsed, String> {
    let mut reader = Reader::open(bytes, headings)?;
    let total_pages = reader.total_pages();

    let mut markdown = String::new();
    while let Some(page) = reader.next_page() {
        if page.trim().is_empty() {
            continue;
        }
        if !markdown.is_empty() {
            markdown.push_str(PAGE_SEPARATOR);
        }
        markdown.push_str(&page);
    }

    let images = if want_images { reader.take_images() } else { Vec::new() };
    Ok(Parsed {
        markdown,
        images,
        total_pages,
        has_text: reader.has_text(),
        skipped: reader.take_skipped(),
    })
}

/// Insert each image into the block stream at the height it is drawn.
fn place_images(
    items: &mut Vec<(f32, Item)>,
    mut placed: Vec<PlacedImage>,
    page_number: usize,
    document: &Document,
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
        let position = items.iter().position(|(top, _)| *top < image.top).unwrap_or(items.len());
        items.insert(position, (image.top, Item::Image(name.clone())));
    }
}
