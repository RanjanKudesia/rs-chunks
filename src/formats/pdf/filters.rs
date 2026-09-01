//! Stream filters for image XObjects.
//!
//! lopdf refuses to decode a stream whose `/Subtype` is `/Image` — its own
//! decoder covers text streams only — so images are unwrapped here. The
//! byte-oriented filters (Flate, LZW, ASCII, run-length) are applied in order
//! until an *image* codec is reached, at which point the remaining bytes are the
//! encoded picture and are handed on as-is.

use lopdf::{Dictionary, Document, Object, Stream};

/// What the bytes are once the byte-oriented filters have been peeled off.
#[derive(Debug, PartialEq)]
pub(crate) enum Codec {
    /// Raw samples, to be interpreted with the image dictionary's colour space.
    Samples,
    /// A complete JPEG file.
    Jpeg,
    /// A complete JPEG 2000 file.
    Jpeg2000,
    /// A codec we do not decode; named so the caller can say which.
    Unsupported(String),
}

pub(crate) fn decode(doc: &Document, stream: &Stream) -> Result<(Vec<u8>, Codec), String> {
    let filters = match stream.dict.get(b"Filter") {
        Ok(Object::Name(n)) => vec![String::from_utf8_lossy(n).to_string()],
        Ok(Object::Array(a)) => a
            .iter()
            .filter_map(|o| doc.dereference(o).ok())
            .filter_map(|(_, o)| o.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).to_string())
            .collect(),
        _ => Vec::new(),
    };
    let params = decode_parms(doc, stream, filters.len());

    let mut data = stream.content.clone();
    for (i, filter) in filters.iter().enumerate() {
        let parms = params.get(i).cloned().flatten();
        data = match filter.as_str() {
            "FlateDecode" | "Fl" => inflate(&data)?,
            "LZWDecode" | "LZW" => lzw(&data, parms.as_ref(), doc)?,
            "ASCII85Decode" | "A85" => ascii85(&data),
            "ASCIIHexDecode" | "AHx" => ascii_hex(&data),
            "RunLengthDecode" | "RL" => run_length(&data),
            "DCTDecode" | "DCT" => return Ok((data, Codec::Jpeg)),
            "JPXDecode" => return Ok((data, Codec::Jpeg2000)),
            other => return Ok((data, Codec::Unsupported(other.to_string()))),
        };
        if let Some(parms) = parms {
            data = unpredict(doc, &data, &parms)?;
        }
    }
    Ok((data, Codec::Samples))
}

/// `/DecodeParms` is a dictionary when `/Filter` is a name, and an array
/// positionally matching it otherwise. Null entries are allowed and common.
fn decode_parms(doc: &Document, stream: &Stream, count: usize) -> Vec<Option<Dictionary>> {
    let raw = stream
        .dict
        .get(b"DecodeParms")
        .or_else(|_| stream.dict.get(b"DP"))
        .ok()
        .and_then(|o| doc.dereference(o).ok())
        .map(|(_, o)| o.clone());
    match raw {
        Some(Object::Dictionary(d)) => vec![Some(d)],
        Some(Object::Array(items)) => items
            .iter()
            .map(|o| {
                doc.dereference(o)
                    .ok()
                    .and_then(|(_, o)| o.as_dict().ok().cloned())
            })
            .collect(),
        _ => vec![None; count],
    }
}

/// A single decode stage may not produce more than this.
///
/// The floor is set by real files, not taste. Measured across the 48-fixture PDF
/// corpus, the largest legitimately decompressed stream is **55.5 MB**
/// (`arxiv_2005.14165_gpt3.pdf`, 375 KB of Flate at 155:1), so 256 MiB is ~4.6x
/// the worst real case while still refusing the classic bomb — a few KB of
/// Flate otherwise expands without limit, and `read_to_end` into an unbounded
/// `Vec` is an OOM *abort*, which `catch_unwind` cannot intercept.
///
/// A *ratio* cap is deliberately absent, and this is the interesting part:
/// DEFLATE's theoretical maximum is 1032:1 and the same corpus contains a
/// legitimate **1025.6:1** stream. Every ratio bound low enough to be useful
/// breaks a real file, and every one high enough to be safe constrains nothing.
///
/// The cap is applied per stage inside the filter loop, so a chained
/// `/Filter [/Fl /Fl /Fl]` is bounded at every step rather than compounding.
const MAX_DECODED_STREAM_BYTES: usize = 256 * 1024 * 1024;

fn over_cap(out: Vec<u8>, filter: &str) -> Result<Vec<u8>, String> {
    if out.len() > MAX_DECODED_STREAM_BYTES {
        return Err(format!(
            "{filter} output exceeds the {MAX_DECODED_STREAM_BYTES}-byte stream cap"
        ));
    }
    Ok(out)
}

