/// Shared types, HTML parser, and helpers for all HTML chunking strategies.
///
/// Key feature vs plain-text formats: heading levels (1-6) are explicit in
/// the HTML tag name (h1-h6), so `heading_level` is stored on every block
/// without any heuristic detection.
use serde_json::{json, Value};

// Re-export shared utilities so strategy files can import from super::common.
pub use crate::shared::{
    has_keyword_overlap, split_at_sentences, split_sentences, tokenize_keywords,
};

pub const MAX_CHUNK_CHARS: usize = 1200;
pub const MIN_CHUNK_CHARS: usize = 350;

// ── Content types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    PlainParagraph,
    HeadingSection,
    BulletNumberedList,
    Table,
    CodeBlock,
    LongSingleParagraph,
    ShortDisconnectedParagraph,
    Semantic,
    Section,
    SlidingWindow,
    Sentence,
    PageAware,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::PlainParagraph => "plain_paragraph",
            ContentType::HeadingSection => "heading",
            ContentType::BulletNumberedList => "bullet_list",
            ContentType::Table => "table",
            ContentType::CodeBlock => "code_block",
            ContentType::LongSingleParagraph => "long_single_paragraph",
            ContentType::ShortDisconnectedParagraph => "short_disconnected_paragraph",
            ContentType::Semantic => "semantic",
            ContentType::Section => "section",
            ContentType::SlidingWindow => "sliding_window",
            ContentType::Sentence => "sentence",
            ContentType::PageAware => "page_aware",
        }
    }
}

// ── Block model ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlBlockType {
    Heading,
    Paragraph,
    Code,
    Table,
    List,
}

#[derive(Debug, Clone)]
pub struct HtmlBlock {
    pub block_type: HtmlBlockType,
    pub content: String,
    /// 1-6 for heading blocks, 0 for everything else.
    pub heading_level: u8,
    /// `true` when the list came from `<ol>`, `false` for `<ul>`.
    /// Always `false` for non-List blocks.
    pub is_ordered: bool,
}

// ── Chunk output ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChunkRecordInput {
    pub content_type: ContentType,
    pub content: String,
    pub metadata: Value,
}

// ── Heading helpers ───────────────────────────────────────────────────────────

fn heading_level_from_tag(tag: &str) -> u8 {
    match tag {
        "h1" => 1,
        "h2" => 2,
        "h3" => 3,
        "h4" => 4,
        "h5" => 5,
        "h6" => 6,
        _ => 0,
    }
}

pub fn update_heading_stack(stack: &mut Vec<(u8, String)>, level: u8, text: String) {
    stack.retain(|(l, _)| *l < level);
    stack.push((level, text));
}

pub fn current_section_heading(stack: &[(u8, String)]) -> Option<String> {
    stack.last().map(|(_, t)| t.clone())
}

pub fn current_section_level(stack: &[(u8, String)]) -> u8 {
    stack.last().map(|(l, _)| *l).unwrap_or(0)
}

pub fn heading_path_strings(stack: &[(u8, String)]) -> Vec<String> {
    stack.iter().map(|(_, t)| t.clone()).collect()
}

// ── Prose helpers ─────────────────────────────────────────────────────────────

pub fn classify_prose(text: &str) -> ContentType {
    if text.len() > 900 {
        ContentType::LongSingleParagraph
    } else if text.len() < 90 {
        ContentType::ShortDisconnectedParagraph
    } else {
        ContentType::PlainParagraph
    }
}

pub fn html_metadata(section_heading: Option<String>) -> Value {
    json!({
        "section_heading":    section_heading,
        "heading_path":       serde_json::Value::Null,
        "footnotes_captions": [],
        "page_number":        serde_json::Value::Null,
        "document_metadata":  { "source_type": "html" }
    })
}

pub fn html_metadata_with_path(
    section_heading: Option<String>,
    heading_path: Vec<String>,
    section_level: u8,
) -> Value {
    json!({
        "section_heading":    section_heading,
        "heading_path":       heading_path,
        "section_level":      section_level,
        "footnotes_captions": [],
        "page_number":        serde_json::Value::Null,
        "document_metadata":  { "source_type": "html" }
    })
}

