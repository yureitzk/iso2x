//! Looks up a named section in an XBE's section table. Generic over
//! which section is wanted - a sibling of [`super::xbe`], which only
//! reads the header far enough to get execution info. Offsets per
//! <https://xboxdevwiki.net/Xbe>.

use anyhow::{Result, bail};
use binrw::BinRead;
use byteorder::{LE, ReadBytesExt};
use std::io::{Cursor, Read, Seek, SeekFrom};

const XBE_SECTION_HEADER_SIZE: u64 = 56;

/// The three fields this crate needs out of a 56-byte XBE section
/// header record: raw (file) address, raw size, and the (virtual) name
/// address, at a fixed `+0x0C` sub-offset within the record.
#[derive(BinRead, Debug, Clone, Copy)]
#[br(little)]
struct XbeSectionHeaderFields {
    raw_address: u32,
    raw_size: u32,
    name_addr: u32,
}

/// Returns the raw bytes of the named XBE section, if present.
/// `xbe_bytes` must be the *complete* XBE file, not just its header - a
/// section can sit well past where a header-only read would stop.
pub(crate) fn find_xbe_section<'a>(xbe_bytes: &'a [u8], target: &str) -> Result<Option<&'a [u8]>> {
    let mut r = Cursor::new(xbe_bytes);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"XBEH" {
        bail!("missing 'XBEH' magic bytes in XBE header");
    }

    // 0x104: base address.
    r.seek(SeekFrom::Start(0x104))?;
    let base_addr = r.read_u32::<LE>()?;

    // 0x11C: number of sections. 0x120: section headers (virtual) address.
    r.seek(SeekFrom::Start(0x11C))?;
    let num_sections = r.read_u32::<LE>()?;
    let section_headers_va = r.read_u32::<LE>()?;
    let section_headers_offset = u64::from(section_headers_va.saturating_sub(base_addr));

    // A corrupt/hostile file could claim an absurd section count; real
    // XBEs have a few dozen at most.
    let num_sections = num_sections.min(4096);

    for i in 0..num_sections {
        let header_offset = section_headers_offset + u64::from(i) * XBE_SECTION_HEADER_SIZE;
        // +0x0C raw (file) address, +0x10 raw size, +0x14 name address
        // (virtual - needs the same base-address translation as above).
        if r.seek(SeekFrom::Start(header_offset + 0x0C)).is_err() {
            break;
        }
        let Ok(fields) = XbeSectionHeaderFields::read(&mut r) else {
            break;
        };

        let name_offset = u64::from(fields.name_addr.saturating_sub(base_addr));
        if r.seek(SeekFrom::Start(name_offset)).is_err() {
            continue;
        }
        let Ok(name) = read_cstr(&mut r, 256) else {
            continue;
        };

        if name == target {
            let start = fields.raw_address as usize;
            let end = start.saturating_add(fields.raw_size as usize);
            return Ok(xbe_bytes.get(start..end));
        }
    }

    Ok(None)
}

fn read_cstr<R: Read>(r: &mut R, max_len: usize) -> Result<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    for _ in 0..max_len {
        r.read_exact(&mut byte)?;
        if byte[0] == 0 {
            break;
        }
        buf.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
