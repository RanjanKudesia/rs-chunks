//! The [MS-PPT] §2.1.2 liveness layer: which records are CURRENT.
//!
//! A `.ppt` is edited in place. Each save appends records and a `UserEditAtom`,
//! and the *only* map of what is live is the persist directory reached from the
//! `Current User` stream. The engine's reader was a linear scanner with no
//! liveness layer at all, so records superseded by later saves were emitted as
//! current content: `poi_47261.ppt` reported `total_slides` **305** against
//! **14** live (22 saves × one `SlideListWithText` each, concatenated), and
//! deleted slide text — content the author believed removed — reached the
//! output. Measured against all 28 corpus fixtures before implementation; the
//! full derivation is the review lane's `spec_ppt_persist.md`.
//!
//! Resolution ([MS-PPT] §2.1.2): `CurrentUserAtom.offsetToCurrentEdit` → the
//! newest `UserEditAtom` → follow `offsetLastEdit` back through every save,
//! reading each edit's `PersistDirectoryAtom`; **newer edits win** on a persist
//! id. (Spec phrases it oldest-first-overwrite; newest-first with
//! `or_insert` is the same function — verified identical on 28/28 fixtures.)
//! The newest edit's `docPersistIdRef` then names the live
//! `DocumentContainer`, whose `SlideListWithText` (instance 0) lists the live
//! slides **in presentation order** — which is NOT stream order in 3 of 28
//! fixtures.
//!
//! Every failure path degrades to [`LiveModel::Fallback`], which callers treat
//! as "scan the whole stream" — the exact previous behaviour — because a
//! readable-but-odd `.ppt` must not become an unreadable one.

use std::collections::{HashMap, HashSet};

use super::records::{parse_header, RecordHeader, REC_VER_CONTAINER};

const RT_CURRENT_USER_ATOM: u16 = 0x0FF6;
const RT_USER_EDIT_ATOM: u16 = 0x0FF5;
const RT_PERSIST_DIRECTORY: u16 = 0x1772;
const RT_DOCUMENT_CONTAINER: u16 = 0x03E8;
const RT_SLIDE_CONTAINER: u16 = 0x03EE;
const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
const RT_SLIDE_PERSIST_ATOM: u16 = 0x03F3;

/// `headerToken` of an unencrypted deck. Anything else (including the
/// encrypted token) falls back — this layer never decrypts.
const TOKEN_PLAIN: u32 = 0xE391_C05F;

/// Attacker-reachable walk: bound the chain far above the corpus maximum (22).
const MAX_EDITS: usize = 4096;
const MAX_DEPTH: usize = 32;

pub(super) enum LiveModel {
    /// The persist directory resolved. Offsets index the document stream.
    Persist {
        /// Body range of the live `DocumentContainer` (0x03E8).
        document_body: (usize, usize),
        /// Live `SlideContainer` (0x03EE) offsets, in PRESENTATION order.
        slide_offsets: Vec<usize>,
    },
    /// Entry point missing or unusable: callers scan the whole stream, which
    /// is byte-for-byte the pre-liveness behaviour.
    Fallback,
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Header at an exact offset, verified against an expected type.
fn header_at(stream: &[u8], off: usize, want: u16) -> Option<RecordHeader> {
    let (h, _) = parse_header(stream, off, stream.len())?;
    (h.rec_type == want).then_some(h)
}

/// Depth-bounded search for the first descendant record of `want` type (and
/// instance, when given) inside a container body.
fn find_in(
    stream: &[u8],
    start: usize,
    end: usize,
    want: u16,
    want_instance: Option<u16>,
    depth: usize,
) -> Option<RecordHeader> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut pos = start;
    while let Some((h, next)) = parse_header(stream, pos, end) {
        if h.rec_type == want && want_instance.is_none_or(|i| h.rec_instance == i) {
            return Some(h);
        }
        if h.rec_ver == REC_VER_CONTAINER {
            if let Some(found) =
                find_in(stream, h.body_start, h.body_end, want, want_instance, depth + 1)
            {
                return Some(found);
            }
        }
        if next <= pos {
            break;
        }
        pos = next;
    }
    None
}

