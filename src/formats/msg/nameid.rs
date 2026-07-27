//! Named-property resolution (`__nameid_version1.0`).
//!
//! Item-type fields (appointment start/end/location, task due-date/status/…)
//! are stored under named properties whose runtime ids are `0x8000+`. This maps
//! a well-known `(property set GUID, LID)` to that runtime propid so the fields
//! become addressable. See MS-OXMSG §2.2.3 and MS-OXPROPS.

use std::io::Read;

type Cfb = cfb::CompoundFile<std::io::Cursor<Vec<u8>>>;

/// {00062002-0000-0000-C000-000000000046} — calendar/appointment properties.
pub const PSETID_APPOINTMENT: [u8; 16] = [
    0x02, 0x20, 0x06, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46,
];
/// {00062003-0000-0000-C000-000000000046} — task properties.
pub const PSETID_TASK: [u8; 16] = [
    0x03, 0x20, 0x06, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46,
];

struct Entry {
    name_id: u32,
    guid_index: u16,
    kind: u8, // 0 = numeric LID, 1 = string
    propid: u16,
}

#[derive(Default)]
pub struct NameIdMap {
    guids: Vec<[u8; 16]>,
    entries: Vec<Entry>,
}

fn read_stream(cfb: &mut Cfb, path: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(mut s) = cfb.open_stream(path) {
        let _ = s.read_to_end(&mut buf);
    }
    buf
}

pub fn parse(cfb: &mut Cfb) -> NameIdMap {
    let guid_bytes = read_stream(cfb, "/__nameid_version1.0/__substg1.0_00020102");
    let entry_bytes = read_stream(cfb, "/__nameid_version1.0/__substg1.0_00030102");

    let mut guids = Vec::new();
    for chunk in guid_bytes.chunks_exact(16) {
        let mut g = [0u8; 16];
        g.copy_from_slice(chunk);
        guids.push(g);
    }

    let mut entries = Vec::new();
    for e in entry_bytes.chunks_exact(8) {
        let name_id = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
        let index_kind = u32::from_le_bytes([e[4], e[5], e[6], e[7]]);
        let kind = (index_kind & 0x1) as u8;
        let guid_index = ((index_kind >> 1) & 0x7FFF) as u16;
        let prop_index = (index_kind >> 16) as u16;
        entries.push(Entry {
            name_id,
            guid_index,
            kind,
            propid: 0x8000u16.wrapping_add(prop_index),
        });
    }

    NameIdMap { guids, entries }
}

impl NameIdMap {
    /// Resolve a numeric named property `(set_guid, lid)` to its runtime propid.
    pub fn lid(&self, set_guid: &[u8; 16], lid: u32) -> Option<u16> {
        // GUID index: 1=PS_MAPI, 2=PS_PUBLIC_STRINGS, >=3 → GUID-stream position.
        let pos = self.guids.iter().position(|g| g == set_guid)?;
        let guid_index = (pos + 3) as u16;
        self.entries
            .iter()
            .find(|e| e.kind == 0 && e.name_id == lid && e.guid_index == guid_index)
            .map(|e| e.propid)
    }
}
