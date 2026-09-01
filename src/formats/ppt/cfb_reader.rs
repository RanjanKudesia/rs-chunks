//! CFB (OLE compound file) access for `.ppt`.
//!
//! [`PptCfb`] opens the container **once** over a borrowed byte slice and hands
//! out the streams a `.ppt` parse needs. The image path reads two streams from
//! the same file ("PowerPoint Document" then "Pictures"), and each read used to
//! re-open the container — parsing the whole directory tree twice for one
//! document (TECH_DEBT X9). Mirrors `formats::doc::cfb_reader::DocCfb`, which
//! solved the same problem for `.doc` in R8.

use std::io::{Cursor, Read};

/// A `.ppt` compound file, opened once over the caller's bytes (no copy).
pub struct PptCfb<'a> {
    compound: cfb::CompoundFile<Cursor<&'a [u8]>>,
}

impl<'a> PptCfb<'a> {
    /// Open the CFB container over `bytes` (borrowed — no copy is made).
    pub fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let compound = cfb::CompoundFile::open(Cursor::new(bytes))
            .map_err(|e| format!("Cannot open .ppt file (invalid CFB format): {e}"))?;
        Ok(PptCfb { compound })
    }

    /// Reads the "PowerPoint Document" stream — the record tree every parse needs.
    ///
    /// PowerPoint 4.0 / 95 keep their content in a `PP40` storage rather than
    /// this stream, so such a file landed on "not a valid .ppt" — technically
    /// true and useless: it is a perfectly valid PowerPoint file, just an
    /// older format, and the caller is told nothing they can act on. Named
    /// now, mirroring what `.doc` already does for pre-Word-97 files.
    ///
    /// Some PowerPoint 95 saves also carry a `PP97_DUALSTORAGE` sub-storage
    /// holding a 97-format copy, which this reader CAN read — so that is
    /// preferred before giving up. Same discriminator Apache POI uses.
    pub fn powerpoint_document_stream(&mut self) -> Result<Vec<u8>, String> {
        let path = if self.compound.is_stream("/PowerPoint Document") {
            "/PowerPoint Document"
        } else if self
            .compound
            .is_stream("/PP97_DUALSTORAGE/PowerPoint Document")
        {
            "/PP97_DUALSTORAGE/PowerPoint Document"
        } else if self.compound.exists("/PP40") {
            return Err("This is a PowerPoint 95 (or earlier) file, which is not \
                        supported. Convert it to .pptx first."
                .to_string());
        } else {
            return Err("Missing 'PowerPoint Document' stream — not a valid .ppt file".to_string());
        };

        let mut buf = Vec::new();
        self.compound
            .open_stream(path)
            .map_err(|_| {
                "Missing 'PowerPoint Document' stream — not a valid .ppt file".to_string()
            })?
            .read_to_end(&mut buf)
            .map_err(|e| format!("Failed to read PowerPoint Document stream: {e}"))?;
        Ok(buf)
    }

    /// Reads the optional "Pictures" stream (embedded images). `None` when absent.
    pub fn pictures_stream(&mut self) -> Result<Option<Vec<u8>>, String> {
        let mut stream = match self.compound.open_stream("/Pictures") {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("Failed to read Pictures stream: {e}"))?;
        Ok(Some(buf))
    }
}

impl<'a> PptCfb<'a> {
    /// Reads the document stream AND the "Current User" stream from the SAME
    /// storage. The pairing is load-bearing: `offsetToCurrentEdit` inside
    /// `Current User` indexes the document stream it was saved beside, so a
    /// root-level `Current User` combined with a `PP97_DUALSTORAGE` document
    /// stream would index the wrong bytes. `Current User` being absent is not
    /// an error — the caller falls back to a whole-stream scan.
    pub fn document_and_current_user(&mut self) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
        let dual = !self.compound.is_stream("/PowerPoint Document")
            && self
                .compound
                .is_stream("/PP97_DUALSTORAGE/PowerPoint Document");
        let doc = self.powerpoint_document_stream()?;
        let cu_path = if dual {
            "/PP97_DUALSTORAGE/Current User"
        } else {
            "/Current User"
        };
        let cu = match self.compound.open_stream(cu_path) {
            Ok(mut s) => {
                let mut buf = Vec::new();
                match s.read_to_end(&mut buf) {
                    Ok(_) => Some(buf),
                    Err(_) => None,
                }
            }
            Err(_) => None,
        };
        Ok((doc, cu))
    }
}

/// Reads the "PowerPoint Document" stream from the CFB (OLE) container.
///
/// Convenience wrapper for the text-only path, which needs exactly one stream.
/// When you need more than one stream from the same file, open a [`PptCfb`] and
/// reuse it rather than calling several of these.
pub fn read_powerpoint_document_stream(bytes: &[u8]) -> Result<Vec<u8>, String> {
    PptCfb::open(bytes)?.powerpoint_document_stream()
}

/// Reads the optional "Pictures" stream (embedded images). `None` when absent.
///
/// Same caveat as above: prefer [`PptCfb`] when reading multiple streams.
pub fn read_pictures_stream(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    PptCfb::open(bytes)?.pictures_stream()
}