/// Resolve the live model. `current_user` is the `Current User` stream, when
/// the container has one. Infallible by design: anything unexpected → Fallback.
pub(super) fn resolve(stream: &[u8], current_user: Option<&[u8]>) -> LiveModel {
    match try_resolve(stream, current_user) {
        Some(m) => m,
        None => LiveModel::Fallback,
    }
}

fn try_resolve(stream: &[u8], current_user: Option<&[u8]>) -> Option<LiveModel> {
    let cu = current_user?;
    // CurrentUserAtom: body = [size:4][headerToken:4][offsetToCurrentEdit:4]…
    // Field reads are bounded by recLen, never the stream length — one corpus
    // fixture declares a 4096-byte stream for a 45-byte atom.
    let h = header_at(cu, 0, RT_CURRENT_USER_ATOM)?;
    let token = u32_at(cu, h.body_start + 4)?;
    if token != TOKEN_PLAIN {
        return None;
    }
    let mut edit_off = u32_at(cu, h.body_start + 8)? as usize;

    // Walk the edit chain newest→oldest; first mapping for a persist id wins.
    let mut persist: HashMap<u32, u32> = HashMap::new();
    let mut visited: HashSet<usize> = HashSet::new();
    let mut doc_persist_id: Option<u32> = None;
    for _ in 0..MAX_EDITS {
        if edit_off == 0 || !visited.insert(edit_off) {
            break;
        }
        let eh = header_at(stream, edit_off, RT_USER_EDIT_ATOM)?;
        let prev = u32_at(stream, eh.body_start + 8)? as usize;
        let dir_off = u32_at(stream, eh.body_start + 12)? as usize;
        if doc_persist_id.is_none() {
            doc_persist_id = Some(u32_at(stream, eh.body_start + 16)?);
        }
        // PersistDirectoryAtom: repeated [packed u32][cPersist × u32 offset].
        // The 20/12 bit split is the parsing trap — two u16s corrupt every id
        // above 0xFFFF.
        let dh = header_at(stream, dir_off, RT_PERSIST_DIRECTORY)?;
        let mut pos = dh.body_start;
        while pos + 4 <= dh.body_end {
            let packed = u32_at(stream, pos)?;
            let first_id = packed & 0x000F_FFFF;
            let count = ((packed >> 20) & 0x0FFF) as usize;
            pos += 4;
            for i in 0..count {
                if pos + 4 > dh.body_end {
                    break;
                }
                let off = u32_at(stream, pos)?;
                persist.entry(first_id + i as u32).or_insert(off);
                pos += 4;
            }
        }
        edit_off = prev;
    }

    // Live DocumentContainer, verified by record type before trusting it.
    let doc_off = *persist.get(&doc_persist_id?)? as usize;
    let dh = header_at(stream, doc_off, RT_DOCUMENT_CONTAINER)?;
    let document_body = (dh.body_start, dh.body_end);

    // Live slides: the SLWT (instance 0) inside the live document, in array
    // order. A persistIdRef of 0 or one missing from the map means "skip", not
    // an error; a resolved offset must actually be a SlideContainer.
    let mut slide_offsets = Vec::new();
    if let Some(slwt) = find_in(
        stream,
        document_body.0,
        document_body.1,
        RT_SLIDE_LIST_WITH_TEXT,
        Some(super::records::SLWT_INSTANCE_SLIDES),
        0,
    ) {
        let mut pos = slwt.body_start;
        while let Some((ch, next)) = parse_header(stream, pos, slwt.body_end) {
            if ch.rec_type == RT_SLIDE_PERSIST_ATOM {
                if let Some(pid) = u32_at(stream, ch.body_start) {
                    if pid != 0 {
                        if let Some(&off) = persist.get(&pid) {
                            if header_at(stream, off as usize, RT_SLIDE_CONTAINER).is_some() {
                                slide_offsets.push(off as usize);
                            }
                        }
                    }
                }
            }
            if next <= pos {
                break;
            }
            pos = next;
        }
    }

    Some(LiveModel::Persist {
        document_body,
        slide_offsets,
    })
}