/// A `Write` sink that refuses to grow past `limit`.
///
/// `weezl`'s `Decoder::decode` is `into_vec(..).decode_all(..)` — an unbounded
/// `Vec` — and LZW emits up to a full dictionary entry per code, so a small
/// stream expands far. Capping the sink bounds the allocation itself rather
/// than checking its size afterwards.
struct CappedWriter {
    buf: Vec<u8>,
    limit: usize,
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len().saturating_add(data.len()) > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "output exceeds the stream cap",
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    // The bound goes on the *reader*: a length check after `read_to_end` is
    // checked after the allocation it was meant to prevent. Reading one byte
    // past the cap is what distinguishes "exactly at the limit" from
    // "truncated here".
    let take = MAX_DECODED_STREAM_BYTES as u64 + 1;
    let mut out = Vec::new();
    // Zlib first; a stream missing its two-byte header is common enough in the
    // wild that raw deflate is worth the second attempt.
    if flate2::read::ZlibDecoder::new(data)
        .take(take)
        .read_to_end(&mut out)
        .is_ok()
        && !out.is_empty()
    {
        // A zlib stream over the cap errors rather than falling through to the
        // raw-deflate attempt below: it will not succeed there either.
        return over_cap(out, "FlateDecode");
    }
    out.clear();
    match flate2::read::DeflateDecoder::new(data)
        .take(take)
        .read_to_end(&mut out)
    {
        Ok(_) => over_cap(out, "FlateDecode"),
        Err(e) => Err(format!("FlateDecode failed: {e}")),
    }
}

fn lzw(data: &[u8], parms: Option<&Dictionary>, doc: &Document) -> Result<Vec<u8>, String> {
    let early = parms
        .and_then(|p| p.get(b"EarlyChange").ok())
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_i64().ok())
        .map(|v| v != 0)
        .unwrap_or(true);
    let mut decoder = if early {
        weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
    } else {
        weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8)
    };
    let mut sink = CappedWriter {
        buf: Vec::new(),
        limit: MAX_DECODED_STREAM_BYTES,
    };
    match decoder.into_stream(&mut sink).decode_all(data).status {
        Ok(()) => Ok(sink.buf),
        Err(e) => Err(format!("LZWDecode failed: {e}")),
    }
}

fn ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut n = 0;
    let mut i = 0;
    // A leading `<~` is optional; `~>` ends the data.
    if data.starts_with(b"<~") {
        i = 2;
    }
    while i < data.len() {
        let b = data[i];
        i += 1;
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[n] = b - b'!';
                n += 1;
                if n == 5 {
                    push_group(&mut out, &group, 5);
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for slot in group.iter_mut().skip(n) {
            *slot = 84;
        }
        push_group(&mut out, &group, n);
    }
    out
}

fn push_group(out: &mut Vec<u8>, group: &[u8; 5], n: usize) {
    let value = group
        .iter()
        .fold(0u32, |acc, d| acc.wrapping_mul(85).wrapping_add(*d as u32));
    let bytes = value.to_be_bytes();
    out.extend_from_slice(&bytes[..n - 1]);
}

fn ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut nibbles: Vec<u8> = Vec::with_capacity(data.len());
    for b in data {
        match b {
            b'>' => break,
            b'0'..=b'9' => nibbles.push(b - b'0'),
            b'a'..=b'f' => nibbles.push(b - b'a' + 10),
            b'A'..=b'F' => nibbles.push(b - b'A' + 10),
            _ => {}
        }
    }
    if nibbles.len() % 2 == 1 {
        nibbles.push(0);
    }
    nibbles
        .chunks_exact(2)
        .map(|c| (c[0] << 4) | c[1])
        .collect()
}

fn run_length(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        // Up to 128.5x per pass, and `/Filter` is an *array* applied in a loop,
        // so `[/RL /RL /RL /RL]` is ~2.7e8x on the file bytes.
        if out.len() > MAX_DECODED_STREAM_BYTES {
            break;
        }
        let length = data[i] as usize;
        i += 1;
        match length {
            128 => break,
            0..=127 => {
                let end = (i + length + 1).min(data.len());
                out.extend_from_slice(&data[i..end]);
                i = end;
            }
            _ => {
                if let Some(b) = data.get(i) {
                    out.extend(std::iter::repeat_n(*b, 257 - length));
                }
                i += 1;
            }
        }
    }
    out
}

/// Undo a `/Predictor`. TIFF prediction (2) and the PNG family (10–15) both
/// exist; the PNG one carries its filter type as the first byte of every row.
fn unpredict(doc: &Document, data: &[u8], parms: &Dictionary) -> Result<Vec<u8>, String> {
    let get = |key: &[u8], default: i64| {
        parms
            .get(key)
            .and_then(|o| doc.dereference(o))
            .and_then(|(_, o)| o.as_i64())
            .unwrap_or(default)
    };
    let predictor = get(b"Predictor", 1);
    if predictor < 2 {
        return Ok(data.to_vec());
    }
    let colors = get(b"Colors", 1).clamp(1, 32) as usize;
    let bpc = get(b"BitsPerComponent", 8).clamp(1, 16) as usize;
    // `Colors` and `BitsPerComponent` are clamped above; `Columns` was not, and
    // it feeds `vec![0u8; row_bytes]` twice below. `/Columns 4000000000` asks
    // for ~8 GB and the multiply can overflow first. 2^20 columns is already
    // absurd for a real scan (a 4 MB row buffer) and keeps the product small.
    let columns = (get(b"Columns", 1).max(1) as usize).min(1 << 20);
    let row_bytes = (columns * colors * bpc).div_ceil(8);
    let pixel_bytes = (colors * bpc).div_ceil(8).max(1);

    if predictor == 2 {
        return Ok(tiff_predictor(data, row_bytes, pixel_bytes, bpc));
    }
    png_predictor(data, row_bytes, pixel_bytes)
}

