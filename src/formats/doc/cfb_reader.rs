//! CFB (OLE compound file) access for `.doc`.
//!
//! [`DocCfb`] opens the container **once** over a borrowed byte slice and hands
//! out the streams a `.doc` parse needs. (Historically each stream read
//! re-opened the CFB — up to 3 opens per document, each over a full copy of the
//! file bytes; both the copies and the re-opens are gone.)

use std::io::{Cursor, Read};

/// A `.doc` compound file, opened once over the caller's bytes (no copy).
pub struct DocCfb<'a> {
    compound: cfb::CompoundFile<Cursor<&'a [u8]>>,
}

impl<'a> DocCfb<'a> {
    /// Open the CFB container over `bytes` (borrowed — no copy is made).
    pub fn open(bytes: &'a [u8]) -> Result<Self, String> {
        let compound = cfb::CompoundFile::open(Cursor::new(bytes))
            .map_err(|e| format!("Cannot open .doc file (invalid CFB format): {e}"))?;
        Ok(DocCfb { compound })
    }

    /// Reads the "WordDocument" stream. Returns raw bytes; caller parses the
    /// FIB from these bytes.
    pub fn word_document_stream(&mut self) -> Result<Vec<u8>, String> {
        let mut buf = Vec::new();
        self.compound
            .open_stream("/WordDocument")
            .map_err(|_| "Missing WordDocument stream — not a valid .doc file".to_string())?
            .read_to_end(&mut buf)
            .map_err(|e| format!("Failed to read WordDocument stream: {e}"))?;
        Ok(buf)
    }

    /// Reads the table stream ("0Table" or "1Table") chosen by `which` (0 or 1).
    pub fn table_stream(&mut self, which: u8) -> Result<Vec<u8>, String> {
        let stream_name = if which == 1 { "/1Table" } else { "/0Table" };
        let mut buf = Vec::new();
        self.compound
            .open_stream(stream_name)
            .map_err(|_| format!("Missing {stream_name} stream — not a valid .doc file"))?
            .read_to_end(&mut buf)
            .map_err(|e| format!("Failed to read {stream_name}: {e}"))?;
        Ok(buf)
    }

    /// Reads the optional "Data" stream, which stores inline picture data (PICF
    /// structures referenced by sprmCPicLocation). Returns `None` when the
    /// stream is absent — a document without inline pictures is not an error.
    pub fn data_stream(&mut self) -> Result<Option<Vec<u8>>, String> {
        let mut stream = match self.compound.open_stream("/Data") {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .map_err(|e| format!("Failed to read Data stream: {e}"))?;
        Ok(Some(buf))
    }
}
