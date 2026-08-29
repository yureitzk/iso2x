//! XPR0 texture container - the format an XBE's `$$XSIMAGE`/`$$XTIMAGE`
//! section holds. Ties the [`super::dxt1`]/[`super::dxt3`]/
//! [`super::swizzle`] decoders to that specific container and to PNG
//! output.
//! <https://xboxdevwiki.net/XPR>

use super::dxt1::decode_dxt1;
use super::dxt3::decode_dxt3;
use super::swizzle::{
    decode_a4r4g4b4_swizzled, decode_argb_swizzled, decode_r5g6b5_swizzled, decode_rgb_swizzled,
    decode_rgba_swizzled,
};
use anyhow::{Context, Result};
use binrw::{BinRead, binread};
use std::io::Cursor;

/// Marks the end of an XPR0 header's resource-entry array (`dwEndOfHeader`
/// in Cxbx-Reloaded's `XprImageHeader`, `Xbe.h`). For the single-texture
/// case this decoder handles, it sits at a fixed offset (32) right after
/// the one resource entry - independent of `header_size`, which just
/// says how far the (`0xAD`-padded) header block extends beyond that.
const XPR_END_OF_HEADER_MARKER: u32 = 0xFFFF_FFFF;

/// The fixed 36-byte single-texture XPR0 header (per `XPR_Resource`/
/// `XPR_Texture` in the [xboxdevwiki XPR spec](https://xboxdevwiki.net/XPR)):
/// magic(4) + `file_size(4)` + `header_size(4)` + flags(4) +
/// `resource_data_offset(4)` + unknown(4) + `texture_misc1(1)` +
/// `texture_format(1)` + `texture_res1(1)` + `texture_res2(1)` +
/// `texture_size_field(4)` + `end_of_header(4)`, the last field always
/// `0xFFFFFFFF`.
///
/// Every field this decoder doesn't use downstream is `#[br(temp)]` -
/// read to validate or to keep the cursor aligned, then discarded.
#[binread]
#[derive(Debug)]
#[br(little, magic = b"XPR0")]
struct XprHeader {
    file_size: u32,
    header_size: u32,
    /// Packs a resource count (low 16 bits) and a resource type (bits
    /// 16-18). A single-texture XPR0 - the only shape this decoder
    /// understands - has count == 1 and type == 4; anything else isn't
    /// a container this decoder can read.
    #[br(assert(flags & 0xffff == 1, "unsupported XPR0 resource count"))]
    #[br(assert((flags >> 16) & 0x7 == 4, "unsupported XPR0 resource type"))]
    flags: u32,
    /// Offset of this resource's own data within the data region. For
    /// the single-resource case that data starts right at the
    /// beginning of the region (located via `header_size`), so this
    /// must be 0.
    #[br(temp, assert(resource_data_offset == 0))]
    resource_data_offset: u32,
    /// Documented as always zero.
    #[br(temp, assert(unknown == 0))]
    unknown: u32,
    #[br(temp)]
    texture_misc1: u8,
    texture_format: u8,
    #[br(temp)]
    texture_res1: u8,
    texture_res2: u8,
    /// Non-power-of-2 texture dimensions; must be zero for power-of-2
    /// (e.g. DXT) textures, which is always the case for the square
    /// icons this decoder reads.
    #[br(temp, assert(texture_size_field == 0))]
    texture_size_field: u32,
    /// End-of-header marker: always immediately follows the one
    /// resource entry (offset 32), regardless of how much `0xAD`
    /// padding follows it out to `header_size`.
    #[br(temp, assert(end_of_header == XPR_END_OF_HEADER_MARKER))]
    end_of_header: u32,
}

