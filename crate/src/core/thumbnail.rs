//! Extracts a launch icon/thumbnail from a title's launch executable.
//!
//! - **XBE**: the `$$XTIMAGE` section (falls back to `$$XSIMAGE`, the
//!   savegame icon, if absent) holds a raw XPR0 texture container,
//!   located via [`crate::core::executable::xbe_sections`] and decoded
//!   via [`crate::core::texture::xpr`].
//!
//! - **XEX**: [`thumbnail_from_xex`] reads the resource table for the
//!   entry named after the title ID, decompresses the image body only
//!   as far as that entry (falling back to decompressing everything and
//!   trying every entry if that doesn't pan out - see
//!   [`try_thumbnail_from_xex`]), then reads it as an XDBF blob
//!   ([`crate::core::xdbf`]) holding the `Thumb` resource, already a
//!   complete PNG.
//!
//!   Of the three compression modes, **none** and **basic** decode
//!   directly; **normal** (LZX) goes through the `lzxd` crate. Any
//!   decode failure falls through to `Ok(None)`.
//!
//!   Retail XEX images are AES-128-CBC encrypted under a static key
//!   ([`crate::core::executable::xex_crypto::RETAIL_KEY`]). If that key
//!   doesn't produce a valid PE (`MZ`) image, the pipeline retries once
//!   with the all-zero devkit key before giving up.

use crate::core::executable::xbe_sections;
use crate::core::executable::xex_crypto::{self, DEVKIT_KEY, RETAIL_KEY};
use crate::core::executable::xex_image::{self, CompressionKind};
use crate::core::texture::xpr::decode_xpr_to_png;
use crate::core::xdbf::{XdbfSection, find_xdbf_resource};
use anyhow::Result;
use std::io::Cursor;

/// Locates `$$XTIMAGE` (the title/game icon) in an XBE's section table,
/// falling back to `$$XSIMAGE` (the savegame icon) only if absent - a
/// title shipping both must prefer the title icon over the (often
/// blank/generic) savegame one. `xbe_bytes` must be the *complete* XBE
/// file, not just its header, since the icon section can sit well past
/// where a header-only read would stop.
///
/// Returns `Ok(None)`, not an error, when no thumbnail can be found or
/// decoded. An `Err` here means `xbe_bytes` isn't a valid XBE at all.
pub(crate) fn thumbnail_from_xbe(xbe_bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    for name in ["$$XTIMAGE", "$$XSIMAGE"] {
        if let Some(section) = xbe_sections::find_xbe_section(xbe_bytes, name)?
            && let Some(png) = decode_xpr_to_png(section)
        {
            return Ok(Some(png));
        }
    }
    Ok(None)
}

/// Locates and decodes the launch title's icon out of a full
/// `default.xex`. See the module doc comment for exactly what's
/// supported (compression modes, encryption).
///
/// Returns `Ok(None)`, not an error, when no thumbnail can be found or
/// decoded, so a hiccup here never fails the rest of a source inspection.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn thumbnail_from_xex(xex_bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    Ok(try_thumbnail_from_xex(xex_bytes).unwrap_or(None))
}

fn try_thumbnail_from_xex(xex_bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    let layout = xex_image::parse_xex_layout(Cursor::new(xex_bytes))?;
    let (Some(resource_info_offset), Some(load_address)) =
        (layout.resource_info_offset, layout.load_address)
    else {
        // No resource table, or no way to translate its VAs.
        return Ok(None);
    };

    let Ok(raw_body) = xex_image::read_raw_body(xex_bytes, layout.code_offset) else {
        return Ok(None);
    };

    // See the module doc: unencrypted images need only one pass.
    let candidate_keys: &[&[u8; 16]] = if layout.encrypted {
        &[&RETAIL_KEY, &DEVKIT_KEY]
    } else {
        &[&RETAIL_KEY]
    };

    // Doesn't depend on the decoded body, so it can be read before any
    // decompression - both to locate the title-ID entry up front (below)
    // and for the fallback "try every entry" pass.
    let resources =
        xex_image::read_resource_table(xex_bytes, layout.header_offset, resource_info_offset)?;

    // Almost every title has an XDBF entry named for its own title ID
    // (see module doc) - decompress only up through that entry first,
    // falling back to the whole image if it doesn't pan out.
    let preferred_target_len = layout.title_id.and_then(|title_id| {
        let expected_name = format!("{title_id:08X}");
        resources.iter().find_map(|(name, address, size)| {
            if name.as_slice() != expected_name.as_bytes() {
                return None;
            }
            let offset = address.checked_sub(load_address)?;
            // `.max(2)` keeps the "MZ" magic check below meaningful even
            // for a (degenerate) zero-size entry right at the image start.
            Some((offset as usize).saturating_add(*size as usize).max(2))
        })
    });

    if let Some(target_len) = preferred_target_len
        && let Some(image) = decode_xex_image(&raw_body, &layout, candidate_keys, Some(target_len))?
        && let Some(png) =
            find_thumbnail_in_resources(&image, load_address, &resources, layout.title_id)
    {
        return Ok(Some(png));
    }

    // No title-ID match above (or its entry didn't decode to a valid
    // thumbnail): decompress the whole image and try every entry.
    let Some(image) = decode_xex_image(&raw_body, &layout, candidate_keys, None)? else {
        return Ok(None);
    };

    Ok(find_thumbnail_in_resources(
        &image,
        load_address,
        &resources,
        layout.title_id,
    ))
}

