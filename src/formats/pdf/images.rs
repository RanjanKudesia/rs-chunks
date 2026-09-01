//! Image XObjects → files.
//!
//! A JPEG or JPEG 2000 stream already *is* a file and is passed through
//! untouched, so no pixel is re-encoded and nothing is lost. Everything else
//! arrives as raw samples that have to be read through the image's colour space
//! and written out as PNG, with any `/SMask` folded in as the alpha channel.
//!
//! What is not decoded is named rather than silently dropped: CCITT Group 3/4
//! and JBIG2 are fax codecs needing decoders of their own, and the caller
//! reports them as a skipped image.

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use super::filters::{self, Codec};

/// Refuse to allocate for an implausible picture: 100 megapixels is far beyond
/// anything a document embeds, and the dimensions come from the file.
const MAX_PIXELS: u64 = 100_000_000;

pub(crate) struct Image {
    // Image naming derives the extension elsewhere; kept alongside the bytes
    // so decoders stay self-describing.
    #[allow(dead_code)]
    pub extension: &'static str,
    pub bytes: Vec<u8>,
}

/// Why this image will not be decoded, from its filter chain alone — cheap
/// enough to ask before deciding whether the markdown may reference it.
pub(crate) fn undecodable(doc: &Document, id: ObjectId) -> Option<String> {
    let stream = doc.get_object(id).and_then(Object::as_stream).ok()?;
    for name in filter_names(doc, stream) {
        if !matches!(
            name.as_str(),
            "FlateDecode"
                | "Fl"
                | "LZWDecode"
                | "LZW"
                | "ASCII85Decode"
                | "A85"
                | "ASCIIHexDecode"
                | "AHx"
                | "RunLengthDecode"
                | "RL"
                | "DCTDecode"
                | "DCT"
                | "JPXDecode"
        ) {
            return Some(format!("{name} images are not decoded"));
        }
    }
    None
}

/// The file extension the image will be written with.
pub(crate) fn extension_of(doc: &Document, id: ObjectId) -> &'static str {
    let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
        return "png";
    };
    match filter_names(doc, stream).last().map(String::as_str) {
        Some("DCTDecode") | Some("DCT") => "jpg",
        Some("JPXDecode") => "jp2",
        _ => "png",
    }
}

fn filter_names(doc: &Document, stream: &Stream) -> Vec<String> {
    match stream
        .dict
        .get(b"Filter")
        .or_else(|_| stream.dict.get(b"F"))
    {
        Ok(Object::Name(n)) => vec![String::from_utf8_lossy(n).to_string()],
        Ok(Object::Array(a)) => a
            .iter()
            .filter_map(|o| doc.dereference(o).ok())
            .filter_map(|(_, o)| o.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Decode one image XObject, or explain why it could not be.
pub(crate) fn extract(doc: &Document, id: ObjectId) -> Result<Image, String> {
    let stream = doc
        .get_object(id)
        .and_then(Object::as_stream)
        .map_err(|e| format!("image object unreadable: {e}"))?;
    let (data, codec) = filters::decode(doc, stream)?;
    match codec {
        Codec::Jpeg => Ok(Image {
            extension: "jpg",
            bytes: data,
        }),
        Codec::Jpeg2000 => Ok(Image {
            extension: "jp2",
            bytes: data,
        }),
        Codec::Unsupported(name) => Err(format!("{name} images are not decoded")),
        Codec::Samples => to_png(doc, stream, &data).map(|bytes| Image {
            extension: "png",
            bytes,
        }),
    }
}

fn to_png(doc: &Document, stream: &Stream, samples: &[u8]) -> Result<Vec<u8>, String> {
    let width = integer(doc, &stream.dict, b"Width").unwrap_or(0).max(0) as u32;
    let height = integer(doc, &stream.dict, b"Height").unwrap_or(0).max(0) as u32;
    if width == 0 || height == 0 {
        return Err("image has no extent".into());
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!("image is implausibly large ({width}x{height})"));
    }

    let stencil = stream
        .dict
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false);
    let bpc = if stencil {
        1
    } else {
        integer(doc, &stream.dict, b"BitsPerComponent")
            .unwrap_or(8)
            .clamp(1, 16) as usize
    };
    let space = ColorSpace::of(doc, stream, stencil)?;
    let mut rgb = to_rgb(
        samples,
        width,
        height,
        bpc,
        &space,
        decode_inverted(doc, stream),
    )?;

    match alpha(doc, stream, width, height) {
        Some(alpha) => {
            let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
            for (pixel, a) in rgb.chunks_exact(3).zip(alpha) {
                rgba.extend_from_slice(pixel);
                rgba.push(a);
            }
            encode(&rgba, width, height, ExtendedColorType::Rgba8)
        }
        None => {
            rgb.truncate(width as usize * height as usize * 3);
            encode(&rgb, width, height, ExtendedColorType::Rgb8)
        }
    }
}

fn encode(
    data: &[u8],
    width: u32,
    height: u32,
    kind: ExtendedColorType,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(data, width, height, kind)
        .map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok(out)
}

/// The colour spaces that appear in real documents, reduced to what turns a
/// sample into a pixel.
enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    /// A palette: component count of the base space, and its RGB entries.
    Indexed(Vec<[u8; 3]>),
}