// split_sentences, split_at_sentences — re-exported from crate::shared above.

// tokenize_keywords, has_keyword_overlap — re-exported from crate::shared above.

// ── Tag parser ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ParsedTag {
    pub name: String,
    pub is_closing: bool,
    pub is_self_closing: bool,
    pub end: usize,
}

pub fn parse_tag_at(s: &str, start: usize) -> Option<ParsedTag> {
    let bytes = s.as_bytes();
    if start >= bytes.len() || bytes[start] != b'<' {
        return None;
    }
    let mut i = start + 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let mut is_closing = false;
    if bytes[i] == b'/' {
        is_closing = true;
        i += 1;
    }
    let name_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = s[name_start..i].to_ascii_lowercase();
    while i < bytes.len() && bytes[i] != b'>' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let mut is_self_closing = i > start + 1 && bytes[i - 1] == b'/';
    if is_void_tag(&name) {
        is_self_closing = true;
    }
    Some(ParsedTag {
        name,
        is_closing,
        is_self_closing,
        end: i + 1,
    })
}

fn is_void_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Elements whose content is raw text, not markup.
///
/// A browser never parses tags inside these — it scans for the literal closing
/// tag. That distinction is load-bearing: `<script>if (a<b) {}</script>` has a
/// `<b` in it that *parses* as a tag whose `>` scan runs into `</script>`,
/// eating the real close. Tag-based matching then fails and the script body is
/// read as prose.
fn is_rawtext_element(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "textarea" | "title" | "xmp")
}

/// Find the end of a raw-text element by literal, case-insensitive search for
/// its closing tag — the way a browser tokenises these.
fn find_rawtext_end(s: &str, from: usize, tag: &str) -> Option<usize> {
    let hay = s.get(from..)?.as_bytes();
    let needle = format!("</{tag}");
    let n = needle.as_bytes();
    let mut i = 0usize;
    while i + n.len() <= hay.len() {
        if hay[i..i + n.len()].eq_ignore_ascii_case(n) {
            // Consume through the '>' that closes it.
            let mut j = from + i + n.len();
            while j < s.len() && s.as_bytes()[j] != b'>' {
                j += 1;
            }
            return Some((j + 1).min(s.len()));
        }
        i += 1;
    }
    None
}

pub fn find_matching_tag_end(s: &str, from: usize, tag: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut i = from;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Comments and declarations can contain anything, including a `</div>`
        // that is not a real close. Step over them whole.
        if let Some(after) = skip_non_element_markup(s, i) {
            i = after;
            continue;
        }
        // A `<` that starts no tag is ordinary text — CSS `a < b`, JS `i<n`, or
        // the `/*<![CDATA[*/` guard real stylesheets use. This used to be
        // `parse_tag_at(s, i)?`, so ONE such character abandoned the whole
        // search and the caller fell back to `tag.end` — skipping only the
        // opening tag and leaving the element's body to be read as prose.
        // `tika_testEPUB.epub` has exactly that: a `<Style>` whose CDATA guard
        // leaked its CSS, plus the words "nothing to see here" placed there by
        // Tika to catch this.
        let Some(parsed) = parse_tag_at(s, i) else {
            i += 1;
            continue;
        };
        if parsed.name == tag {
            if parsed.is_closing {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(parsed.end);
                }
            } else if !parsed.is_self_closing {
                depth += 1;
            }
        }
        i = parsed.end;
    }
    None
}

// ── Optional end tags ─────────────────────────────────────────────────────────

/// Elements that never have an end tag, so they must not be pushed on the
/// open-element stack while looking for a block's implicit end.
fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// True when a start tag `next` implicitly closes a still-open `open` element.
///
/// HTML has always allowed these end tags to be omitted, and HTML4 pages —
/// which is most hand-authored legacy markup — leave them out routinely.
/// The list follows the "optional tags" section of the HTML spec.
fn implicitly_closed_by(open: &str, next: &str) -> bool {
    match open {
        "p" => matches!(
            next,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "details"
                | "div"
                | "dl"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "header"
                | "hgroup"
                | "hr"
                | "main"
                | "menu"
                | "nav"
                | "ol"
                | "p"
                | "pre"
                | "section"
                | "table"
                | "ul"
        ),
        "li" => next == "li",
        "dt" | "dd" => matches!(next, "dt" | "dd"),
        "option" => matches!(next, "option" | "optgroup"),
        "tr" => next == "tr",
        "td" | "th" => matches!(next, "td" | "th" | "tr"),
        "thead" | "tbody" => matches!(next, "tbody" | "tfoot"),
        _ => false,
    }
}