/// Decrypts (if needed) and decompresses a XEX body, trying each
/// candidate key in turn, up to `target_len` bytes of decompressed
/// output if given (see the caller). Returns `Ok(None)` rather than an
/// error for any decode failure - see `thumbnail_from_xex`'s doc comment.
fn decode_xex_image(
    raw_body: &[u8],
    layout: &xex_image::XexLayout,
    candidate_keys: &[&[u8; 16]],
    target_len: Option<usize>,
) -> Result<Option<Vec<u8>>> {
    for xex_key in candidate_keys {
        let mut body = raw_body.to_vec();
        if layout.encrypted {
            let Some(session_key) = layout.encrypted_session_key else {
                // Encrypted but no session key available - no key choice
                // will fix that, so don't bother looping further.
                return Ok(None);
            };
            if xex_crypto::decrypt_xex_body(xex_key, session_key, &mut body).is_err() {
                continue;
            }
        }

        let decompressed = match &layout.compression {
            CompressionKind::None => Some(xex_image::decompress_none(&body, target_len)),
            CompressionKind::Basic(runs) => {
                xex_image::decompress_basic(&body, runs, target_len).ok()
            }
            CompressionKind::Normal {
                window_size,
                first_block_size,
            } => xex_image::decompress_normal(&body, *window_size, *first_block_size, target_len)
                .ok(),
            CompressionKind::Unsupported(_) => None,
        };

        let Some(decompressed) = decompressed else {
            continue;
        };

        // The "MZ" DOS header magic doubles as a check that the key (or,
        // for unencrypted images, decompression) was correct.
        if decompressed.get(0..2) == Some(b"MZ") {
            return Ok(Some(decompressed));
        }
    }

    Ok(None)
}

/// Prefers the entry named for the title ID over trying every entry (see
/// module doc), falling back to every entry - stays lenient with odd
/// files where no title ID could be read, or where the name doesn't
/// match (some titles' resource entries have been observed not to follow
/// the convention exactly).
fn find_thumbnail_in_resources(
    image: &[u8],
    load_address: u32,
    resources: &[([u8; 8], u32, u32)],
    title_id: Option<u32>,
) -> Option<Vec<u8>> {
    if let Some(title_id) = title_id {
        let expected_name = format!("{title_id:08X}");
        for (name, address, size) in resources {
            if name.as_slice() != expected_name.as_bytes() {
                continue;
            }
            if let Some(png) = try_resource_as_thumbnail(image, load_address, *address, *size) {
                return Some(png);
            }
        }
    }

    for (_name, address, size) in resources {
        if let Some(png) = try_resource_as_thumbnail(image, load_address, *address, *size) {
            return Some(png);
        }
    }

    None
}

/// Slices one resource-table entry out of the decompressed image (VA ->
/// file offset via `load_address`) and tries it as an XDBF blob holding
/// the game's thumbnail.
fn try_resource_as_thumbnail(
    image: &[u8],
    load_address: u32,
    address: u32,
    size: u32,
) -> Option<Vec<u8>> {
    let offset = address.checked_sub(load_address)? as usize;
    let blob = image.get(offset..offset.saturating_add(size as usize))?;
    thumbnail_from_xdbf(blob).ok().flatten()
}