impl ColorSpace {
    fn of(doc: &Document, stream: &Stream, stencil: bool) -> Result<ColorSpace, String> {
        if stencil {
            return Ok(ColorSpace::Gray);
        }
        let object = stream
            .dict
            .get(b"ColorSpace")
            .or_else(|_| stream.dict.get(b"CS"))
            .and_then(|o| doc.dereference(o))
            .map(|(_, o)| o.clone())
            .map_err(|_| "image declares no colour space".to_string())?;
        Self::from_object(doc, &object)
    }

    fn from_object(doc: &Document, object: &Object) -> Result<ColorSpace, String> {
        match object {
            Object::Name(name) => match name.as_slice() {
                b"DeviceGray" | b"G" | b"CalGray" => Ok(ColorSpace::Gray),
                b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => Ok(ColorSpace::Rgb),
                b"DeviceCMYK" | b"CMYK" => Ok(ColorSpace::Cmyk),
                other => Err(format!(
                    "unsupported colour space /{}",
                    String::from_utf8_lossy(other)
                )),
            },
            Object::Array(items) if !items.is_empty() => {
                let family = items[0].as_name().unwrap_or(b"");
                match family {
                    b"ICCBased" => Self::icc(doc, items),
                    b"Indexed" | b"I" => Self::indexed(doc, items),
                    // Separation and DeviceN paint one ink; treating it as
                    // coverage keeps the picture legible without a tint
                    // transform interpreter.
                    b"Separation" | b"DeviceN" => Ok(ColorSpace::Gray),
                    b"CalRGB" | b"Lab" => Ok(ColorSpace::Rgb),
                    b"CalGray" => Ok(ColorSpace::Gray),
                    b"DeviceGray" => Ok(ColorSpace::Gray),
                    b"DeviceRGB" => Ok(ColorSpace::Rgb),
                    b"DeviceCMYK" => Ok(ColorSpace::Cmyk),
                    other => Err(format!(
                        "unsupported colour space /{}",
                        String::from_utf8_lossy(other)
                    )),
                }
            }
            _ => Err("unreadable colour space".into()),
        }
    }