/// Find where a block ends, tolerating an omitted end tag.
///
/// Returns `(end_exclusive, had_explicit_close)`. `end_exclusive` is just past
/// `</tag>` when there is one, and otherwise the offset of the markup that
/// implicitly closed it — so the caller can resume there without skipping it.
///
/// The scan keeps a stack of elements opened since `from`. A closing tag that
/// matches nothing on that stack must belong to an ancestor (`</div>`,
/// `</body>`), which ends the block; a closing tag that does match is ordinary
/// nested markup and is consumed. That distinction is what keeps `</a>` inside
/// `<p>Hello <a>link</a> world` from truncating the paragraph at "Hello".
pub fn find_block_end(s: &str, from: usize, tag: &str) -> (usize, bool) {
    if let Some(end) = find_matching_tag_end(s, from, tag) {
        return (end, true);
    }
    let bytes = s.as_bytes();
    let mut stack: Vec<String> = Vec::new();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(parsed) = parse_tag_at(s, i) else {
            i += 1;
            continue;
        };
        if parsed.is_closing {
            match stack.iter().rposition(|t| t == &parsed.name) {
                Some(pos) => stack.truncate(pos),
                // Unmatched close: it belongs to an ancestor, so we are past
                // the end of this block.
                None => return (i, false),
            }
        } else if stack.is_empty() && implicitly_closed_by(tag, &parsed.name) {
            return (i, false);
        } else if !parsed.is_self_closing && !is_void_element(&parsed.name) {
            stack.push(parsed.name.clone());
        }
        i = parsed.end;
    }
    (s.len(), false)
}

pub fn is_ignored_container(tag: &str) -> bool {
    matches!(
        tag,
        "script" | "style" | "noscript" | "head" | "svg" | "canvas"
    )
}

/// Tags that imply a line break when text around them is gathered loose.
///
/// Only used for separating loose text (see `parse_html_blocks`). Without it,
/// `<div>A</div><div>B</div>` gathers as `AB` — two words welded together.
/// Inline tags are deliberately absent so `<b>bold</b>face` stays one word, as
/// a browser renders it.
fn breaks_line_when_loose(tag: &str) -> bool {
    matches!(
        tag,
        "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "form"
            | "fieldset"
            | "figure"
            | "details"
            | "dialog"
            | "dl"
            | "li"
            | "tr"
            | "td"
            | "th"
            | "caption"
            | "br"
            | "hr"
            | "center"
            | "body"
    )
}

/// Skip markup that is not an element, returning the offset just past it.
///
/// Handles `<!-- comments -->`, `<![CDATA[ ... ]]>`, `<!DOCTYPE ...>` and
/// processing instructions / XML declarations `<? ... ?>`. Returns `None` when
/// `at` is not one of these, so the caller can try to parse an element.
///
/// An unterminated construct consumes the rest of the input, which is what a
/// browser does and is what keeps a truncated document from emitting its own
/// markup as text.
fn skip_non_element_markup(html: &str, at: usize) -> Option<usize> {
    let rest = html.get(at..)?;
    if let Some(body) = rest.strip_prefix("<!--") {
        return Some(match body.find("-->") {
            Some(k) => at + 4 + k + 3,
            None => html.len(),
        });
    }
    if let Some(body) = rest.strip_prefix("<![CDATA[") {
        return Some(match body.find("]]>") {
            Some(k) => at + 9 + k + 3,
            None => html.len(),
        });
    }
    if let Some(body) = rest.strip_prefix("<?") {
        // `?>` is the proper terminator; fall back to `>` for sloppy authors.
        return Some(match body.find("?>") {
            Some(k) => at + 2 + k + 2,
            None => match body.find('>') {
                Some(k) => at + 2 + k + 1,
                None => html.len(),
            },
        });
    }
    if rest.starts_with("<!") {
        return Some(match rest.find('>') {
            Some(k) => at + k + 1,
            None => html.len(),
        });
    }
    None
}