/// XDBF resource id for the title's thumbnail image.
const XDBF_THUMB_ID: u64 = 0x8000;

/// Locates the game's thumbnail inside an XDBF resource blob: `Thumb`
/// (id `0x8000`) under [`XdbfSection::Image`]. The bytes returned are
/// already a complete PNG. This is the thumbnail-specific lookup
/// *policy*; [`crate::core::xdbf`] just does the generic `(id, section)`
/// lookup underneath it.
fn thumbnail_from_xdbf(xdbf_bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    find_xdbf_resource(xdbf_bytes, XDBF_THUMB_ID, XdbfSection::Image as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::executable::xex_image::{
        FIELD_BASE_FILE_FORMAT, FIELD_EXECUTION_INFO, FIELD_IMAGE_BASE_ADDRESS, FIELD_RESOURCE_INFO,
    };
    use byteorder::{BE, ByteOrder};

    // XBE end-to-end tests.

    fn write_u32_le(buf: &mut Vec<u8>, offset: usize, value: u32) {
        if buf.len() < offset + 4 {
            buf.resize(offset + 4, 0);
        }
        buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn build_minimal_xbe(section_name: &str, section_data: &[u8]) -> Vec<u8> {
        const BASE_ADDR: u32 = 0x0001_0000;
        const SECTION_HEADER_FILE_OFFSET: usize = 0x140;

        let mut buf = vec![0u8; SECTION_HEADER_FILE_OFFSET];
        buf[0..4].copy_from_slice(b"XBEH");
        write_u32_le(&mut buf, 0x104, BASE_ADDR);
        write_u32_le(&mut buf, 0x11C, 1); // num_sections
        write_u32_le(
            &mut buf,
            0x120,
            BASE_ADDR + SECTION_HEADER_FILE_OFFSET as u32,
        );

        let name_file_offset = SECTION_HEADER_FILE_OFFSET + 56;
        let data_file_offset = name_file_offset + section_name.len() + 1;

        let mut header = vec![0u8; 56];
        write_u32_le(&mut header, 0x0C, data_file_offset as u32); // raw file address
        write_u32_le(&mut header, 0x10, section_data.len() as u32); // raw size
        write_u32_le(&mut header, 0x14, BASE_ADDR + name_file_offset as u32); // name VA

        buf.extend_from_slice(&header);
        buf.extend_from_slice(section_name.as_bytes());
        buf.push(0);
        buf.extend_from_slice(section_data);
        buf
    }

    fn build_xpr0_dxt1(width_log2: u8, dxt_block: &[u8; 8]) -> Vec<u8> {
        let header_size = 36u32; // real single-texture XPR0 header size
        let file_size = header_size + 8;

        let mut buf = Vec::new();
        buf.extend_from_slice(&0x3052_5058u32.to_le_bytes()); // XPR0
        buf.extend_from_slice(&file_size.to_le_bytes());
        buf.extend_from_slice(&header_size.to_le_bytes());
        buf.extend_from_slice(&(1u32 | (4u32 << 16)).to_le_bytes()); // flags: count=1, type=4 (texture)
        buf.extend_from_slice(&0u32.to_le_bytes()); // resource_data_offset (must be 0)
        buf.extend_from_slice(&0u32.to_le_bytes()); // unknown (must be 0)
        buf.push(0); // texture_misc1
        buf.push(12); // texture_format = DXT1
        buf.push(0); // texture_res1
        buf.push(width_log2); // texture_res2
        buf.extend_from_slice(&0u32.to_le_bytes()); // texture size field (must be 0 for power-of-2)
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // end-of-header marker
        buf.extend_from_slice(dxt_block);
        buf
    }

    #[test]
    fn thumbnail_from_xbe_decodes_dxt1_icon_end_to_end() {
        let dxt_block: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00];
        let xpr = build_xpr0_dxt1(2, &dxt_block); // 2^2 = 4x4
        let xbe = build_minimal_xbe("$$XTIMAGE", &xpr);

        let png = thumbnail_from_xbe(&xbe)
            .unwrap()
            .expect("should find a thumbnail");
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    }

    #[test]
    fn thumbnail_from_xbe_falls_back_to_xsimage() {
        let dxt_block: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00];
        let xpr = build_xpr0_dxt1(2, &dxt_block);
        let xbe = build_minimal_xbe("$$XSIMAGE", &xpr);
        assert!(thumbnail_from_xbe(&xbe).unwrap().is_some());
    }

    #[test]
    fn thumbnail_from_xbe_prefers_xtimage_when_both_sections_present() {
        const BASE_ADDR: u32 = 0x0001_0000;
        const SECTION_HEADER_FILE_OFFSET: usize = 0x140;

        // Title icon: a solid white 4x4 DXT1 block.
        let title_dxt: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00];
        let title_xpr = build_xpr0_dxt1(2, &title_dxt);
        let save_dxt: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let save_xpr = build_xpr0_dxt1(2, &save_dxt);

        let mut buf = vec![0u8; SECTION_HEADER_FILE_OFFSET];
        buf[0..4].copy_from_slice(b"XBEH");
        write_u32_le(&mut buf, 0x104, BASE_ADDR);
        write_u32_le(&mut buf, 0x11C, 2); // num_sections
        write_u32_le(
            &mut buf,
            0x120,
            BASE_ADDR + SECTION_HEADER_FILE_OFFSET as u32,
        );

        // Two section headers (56 bytes each), then both names, then both payloads.
        let headers_end = SECTION_HEADER_FILE_OFFSET + 2 * 56;
        let xtimage_name_offset = headers_end;
        let xsimage_name_offset = xtimage_name_offset + "$$XTIMAGE".len() + 1;
        let xtimage_data_offset = xsimage_name_offset + "$$XSIMAGE".len() + 1;
        let xsimage_data_offset = xtimage_data_offset + title_xpr.len();

        let mut header0 = vec![0u8; 56];
        write_u32_le(&mut header0, 0x0C, xtimage_data_offset as u32);
        write_u32_le(&mut header0, 0x10, title_xpr.len() as u32);
        write_u32_le(&mut header0, 0x14, BASE_ADDR + xtimage_name_offset as u32);

        let mut header1 = vec![0u8; 56];
        write_u32_le(&mut header1, 0x0C, xsimage_data_offset as u32);
        write_u32_le(&mut header1, 0x10, save_xpr.len() as u32);
        write_u32_le(&mut header1, 0x14, BASE_ADDR + xsimage_name_offset as u32);

        buf.extend_from_slice(&header0);
        buf.extend_from_slice(&header1);
        buf.extend_from_slice(b"$$XTIMAGE\0");
        buf.extend_from_slice(b"$$XSIMAGE\0");
        buf.extend_from_slice(&title_xpr);
        buf.extend_from_slice(&save_xpr);

        let png = thumbnail_from_xbe(&buf)
            .unwrap()
            .expect("should find a thumbnail");

        let expected = decode_xpr_to_png(&title_xpr).expect("title icon should decode");
        assert_eq!(
            png, expected,
            "title icon ($$XTIMAGE) should win over save icon ($$XSIMAGE)"
        );
    }

    #[test]
    fn thumbnail_from_xbe_returns_none_when_no_icon_section_present() {
        let xbe = build_minimal_xbe("$$SOMEOTHER", &[1, 2, 3]);
        assert!(thumbnail_from_xbe(&xbe).unwrap().is_none());
    }

    // thumbnail_from_xdbf lookup tests.

    fn build_synthetic_xdbf_with_data(section: u16, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"XDBF");
        buf.extend_from_slice(&0u32.to_be_bytes()); // version
        buf.extend_from_slice(&1u32.to_be_bytes()); // entry_table_len (capacity)
        buf.extend_from_slice(&1u32.to_be_bytes()); // entry_used
        buf.extend_from_slice(&0u32.to_be_bytes()); // free_table_len
        buf.extend_from_slice(&0u32.to_be_bytes()); // free_used
        buf.extend_from_slice(&section.to_be_bytes()); // entry.section
        buf.extend_from_slice(&XDBF_THUMB_ID.to_be_bytes()); // entry.id
        buf.extend_from_slice(&0u32.to_be_bytes()); // entry.offset
        buf.extend_from_slice(&(data.len() as u32).to_be_bytes()); // entry.size
        buf.extend_from_slice(data);
        buf
    }

    fn build_synthetic_xdbf(section: u16) -> Vec<u8> {
        build_synthetic_xdbf_with_data(section, b"PNG!")
    }

    #[test]
    fn thumbnail_from_xdbf_finds_thumb_under_image_section() {
        let buf = build_synthetic_xdbf(XdbfSection::Image as u16);
        let result = thumbnail_from_xdbf(&buf).unwrap();
        assert_eq!(result.as_deref(), Some(b"PNG!".as_slice()));
    }

    #[test]
    fn thumbnail_from_xdbf_does_not_match_under_string_table_section() {
        let buf = build_synthetic_xdbf(XdbfSection::StringTable as u16);
        assert!(thumbnail_from_xdbf(&buf).unwrap().is_none());
    }

    // XEX end-to-end tests.

    #[test]
    fn thumbnail_from_xex_locates_thumb_end_to_end_uncompressed() {
        const LOAD_ADDRESS: u32 = 0x0001_0000;
        const CODE_OFFSET: u32 = 0x80;

        let xdbf = build_synthetic_xdbf(XdbfSection::Image as u16);

        let mut header = vec![0u8; 0x30];
        header[0..4].copy_from_slice(b"XEX2");
        BE::write_u32(&mut header[0x08..], CODE_OFFSET);
        BE::write_u32(&mut header[0x14..], 3); // field_count
        BE::write_u32(&mut header[0x18..], FIELD_IMAGE_BASE_ADDRESS);
        BE::write_u32(&mut header[0x1C..], LOAD_ADDRESS);
        BE::write_u32(&mut header[0x20..], FIELD_RESOURCE_INFO);
        BE::write_u32(&mut header[0x24..], 0x30); // -> resource block at 0x30
        BE::write_u32(&mut header[0x28..], FIELD_BASE_FILE_FORMAT);
        BE::write_u32(&mut header[0x2C..], 0x44); // -> file format block at 0x44

        let mut resource_block = vec![0u8; 20];
        BE::write_u32(&mut resource_block[0..], 20); // block size
        resource_block[4..12].copy_from_slice(b"THUMBRES");
        BE::write_u32(&mut resource_block[12..], LOAD_ADDRESS); // VA
        BE::write_u32(&mut resource_block[16..], xdbf.len() as u32);

        let mut file_format_block = vec![0u8; 8];
        BE::write_u32(&mut file_format_block[0..], 8);

        let mut xex = header;
        xex.extend_from_slice(&resource_block);
        xex.extend_from_slice(&file_format_block);
        xex.resize(CODE_OFFSET as usize, 0);

        xex.extend_from_slice(b"MZ\0\0");
        let resource_va_for_body = LOAD_ADDRESS + 4;
        BE::write_u32(&mut xex[0x30 + 12..], resource_va_for_body);
        xex.extend_from_slice(&xdbf);

        let png = thumbnail_from_xex(&xex).unwrap();
        assert_eq!(png.as_deref(), Some(b"PNG!".as_slice()));
    }

    #[test]
    fn thumbnail_from_xex_prefers_resource_entry_matching_title_id() {
        const LOAD_ADDRESS: u32 = 0x0001_0000;
        const CODE_OFFSET: u32 = 0x80;
        const TITLE_ID: u32 = 0x4D5A_0001;

        let wrong_xdbf = build_synthetic_xdbf_with_data(XdbfSection::Image as u16, b"WRNG");
        let right_xdbf = build_synthetic_xdbf_with_data(XdbfSection::Image as u16, b"PNG!");

        let mut header = vec![0u8; 0x38];
        header[0..4].copy_from_slice(b"XEX2");
        BE::write_u32(&mut header[0x08..], CODE_OFFSET);
        BE::write_u32(&mut header[0x14..], 4); // field_count
        BE::write_u32(&mut header[0x18..], FIELD_IMAGE_BASE_ADDRESS);
        BE::write_u32(&mut header[0x1C..], LOAD_ADDRESS);
        BE::write_u32(&mut header[0x20..], FIELD_RESOURCE_INFO);
        BE::write_u32(&mut header[0x24..], 0x38); // -> resource block at 0x38
        BE::write_u32(&mut header[0x28..], FIELD_BASE_FILE_FORMAT);
        BE::write_u32(&mut header[0x2C..], 0x5C); // -> file format block at 0x5C
        BE::write_u32(&mut header[0x30..], FIELD_EXECUTION_INFO);
        BE::write_u32(&mut header[0x34..], 0x64); // -> execution info block at 0x64

        let entry_a_va = LOAD_ADDRESS + 4;
        let entry_b_va = entry_a_va + wrong_xdbf.len() as u32;

        let mut resource_block = vec![0u8; 36];
        BE::write_u32(&mut resource_block[0..], 36); // block size
        resource_block[4..12].copy_from_slice(b"AAAAAAAA");
        BE::write_u32(&mut resource_block[12..], entry_a_va);
        BE::write_u32(&mut resource_block[16..], wrong_xdbf.len() as u32);
        let title_name = format!("{TITLE_ID:08X}");
        resource_block[20..28].copy_from_slice(title_name.as_bytes());
        BE::write_u32(&mut resource_block[28..], entry_b_va);
        BE::write_u32(&mut resource_block[32..], right_xdbf.len() as u32);

        let mut file_format_block = vec![0u8; 8];
        BE::write_u32(&mut file_format_block[0..], 8);

        let mut execution_info_block = vec![0u8; 16];
        BE::write_u32(&mut execution_info_block[12..], TITLE_ID); // title_id is the 4th field

        let mut xex = header;
        xex.extend_from_slice(&resource_block);
        xex.extend_from_slice(&file_format_block);
        xex.extend_from_slice(&execution_info_block);
        xex.resize(CODE_OFFSET as usize, 0);
        xex.extend_from_slice(b"MZ\0\0");
        xex.extend_from_slice(&wrong_xdbf);
        xex.extend_from_slice(&right_xdbf);

        let png = thumbnail_from_xex(&xex).unwrap();
        assert_eq!(
            png.as_deref(),
            Some(b"PNG!".as_slice()),
            "should pick the resource entry named for the title ID, not just the first one that parses"
        );
    }

    #[test]
    fn thumbnail_from_xex_returns_none_when_encrypted_without_key() {
        const LOAD_ADDRESS: u32 = 0x0001_0000;
        const CODE_OFFSET: u32 = 0x80;

        let mut header = vec![0u8; 0x30];
        header[0..4].copy_from_slice(b"XEX2");
        BE::write_u32(&mut header[0x08..], CODE_OFFSET);
        BE::write_u32(&mut header[0x14..], 3);
        BE::write_u32(&mut header[0x18..], FIELD_IMAGE_BASE_ADDRESS);
        BE::write_u32(&mut header[0x1C..], LOAD_ADDRESS);
        BE::write_u32(&mut header[0x20..], FIELD_RESOURCE_INFO);
        BE::write_u32(&mut header[0x24..], 0x30);
        BE::write_u32(&mut header[0x28..], FIELD_BASE_FILE_FORMAT);
        BE::write_u32(&mut header[0x2C..], 0x44);

        let mut resource_block = vec![0u8; 20];
        BE::write_u32(&mut resource_block[0..], 20);
        resource_block[4..12].copy_from_slice(b"THUMBRES");
        BE::write_u32(&mut resource_block[12..], LOAD_ADDRESS);
        BE::write_u32(&mut resource_block[16..], 4);

        let mut file_format_block = vec![0u8; 8];
        BE::write_u32(&mut file_format_block[0..], 8);
        // encryption_type = 1 (encrypted) but no valid session key
        file_format_block[4..6].copy_from_slice(&1u16.to_be_bytes());

        let mut xex = header;
        xex.extend_from_slice(&resource_block);
        xex.extend_from_slice(&file_format_block);
        xex.resize(CODE_OFFSET as usize, 0);
        xex.extend_from_slice(b"XDBF"); // not a real blob - shouldn't matter

        assert!(thumbnail_from_xex(&xex).unwrap().is_none());
    }

    #[test]
    fn thumbnail_from_xex_returns_none_without_resource_info_field() {
        let mut header = vec![0u8; 0x18];
        header[0..4].copy_from_slice(b"XEX2");
        BE::write_u32(&mut header[0x08..], 0x18);
        BE::write_u32(&mut header[0x14..], 0); // field_count = 0
        assert!(thumbnail_from_xex(&header).unwrap().is_none());
    }

    #[test]
    fn thumbnail_from_xex_rejects_bad_magic_as_none() {
        // Wrong magic still returns Ok(None), not Err.
        let not_a_xex = vec![0u8; 64];
        assert!(thumbnail_from_xex(&not_a_xex).unwrap().is_none());
    }
}