    /// An ICC profile's `/N` says how many components it has, which is all that
    /// is needed to lay the samples out.
    fn icc(doc: &Document, items: &[Object]) -> Result<ColorSpace, String> {
        let n = items
            .get(1)
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_stream().ok())
            .and_then(|s| s.dict.get(b"N").and_then(Object::as_i64).ok())
            .unwrap_or(3);
        match n {
            1 => Ok(ColorSpace::Gray),
            4 => Ok(ColorSpace::Cmyk),
            _ => Ok(ColorSpace::Rgb),
        }
    }

    /// `[/Indexed base hival lookup]` — resolve the palette to RGB once, so the
    /// per-pixel path is a table lookup.
    fn indexed(doc: &Document, items: &[Object]) -> Result<ColorSpace, String> {
        let base_object = items.get(1).ok_or("indexed colour space has no base")?;
        let base = doc
            .dereference(base_object)
            .map_err(|e| e.to_string())?
            .1
            .clone();
        let base = Self::from_object(doc, &base)?;
        let components = base.components();
        let lookup = match items.get(3).and_then(|o| doc.dereference(o).ok()) {
            Some((_, Object::String(bytes, _))) => bytes.clone(),
            Some((_, Object::Stream(s))) => filters::decode(doc, s)?.0,
            _ => return Err("indexed colour space has no palette".into()),
        };
        let entries = lookup.len() / components.max(1);
        let mut palette = Vec::with_capacity(entries);
        for i in 0..entries {
            let slice = &lookup[i * components..(i + 1) * components];
            palette.push(base.to_rgb(slice));
        }
        if palette.is_empty() {
            return Err("indexed colour space has an empty palette".into());
        }
        Ok(ColorSpace::Indexed(palette))
    }

    fn components(&self) -> usize {
        match self {
            ColorSpace::Gray => 1,
            ColorSpace::Rgb => 3,
            ColorSpace::Cmyk => 4,
            ColorSpace::Indexed(_) => 1,
        }
    }

    fn to_rgb(&self, values: &[u8]) -> [u8; 3] {
        match self {
            ColorSpace::Gray => [values[0], values[0], values[0]],
            ColorSpace::Rgb => [values[0], values[1], values[2]],
            ColorSpace::Cmyk => {
                let f = |i: usize, k: u8| {
                    (255u16.saturating_sub(values[i] as u16) * 255u16.saturating_sub(k as u16)
                        / 255) as u8
                };
                let k = values[3];
                [f(0, k), f(1, k), f(2, k)]
            }
            ColorSpace::Indexed(palette) => *palette.get(values[0] as usize).unwrap_or(&[0, 0, 0]),
        }
    }
}

fn to_rgb(
    samples: &[u8],
    width: u32,
    height: u32,
    bpc: usize,
    space: &ColorSpace,
    inverted: bool,
) -> Result<Vec<u8>, String> {
    // The 100 MP guard used to live only in `to_png`, checked against the
    // *parent* image. `alpha()` re-enters here with the /SMask's own Width and
    // Height, so a 1x1 image carrying a 100000x100000 mask reserved ~30 GB —
    // an OOM abort, which `catch_unwind` cannot intercept. Guarding here covers
    // every caller instead of every call site.
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err("image is implausibly large".to_string());
    }
    let components = space.components();
    let pixels = width as usize * height as usize;
    let row_bits = width as usize * components * bpc;
    let row_bytes = row_bits.div_ceil(8);
    if samples.len() < row_bytes * height as usize {
        // Truncated image data is common in damaged files; pad rather than
        // refuse, so a partial picture still comes through.
        if samples.is_empty() {
            return Err("image has no sample data".into());
        }
    }

    let mut out = Vec::with_capacity(pixels * 3);
    let mut values = vec![0u8; components];
    for y in 0..height as usize {
        let row = y * row_bytes;
        for x in 0..width as usize {
            for (c, slot) in values.iter_mut().enumerate() {
                let index = (x * components + c) * bpc;
                *slot = sample(
                    samples,
                    row,
                    index,
                    bpc,
                    matches!(space, ColorSpace::Indexed(_)),
                );
            }
            let mut rgb = space.to_rgb(&values);
            if inverted {
                rgb = [255 - rgb[0], 255 - rgb[1], 255 - rgb[2]];
            }
            out.extend_from_slice(&rgb);
        }
    }
    Ok(out)
}

/// Read one component, scaling sub-byte and 16-bit depths to 8 bits. An index
/// into a palette is *not* scaled — it is an ordinal, not an intensity.
fn sample(data: &[u8], row: usize, bit_index: usize, bpc: usize, raw_index: bool) -> u8 {
    match bpc {
        8 => *data.get(row + bit_index / 8).unwrap_or(&0),
        16 => *data.get(row + bit_index / 8).unwrap_or(&0),
        _ => {
            let byte = *data.get(row + bit_index / 8).unwrap_or(&0);
            let shift = 8 - bpc - (bit_index % 8);
            let mask = (1u16 << bpc) - 1;
            let value = ((byte as u16) >> shift) & mask;
            if raw_index {
                value as u8
            } else {
                // Spread the range so 1-bit black/white becomes 0/255.
                (value * 255 / mask) as u8
            }
        }
    }
}