fn tag_to_block_type(tag: &str) -> Option<HtmlBlockType> {
    match tag {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(HtmlBlockType::Heading),
        "ul" | "ol" => Some(HtmlBlockType::List),
        "table" => Some(HtmlBlockType::Table),
        "pre" => Some(HtmlBlockType::Code),
        "p" | "blockquote" | "figcaption" | "summary" | "legend" | "label" | "textarea"
        | "option" | "button" | "address" | "dd" | "dt" | "aside" | "nav" => {
            Some(HtmlBlockType::Paragraph)
        }
        _ => None,
    }
}

// ── HTML block parser ─────────────────────────────────────────────────────────

pub fn parse_html_blocks(html: &str) -> Vec<HtmlBlock> {
    let mut blocks = Vec::new();
    let mut i = 0usize;
    let bytes = html.as_bytes();
    // Text that belongs to no recognised block tag. `tag_to_block_type` is a
    // whitelist, and everything outside it used to be dropped on the floor:
    // `<div>Hello</div>` produced ZERO chunks, and `<div>a</div><p>b</p>`
    // produced one chunk containing only "b" — a silent partial extraction that
    // still reported success. Modern pages are mostly div/section/span, so this
    // was a large class of documents returning nothing or a fragment.
    //
    // Unrecognised tags are already descended into rather than skipped, so the
    // only thing missing was gathering the text between them. Recognised blocks
    // still win: this flushes before one starts and resumes after it ends, so
    // nesting and document order are unchanged.
    let mut loose = String::new();

    macro_rules! flush_loose {
        () => {
            if !loose.trim().is_empty() {
                let text = normalize_inline_text(&strip_tags(&loose, true));
                if !text.is_empty() && !is_noise_text(&text) {
                    blocks.push(HtmlBlock {
                        block_type: HtmlBlockType::Paragraph,
                        content: text,
                        heading_level: 0,
                        is_ordered: false,
                    });
                }
            }
            loose.clear();
        };
    }

    while i < bytes.len() {
        if bytes[i] != b'<' {
            // Take the whole run to the next '<' at once. '<' is ASCII, so every
            // such position is a char boundary and multi-byte text is preserved.
            let start = i;
            while i < bytes.len() && bytes[i] != b'<' {
                i += 1;
            }
            loose.push_str(&html[start..i]);
            continue;
        }
        // Markup that is not an element: `<?xml ... ?>`, `<!DOCTYPE ...>`,
        // `<!-- ... -->`, `<![CDATA[ ... ]]>`. `parse_tag_at` cannot read these,
        // and until loose text was gathered it did not matter — the fallthrough
        // dropped them. Now it would swallow the declaration body as prose, so
        // each is skipped whole. (`?xml version="1.0" ...` really did show up as
        // a chunk of `tika_testEPUB.epub`.)
        if let Some(after) = skip_non_element_markup(html, i) {
            i = after;
            continue;
        }
        let Some(tag) = parse_tag_at(html, i) else {
            // A stray '<' that begins nothing: keep it as text, as before.
            loose.push('<');
            i += 1;
            continue;
        };
        if tag.is_closing {
            if breaks_line_when_loose(&tag.name) && !loose.ends_with('\n') {
                loose.push('\n');
            }
            i = tag.end;
            continue;
        }
        if is_ignored_container(&tag.name) {
            // Flush first: a <script> body must not be swept into the text
            // gathered around it.
            flush_loose!();
            i = if is_rawtext_element(&tag.name) {
                find_rawtext_end(html, tag.end, &tag.name)
            } else {
                find_matching_tag_end(html, tag.end, &tag.name)
            }
            .unwrap_or(html.len());
            continue;
        }
        if tag.name == "hr" {
            if !loose.ends_with('\n') {
                loose.push('\n');
            }
            i = tag.end;
            continue;
        }
        let Some(block_type) = tag_to_block_type(&tag.name) else {
            if breaks_line_when_loose(&tag.name) && !loose.ends_with('\n') {
                loose.push('\n');
            }
            i = tag.end;
            continue;
        };
        // A recognised block starts here, so any loose text before it is its own
        // paragraph and must be emitted first to keep document order.
        flush_loose!();
        // An omitted end tag used to discard the whole block: w3c_html40_cover.htm
        // yielded 611 characters out of 53 KB. Infer the implicit close instead.
        let (end, _explicit) = find_block_end(html, tag.end, &tag.name);
        if let Some(content) = extract_block_content(&html[i..end], &tag.name, block_type) {
            if !content.is_empty() {
                let heading_level = heading_level_from_tag(&tag.name);
                blocks.push(HtmlBlock {
                    block_type,
                    content,
                    heading_level,
                    is_ordered: tag.name == "ol",
                });
            }
        }
        i = end;
    }
    flush_loose!();
    bound_block_size(blocks)
}