/// Decodes one XPR0 texture container (an XBE `$$XSIMAGE`/`$$XTIMAGE`
/// section's raw bytes) into a PNG. Returns `None` for an unsupported
/// texture format, or anything else that doesn't look right.
pub(crate) fn decode_xpr_to_png(section: &[u8]) -> Option<Vec<u8>> {
    let mut r = Cursor::new(section);
    let header = XprHeader::read(&mut r).ok()?;

    // XBE icons are always square; both dimensions share this field.
    let side = 1u32.checked_shl(u32::from(header.texture_res2))?;
    // Cap against a corrupt/hostile size before allocating the buffer.
    if side == 0 || side > 1024 {
        return None;
    }
    let (width, height) = (side, side);

    let image_start = header.header_size as usize;
    let image_end = (header.file_size as usize).min(section.len());
    if image_start > image_end {
        return None;
    }
    let image_data = section.get(image_start..image_end)?;

    // Format codes per <https://github.com/Team-Resurgent/XboxToolkit>
    // (`XprUtility.cs`).
    let rgba = match header.texture_format {
        12 => decode_dxt1(width, height, image_data)?,
        6 => decode_argb_swizzled(width, height, image_data)?,
        14 => decode_dxt3(width, height, image_data)?,
        5 => decode_r5g6b5_swizzled(width, height, image_data)?,
        4 => decode_a4r4g4b4_swizzled(width, height, image_data)?,
        7 => decode_rgb_swizzled(width, height, image_data)?,
        0x3c => decode_rgba_swizzled(width, height, image_data)?,
        _ => return None,
    };

    encode_png(width, height, &rgba).ok()
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .context("failed to write PNG header")?;
        writer
            .write_image_data(rgba)
            .context("failed to write PNG image data")?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_xpr_to_png_rejects_bad_magic() {
        let bogus = vec![0u8; 32];
        assert!(decode_xpr_to_png(&bogus).is_none());
    }

    #[test]
    fn decode_xpr_to_png_rejects_short_buffer() {
        assert!(decode_xpr_to_png(&[0u8; 10]).is_none());
    }

    #[test]
    fn decode_xpr_to_png_rejects_28_to_31_byte_buffer() {
        for len in 28..32 {
            assert!(
                decode_xpr_to_png(&vec![0u8; len]).is_none(),
                "{len}-byte buffer should be rejected as too short"
            );
        }
    }

    const VALID_FLAGS: u32 = 1 | (4 << 16);

    fn build_xpr0(format: u8, payload: &[u8]) -> Vec<u8> {
        build_xpr0_with_header(
            format,
            payload,
            VALID_FLAGS,
            0,
            0,
            0,
            XPR_END_OF_HEADER_MARKER,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_xpr0_with_header(
        format: u8,
        payload: &[u8],
        flags: u32,
        resource_data_offset: u32,
        unknown: u32,
        texture_size_field: u32,
        end_of_header: u32,
    ) -> Vec<u8> {
        let header_size = 36u32;
        let file_size = header_size + payload.len() as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(b"XPR0");
        buf.extend_from_slice(&file_size.to_le_bytes());
        buf.extend_from_slice(&header_size.to_le_bytes());
        buf.extend_from_slice(&flags.to_le_bytes());
        buf.extend_from_slice(&resource_data_offset.to_le_bytes());
        buf.extend_from_slice(&unknown.to_le_bytes());
        buf.push(0); // texture_misc1
        buf.push(format);
        buf.push(0); // texture_res1
        buf.push(2); // texture_res2 -> side = 1 << 2 = 4
        buf.extend_from_slice(&texture_size_field.to_le_bytes());
        buf.extend_from_slice(&end_of_header.to_le_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    #[test]
    fn decode_xpr_to_png_rejects_an_unknown_format() {
        // Format 99 isn't one of the seven handled formats.
        let xpr = build_xpr0(99, &[0u8; 64]);
        assert!(decode_xpr_to_png(&xpr).is_none());
    }

    #[test]
    fn decode_xpr_to_png_rejects_resource_count_other_than_one() {
        let bad_flags = 2 | (4 << 16); // count = 2
        let xpr =
            build_xpr0_with_header(12, &[0u8; 64], bad_flags, 0, 0, 0, XPR_END_OF_HEADER_MARKER);
        assert!(decode_xpr_to_png(&xpr).is_none());
    }

    #[test]
    fn decode_xpr_to_png_rejects_resource_type_other_than_texture() {
        let bad_flags = 1 | (2 << 16); // type = 2, not 4 (texture)
        let xpr =
            build_xpr0_with_header(12, &[0u8; 64], bad_flags, 0, 0, 0, XPR_END_OF_HEADER_MARKER);
        assert!(decode_xpr_to_png(&xpr).is_none());
    }

    #[test]
    fn decode_xpr_to_png_rejects_nonzero_unused_fields() {
        let xpr_data_offset = build_xpr0_with_header(
            12,
            &[0u8; 64],
            VALID_FLAGS,
            1, // resource_data_offset should be 0
            0,
            0,
            XPR_END_OF_HEADER_MARKER,
        );
        assert!(decode_xpr_to_png(&xpr_data_offset).is_none());

        let xpr_unknown = build_xpr0_with_header(
            12,
            &[0u8; 64],
            VALID_FLAGS,
            0,
            1, // unknown should be 0
            0,
            XPR_END_OF_HEADER_MARKER,
        );
        assert!(decode_xpr_to_png(&xpr_unknown).is_none());
    }

    #[test]
    fn decode_xpr_to_png_rejects_nonzero_texture_size_field() {
        let xpr = build_xpr0_with_header(
            12,
            &[0u8; 64],
            VALID_FLAGS,
            0,
            0,
            1, // texture_size_field should be 0 for power-of-2 textures
            XPR_END_OF_HEADER_MARKER,
        );
        assert!(decode_xpr_to_png(&xpr).is_none());
    }

    #[test]
    fn decode_xpr_to_png_rejects_missing_end_of_header_marker() {
        let xpr = build_xpr0_with_header(12, &[0u8; 64], VALID_FLAGS, 0, 0, 0, 0);
        assert!(decode_xpr_to_png(&xpr).is_none());
    }

    #[test]
    fn decode_xpr_to_png_decodes_dxt1() {
        let mut block = [0u8; 8];
        block[0..2].copy_from_slice(&0xFFFFu16.to_le_bytes()); // c0
        block[2..4].copy_from_slice(&0xFFFFu16.to_le_bytes()); // c1
        // indices = 0 already via the zeroed buffer.
        let xpr = build_xpr0(12, &block); // format 12 = DXT1
        let png = decode_xpr_to_png(&xpr).expect("DXT1 should decode");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_xpr_decode() {
        let mut block = [0u8; 8];
        block[0..2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        block[2..4].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let xpr = build_xpr0(12, &block);
        let dir = "fuzz/corpus/xpr_decode";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-minimal-dxt1"), &xpr)
            .expect("seed file should be writable");
    }

    #[test]
    fn decode_xpr_to_png_decodes_dxt3() {
        let mut block = [0xFFu8; 16];
        block[12] = 0x00;
        block[13] = 0x00;
        block[14] = 0x00;
        block[15] = 0x00;
        let xpr = build_xpr0(14, &block);
        let png = decode_xpr_to_png(&xpr).expect("DXT3 should decode");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn decode_xpr_to_png_decodes_r5g6b5() {
        let payload = vec![0xFFu8; 4 * 4 * 2];
        let xpr = build_xpr0(5, &payload);
        let png = decode_xpr_to_png(&xpr).expect("R5G6B5 should decode");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn decode_xpr_to_png_decodes_a4r4g4b4() {
        let payload = vec![0xFFu8; 4 * 4 * 2];
        let xpr = build_xpr0(4, &payload);
        let png = decode_xpr_to_png(&xpr).expect("A4R4G4B4 should decode");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn decode_xpr_to_png_decodes_rgb() {
        let payload = vec![0xFFu8; 4 * 4 * 4];
        let xpr = build_xpr0(7, &payload);
        let png = decode_xpr_to_png(&xpr).expect("RGB should decode");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn decode_xpr_to_png_decodes_rgba() {
        let payload = vec![0xFFu8; 4 * 4 * 4];
        let xpr = build_xpr0(0x3c, &payload);
        let png = decode_xpr_to_png(&xpr).expect("RGBA should decode");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");
    }
}