/// A `/Decode` array that runs backwards means the samples are inverted. Only
/// the common `[1 0]` stencil form is honoured; partial ranges are rare and
/// guessing at them would distort colours.
fn decode_inverted(doc: &Document, stream: &Stream) -> bool {
    let Ok(array) = stream
        .dict
        .get(b"Decode")
        .or_else(|_| stream.dict.get(b"D"))
        .and_then(Object::as_array)
    else {
        return false;
    };
    let first = array
        .first()
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_float().ok());
    let second = array
        .get(1)
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_float().ok());
    matches!((first, second), (Some(a), Some(b)) if a > b)
}

/// The `/SMask`'s samples as an alpha channel, resampled to the image's size.
fn alpha(doc: &Document, stream: &Stream, width: u32, height: u32) -> Option<Vec<u8>> {
    let (_, object) = doc.dereference(stream.dict.get(b"SMask").ok()?).ok()?;
    let mask = object.as_stream().ok()?;
    let (data, codec) = filters::decode(doc, mask).ok()?;
    if codec != Codec::Samples {
        return None;
    }
    let mw = integer(doc, &mask.dict, b"Width")? as u32;
    let mh = integer(doc, &mask.dict, b"Height")? as u32;
    let bpc = integer(doc, &mask.dict, b"BitsPerComponent")
        .unwrap_or(8)
        .clamp(1, 16) as usize;
    if mw == 0 || mh == 0 {
        return None;
    }
    let gray = to_rgb(
        &data,
        mw,
        mh,
        bpc,
        &ColorSpace::Gray,
        decode_inverted(doc, mask),
    )
    .ok()?;

    // Nearest-neighbour is enough: a soft mask is a coverage map, and its
    // resolution usually matches the image it belongs to already.
    let mut out = Vec::with_capacity(width as usize * height as usize);
    for y in 0..height {
        let sy = (y as u64 * mh as u64 / height as u64).min(mh as u64 - 1) as usize;
        for x in 0..width {
            let sx = (x as u64 * mw as u64 / width as u64).min(mw as u64 - 1) as usize;
            out.push(*gray.get((sy * mw as usize + sx) * 3).unwrap_or(&255));
        }
    }
    Some(out)
}

fn integer(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<i64> {
    dict.get(key)
        .and_then(|o| doc.dereference(o))
        .map(|(_, o)| o)
        .ok()?
        .as_i64()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmyk_black_and_white_convert_the_way_ink_does() {
        let space = ColorSpace::Cmyk;
        assert_eq!(space.to_rgb(&[0, 0, 0, 0]), [255, 255, 255]);
        assert_eq!(space.to_rgb(&[0, 0, 0, 255]), [0, 0, 0]);
        assert_eq!(space.to_rgb(&[255, 0, 0, 0]), [0, 255, 255]);
    }

    #[test]
    fn a_one_bit_image_becomes_black_and_white_not_black_and_one() {
        // Two pixels: 1 then 0, packed into the top bits of one byte.
        let rgb = to_rgb(&[0b1000_0000], 2, 1, 1, &ColorSpace::Gray, false).unwrap();
        assert_eq!(rgb, vec![255, 255, 255, 0, 0, 0]);
    }

    #[test]
    fn a_palette_index_is_an_ordinal_and_is_not_scaled() {
        let space = ColorSpace::Indexed(vec![[10, 20, 30], [40, 50, 60]]);
        // 1-bit indices 0 then 1 must select entry 0 then entry 1.
        let rgb = to_rgb(&[0b0100_0000], 2, 1, 1, &space, false).unwrap();
        assert_eq!(rgb, vec![10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn an_inverted_decode_array_flips_the_samples() {
        let rgb = to_rgb(&[0b1000_0000], 2, 1, 1, &ColorSpace::Gray, true).unwrap();
        assert_eq!(rgb, vec![0, 0, 0, 255, 255, 255]);
    }
}