/// Split any block still over the cap so a chunk cannot be unbounded (#68).
///
/// `Table` and `Code` are left whole: a table's rows carry its structure and it
/// is documented as "kept whole", and a code block's lines are its meaning.
/// Everything else is prose. `.epub` reuses these builders, so this bounds
/// EPUB too — `gutenberg_moby_dick.epub` had 112 chunks over the cap.
fn bound_block_size(blocks: Vec<HtmlBlock>) -> Vec<HtmlBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        if matches!(block.block_type, HtmlBlockType::Table | HtmlBlockType::Code)
            || block.content.chars().count() <= MAX_CHUNK_CHARS
        {
            out.push(block);
            continue;
        }
        for part in
            crate::shared::split_block_on_lines_and_sentences(&block.content, MAX_CHUNK_CHARS)
        {
            out.push(HtmlBlock {
                block_type: block.block_type,
                content: part,
                heading_level: block.heading_level,
                is_ordered: block.is_ordered,
            });
        }
    }
    out
}

// ── Block content extraction ──────────────────────────────────────────────────

pub fn extract_block_content(raw: &str, tag: &str, block_type: HtmlBlockType) -> Option<String> {
    match block_type {
        HtmlBlockType::Heading | HtmlBlockType::Paragraph => {
            let inner = extract_inner_html(raw, tag)?;
            let text = normalize_inline_text(&strip_tags(&inner, true));
            if text.is_empty() || is_noise_text(&text) {
                None
            } else {
                Some(text)
            }
        }
        HtmlBlockType::Code => {
            let inner = extract_inner_html(raw, tag)?;
            let text = normalize_code_text(&strip_tags(&inner, false));
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        HtmlBlockType::List => {
            let mut items = Vec::new();
            for li_raw in extract_tag_blocks(raw, "li") {
                if let Some(inner) = extract_inner_html(li_raw, "li") {
                    let item = normalize_inline_text(&strip_tags(&inner, true));
                    if !item.is_empty() {
                        items.push(item);
                    }
                }
            }
            if items.is_empty() {
                None
            } else {
                Some(items.join("\n"))
            }
        }
        HtmlBlockType::Table => render_table(raw),
    }
}

/// Blocks of `tag` that are *direct* content of `html` — occurrences inside a
/// nested `container` are skipped, along with the container itself.
///
/// `extract_tag_blocks` scans at any depth, which is right for a flat document
/// and wrong inside a table: a nested table's `<tr>`s were collected as extra
/// rows of the parent (TECH_DEBT #24).
fn find_block_end_skipping(html: &str, from: usize, tag: &str, container: &str) -> usize {
    let bytes = html.as_bytes();
    let mut i = from;
    let mut depth = 1usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(parsed) = parse_tag_at(html, i) else {
            i += 1;
            continue;
        };
        if tag != container
            && !parsed.is_closing
            && !parsed.is_self_closing
            && parsed.name == container
        {
            let end = find_block_end_skipping(html, parsed.end, container, container);
            i = if end > i { end } else { parsed.end };
            continue;
        }
        // `<td/>` opens and closes at once, so it must not raise the depth.
        // Missing this merged Moby Dick's `<td/><td>word</td><td>lang</td>`
        // rows into single cells — real XHTML uses empty self-closing cells for
        // layout indentation all the time.
        if parsed.name == tag && !parsed.is_self_closing {
            if parsed.is_closing {
                depth -= 1;
                if depth == 0 {
                    return parsed.end;
                }
            } else {
                depth += 1;
            }
        }
        i = parsed.end;
    }
    html.len()
}