fn tiff_predictor(data: &[u8], row_bytes: usize, pixel_bytes: usize, bpc: usize) -> Vec<u8> {
    // Sub-byte TIFF prediction is vanishingly rare and cannot be undone by byte
    // arithmetic, so it is left as-is rather than corrupted.
    if bpc != 8 {
        return data.to_vec();
    }
    let mut out = data.to_vec();
    for row in out.chunks_mut(row_bytes) {
        for i in pixel_bytes..row.len() {
            row[i] = row[i].wrapping_add(row[i - pixel_bytes]);
        }
    }
    out
}

fn png_predictor(data: &[u8], row_bytes: usize, pixel_bytes: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data.len());
    let mut previous = vec![0u8; row_bytes];
    let mut current = vec![0u8; row_bytes];
    let mut pos = 0;
    while pos < data.len() {
        let tag = data[pos];
        pos += 1;
        let take = row_bytes.min(data.len().saturating_sub(pos));
        current[..take].copy_from_slice(&data[pos..pos + take]);
        current[take..].fill(0);
        pos += take;

        for i in 0..row_bytes {
            let left = if i >= pixel_bytes {
                current[i - pixel_bytes]
            } else {
                0
            };
            let up = previous[i];
            let up_left = if i >= pixel_bytes {
                previous[i - pixel_bytes]
            } else {
                0
            };
            current[i] = match tag {
                0 => current[i],
                1 => current[i].wrapping_add(left),
                2 => current[i].wrapping_add(up),
                3 => current[i].wrapping_add((((left as u16) + (up as u16)) / 2) as u8),
                4 => current[i].wrapping_add(paeth(left, up, up_left)),
                other => return Err(format!("unknown PNG predictor row filter {other}")),
            };
        }
        out.extend_from_slice(&current);
        std::mem::swap(&mut previous, &mut current);
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = (
        (p - a as i16).abs(),
        (p - b as i16).abs(),
        (p - c as i16).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_hex_ignores_whitespace_and_pads_an_odd_digit() {
        assert_eq!(ascii_hex(b"48 65 6C 6C 6F>"), b"Hello");
        assert_eq!(ascii_hex(b"4>"), vec![0x40]);
    }

    #[test]
    fn ascii85_round_trips_a_known_group() {
        // "Man " encodes to "9jqo" + one more character in Adobe's own example.
        assert_eq!(ascii85(b"87cURD]~>"), b"Hello");
        assert_eq!(ascii85(b"z~>"), vec![0, 0, 0, 0]);
    }

    #[test]
    fn run_length_expands_runs_and_stops_at_the_marker() {
        // 2 -> copy 3 literals; 254 -> repeat the next byte 3 times; 128 -> end.
        assert_eq!(
            run_length(&[2, b'a', b'b', b'c', 254, b'z', 128, b'x']),
            b"abczzz"
        );
    }

    #[test]
    fn the_up_predictor_adds_the_row_above() {
        // Two 3-byte rows, both filtered with "Up"; the second is all zeros so
        // it must come back identical to the first.
        let data = [2, 10, 20, 30, 2, 0, 0, 0];
        assert_eq!(
            png_predictor(&data, 3, 1).unwrap(),
            vec![10, 20, 30, 10, 20, 30]
        );
    }

    #[test]
    fn tiff_prediction_accumulates_along_the_row() {
        assert_eq!(tiff_predictor(&[1, 1, 1, 1], 4, 1, 8), vec![1, 2, 3, 4]);
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    fn bomb(mb: usize) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..mb {
            enc.write_all(&zeros).expect("compress");
        }
        enc.finish().expect("finish")
    }

    /// The cap must actually fire — a test that only asserts "the process
    /// survived" would pass even if `inflate` were never reached.
    #[test]
    fn inflate_refuses_a_stream_past_the_cap() {
        let over = bomb((MAX_DECODED_STREAM_BYTES >> 20) + 8);
        let err = inflate(&over).expect_err("a stream past the cap must be refused");
        assert!(
            err.contains("exceeds"),
            "the error must name the cause, got {err:?}"
        );
    }

    /// And it must not fire early. The real corpus maximum is ~55.5 MB, so a
    /// stream of that size has to keep working — this is the half that stops
    /// the cap being tightened into a regression.
    #[test]
    fn inflate_still_accepts_the_largest_real_stream_size() {
        let ok = bomb(56);
        let out = inflate(&ok).expect("56 MB is under the cap and must decode");
        assert_eq!(out.len(), 56 << 20);
    }
}
