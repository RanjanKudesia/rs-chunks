use std::io::{Cursor, Read};

fn open_cfb(bytes: &[u8]) -> Result<cfb::CompoundFile<Cursor<&[u8]>>, String> {
    // Borrowed cursor: the CFB is read-only here, so no copy of the file bytes.
    cfb::CompoundFile::open(Cursor::new(bytes))
        .map_err(|e| format!("Cannot open .ppt file (invalid CFB format): {e}"))
}

/// Reads the "PowerPoint Document" stream from the CFB (OLE) container.
pub fn read_powerpoint_document_stream(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut compound = open_cfb(bytes)?;
    let mut buf = Vec::new();
    compound
        .open_stream("/PowerPoint Document")
        .map_err(|_| "Missing 'PowerPoint Document' stream — not a valid .ppt file".to_string())?
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read PowerPoint Document stream: {e}"))?;
    Ok(buf)
}

/// Reads the optional "Pictures" stream (embedded images). `None` when absent.
pub fn read_pictures_stream(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut compound = open_cfb(bytes)?;
    let mut stream = match compound.open_stream("/Pictures") {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read Pictures stream: {e}"))?;
    Ok(Some(buf))
}