fn extract_blocks_skipping_nested<'a>(html: &'a str, tag: &str, container: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(parsed) = parse_tag_at(html, i) else {
            i += 1;
            continue;
        };
        if tag != container
            && !parsed.is_closing
            && !parsed.is_self_closing
            && parsed.name == container
        {
            // Jump the whole nested container; nothing inside belongs to us.
            let end = find_block_end_skipping(html, parsed.end, container, container);
            i = if end > i { end } else { parsed.end };
            continue;
        }
        // A self-closing `<td/>` is an empty cell: it has no block to extract,
        // and treating it as an opening tag swallows the cells that follow.
        if parsed.is_closing || parsed.is_self_closing || parsed.name != tag {
            i = parsed.end;
            continue;
        }
        // A `</tag>` belonging to a nested container must not close this block:
        // the outer `<tr>` used to end at the inner table's first `</tr>`.
        let end = find_block_end_skipping(html, parsed.end, tag, container);
        if end <= i {
            i = parsed.end;
            continue;
        }
        out.push(&html[i..end]);
        i = end;
    }
    out
}

/// Render one `<table>` as `cell | cell` rows, one row per line.
///
/// Nested tables are rendered in place, inside the cell that holds them,
/// instead of leaking their rows into the parent and losing their cell
/// separators (TECH_DEBT #24).
fn render_table(raw: &str) -> Option<String> {
    render_table_with(raw, "\n")
}

/// `row_sep` is `"\n"` at the top level, so one row reads as one line. A table
/// nested inside a cell uses `" / "` instead: its rows must stay on the parent
/// row's line, or a single outer row looks like several.
fn render_table_with(raw: &str, row_sep: &str) -> Option<String> {
    let inner = extract_inner_html(raw, "table").unwrap_or_else(|| raw.to_string());
    let mut rows = Vec::new();
    for tr_raw in extract_blocks_skipping_nested(&inner, "tr", "table") {
        let mut cells = Vec::new();
        for cell_tag in ["th", "td"] {
            for cell_raw in extract_blocks_skipping_nested(tr_raw, cell_tag, "table") {
                let Some(cell_inner) = extract_inner_html(cell_raw, cell_tag) else {
                    continue;
                };
                // A cell may hold its own table; render it rather than letting
                // strip_tags run its cells together.
                let mut parts: Vec<String> = Vec::new();
                let nested_tables = extract_tag_blocks(&cell_inner, "table");
                // The cell's own text excludes its nested tables — otherwise
                // their cells appear twice: once run together by strip_tags,
                // once rendered properly below.
                let mut own_text = cell_inner.clone();
                for nested in &nested_tables {
                    own_text = own_text.replace(*nested, " ");
                }
                let text = normalize_inline_text(&strip_tags(&own_text, true));
                if !text.is_empty() {
                    parts.push(text);
                }
                for nested in &nested_tables {
                    if let Some(rendered) = render_table_with(nested, " / ") {
                        parts.push(rendered);
                    }
                }
                if !parts.is_empty() {
                    cells.push(parts.join(" "));
                }
            }
        }
        if !cells.is_empty() {
            rows.push(cells.join(" | "));
        }
    }
    if rows.is_empty() {
        None
    } else {
        Some(rows.join(row_sep))
    }
}

pub fn extract_tag_blocks<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(parsed) = parse_tag_at(html, i) else {
            i += 1;
            continue;
        };
        if parsed.is_closing || parsed.name != tag {
            i = parsed.end;
            continue;
        }
        let (end, _explicit) = find_block_end(html, parsed.end, tag);
        if end <= i {
            i = parsed.end;
            continue;
        }
        out.push(&html[i..end]);
        i = end;
    }
    out
}

fn extract_inner_html(raw: &str, tag: &str) -> Option<String> {
    let open = parse_tag_at(raw, 0)?;
    if open.is_closing || open.name != tag {
        return None;
    }
    // With the end tag omitted, `raw` was already cut at the implicit
    // boundary by find_block_end, so everything after the open tag is content.
    let close = find_last_close_tag(raw, tag).unwrap_or(raw.len());
    Some(raw[open.end..close.max(open.end)].to_string())
}

fn find_last_close_tag(html: &str, tag: &str) -> Option<usize> {
    let close = format!("</{tag}");
    html.to_ascii_lowercase().rfind(&close)
}

// ── Text cleaning ─────────────────────────────────────────────────────────────

pub fn strip_tags(input: &str, newline_for_breaks: bool) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(tag) = parse_tag_at(input, i) {
                if newline_for_breaks && matches!(tag.name.as_str(), "br" | "p" | "li" | "tr") {
                    out.push('\n');
                }
                i = tag.end;
                continue;
            }
            // Unparseable '<' — emit it and advance one byte (safe: '<' is ASCII).
            out.push('<');
            i += 1;
            continue;
        }
        // Non-tag run: advance until the next '<' and push the slice as-is.
        // This preserves multi-byte UTF-8 sequences correctly — we never cast
        // individual bytes to char.  '<' is ASCII so byte positions of '<'
        // are always valid char boundaries in a UTF-8 string.
        let text_start = i;
        while i < bytes.len() && bytes[i] != b'<' {
            i += 1;
        }
        out.push_str(&input[text_start..i]);
    }
    decode_html_entities(&out)
}

pub fn normalize_inline_text(input: &str) -> String {
    input
        .lines()
        .map(collapse_whitespace)
        .map(|line| strip_angle_wrapped_tokens(&line))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_angle_wrapped_tokens(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '<' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '>' {
                j += 1;
            }
            if j < chars.len() && j > i + 1 {
                let inner: String = chars[i + 1..j].iter().collect();
                if inner
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_'))
                {
                    out.push_str(inner.trim_matches('/'));
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    collapse_whitespace(&out)
}

pub fn normalize_code_text(input: &str) -> String {
    input
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_space = false;
    for c in input.chars() {
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out.trim().to_string()
}

pub fn is_noise_text(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || t.chars()
            .all(|c| matches!(c, '|' | '-' | '_' | '*' | '=' | '.'))
}

pub(crate) fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '&' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && j - i <= 12 && chars[j] != ';' {
            j += 1;
        }
        if j < chars.len() && chars[j] == ';' {
            let entity: String = chars[i + 1..j].iter().collect();
            if let Some(decoded) = decode_entity(&entity) {
                out.push_str(&decoded);
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn decode_entity(entity: &str) -> Option<String> {
    match entity {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" | "#39" => Some("'".to_string()),
        // These four are deliberately lossy: HTML is flattened to plain text,
        // and a non-breaking space or a bullet glyph reads better normalised.
        // Everything else should be its real character.
        "nbsp" => Some(" ".to_string()),
        "bull" => Some("*".to_string()),
        "mdash" | "ndash" | "minus" => Some("-".to_string()),
        _ => {
            // Numeric references and the rest of the named table live in the
            // shared resolver, so HTML gets the same coverage as the XML
            // walkers instead of its own short list — "&copy;" used to survive
            // into the output verbatim.
            let resolved = crate::entities::resolve_entity(entity);
            // The resolver echoes "&name;" back for anything it doesn't know;
            // returning None there keeps the original text untouched.
            (resolved != format!("&{entity};")).then_some(resolved)
        }
    }
}

pub fn remove_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"<!--") {
            // Skip everything inside the comment.
            i += 4;
            while i < bytes.len() && !bytes[i..].starts_with(b"-->") {
                i += 1;
            }
            if bytes[i..].starts_with(b"-->") {
                i += 3;
            }
            continue;
        }
        // Non-comment run: advance until the next comment start and push as-is.
        // '<' is ASCII so byte positions of "<!--" are valid char boundaries.
        let text_start = i;
        while i < bytes.len() && !bytes[i..].starts_with(b"<!--") {
            i += 1;
        }
        out.push_str(&input[text_start..i]);
    }
    out
}
