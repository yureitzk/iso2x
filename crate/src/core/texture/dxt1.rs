//! DXT1 (S3TC/BC1) block decompression.
//! <https://en.wikipedia.org/wiki/S3_Texture_Compression#DXT1>

/// Decodes a whole DXT1-compressed image into interleaved 8-bit RGBA.
pub(crate) fn decode_dxt1(width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let blocks_w = width.div_ceil(4);
    let blocks_h = height.div_ceil(4);

    let required_bytes = (blocks_w as usize)
        .checked_mul(blocks_h as usize)?
        .checked_mul(8)?;
    if data.len() < required_bytes {
        return None;
    }

    let pixel_bytes = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    let mut out = vec![0u8; pixel_bytes];

    let mut pos = 0usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block = data.get(pos..pos + 8)?;
            pos += 8;
            decompress_block_dxt1(bx * 4, by * 4, width, height, block, &mut out);
        }
    }

    Some(out)
}

// midpoint() of two u8 channel values always fits back in a u8.
//
// x/y (pixel coords) and r/g/b/a (color channels) are the conventional,
// most-readable names for this domain - not worth spelling out.
#[allow(clippy::cast_possible_truncation, clippy::many_single_char_names)]
fn decompress_block_dxt1(bx: u32, by: u32, width: u32, height: u32, block: &[u8], out: &mut [u8]) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (r0, g0, b0) = unpack_565(c0);
    let (r1, g1, b1) = unpack_565(c1);
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);

    for i in 0..4u32 {
        for j in 0..4u32 {
            let (x, y) = (bx + j, by + i);
            if x >= width || y >= height {
                continue;
            }
            let code = (indices >> (2 * (4 * i + j))) & 3;
            // c0 > c1: opaque mode, index 2/3 interpolate toward c0/c1.
            // c0 <= c1: punch-through-alpha mode, index 3 is transparent
            // instead of a third color.
            let (r, g, b, a) = if c0 > c1 {
                match code {
                    0 => (r0, g0, b0, 255),
                    1 => (r1, g1, b1, 255),
                    2 => {
                        let (r, g, b) = lerp3(1, r0, r1, g0, g1, b0, b1);
                        (r, g, b, 255)
                    }
                    _ => {
                        let (r, g, b) = lerp3(2, r0, r1, g0, g1, b0, b1);
                        (r, g, b, 255)
                    }
                }
            } else {
                match code {
                    0 => (r0, g0, b0, 255),
                    1 => (r1, g1, b1, 255),
                    2 => (
                        u32::midpoint(u32::from(r0), u32::from(r1)) as u8,
                        u32::midpoint(u32::from(g0), u32::from(g1)) as u8,
                        u32::midpoint(u32::from(b0), u32::from(b1)) as u8,
                        255,
                    ),
                    _ => (0, 0, 0, 0),
                }
            };

            let idx = ((y * width + x) * 4) as usize;
            out[idx] = r;
            out[idx + 1] = g;
            out[idx + 2] = b;
            out[idx + 3] = a;
        }
    }
}

/// `weight_c1` is 1 for `(2*c0 + c1) / 3`, 2 for `(c0 + 2*c1) / 3`.
// weight_c0 + weight_c1 == 3, so the weighted average always fits in a u8.
//
// pub(crate): dxt3.rs's color block is identical to DXT1's opaque mode
// and reuses this.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn lerp3(
    weight_c1: u32,
    r0: u8,
    r1: u8,
    g0: u8,
    g1: u8,
    b0: u8,
    b1: u8,
) -> (u8, u8, u8) {
    let weight_c0 = 3 - weight_c1;
    (
        ((weight_c0 * u32::from(r0) + weight_c1 * u32::from(r1)) / 3) as u8,
        ((weight_c0 * u32::from(g0) + weight_c1 * u32::from(g1)) / 3) as u8,
        ((weight_c0 * u32::from(b0) + weight_c1 * u32::from(b1)) / 3) as u8,
    )
}

/// RGB565 -> 8-bit-per-channel via bit replication rather than a plain
/// left shift, so e.g. a maxed 5-bit channel expands to 255, not 248.
//
// pub(crate): also used by swizzle.rs's decode_r5g6b5_swizzled.
pub(crate) fn unpack_565(c: u16) -> (u8, u8, u8) {
    let r5 = u32::from((c >> 11) & 0x1F);
    let g6 = u32::from((c >> 5) & 0x3F);
    let b5 = u32::from(c & 0x1F);
    (
        expand_bits(r5, 16, 32),
        expand_bits(g6, 32, 64),
        expand_bits(b5, 16, 32),
    )
}

// Bit-replication formula; always yields 0..=255 for a 5- or 6-bit input.
#[allow(clippy::cast_possible_truncation)]
fn expand_bits(v: u32, add: u32, div: u32) -> u8 {
    let t = v * 255 + add;
    ((t / div + t) / div) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpack_565_white_expands_to_255() {
        assert_eq!(unpack_565(0xFFFF), (255, 255, 255));
    }

    #[test]
    fn unpack_565_black_expands_to_zero() {
        assert_eq!(unpack_565(0x0000), (0, 0, 0));
    }

    #[test]
    fn decompress_block_dxt1_punch_through_alpha_is_transparent_not_black() {
        let block: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF];
        let mut out = vec![0u8; 4 * 4 * 4];
        decompress_block_dxt1(0, 0, 4, 4, &block, &mut out);
        for px in out.chunks_exact(4) {
            assert_eq!(
                px,
                [0, 0, 0, 0],
                "punch-through pixel should be fully transparent"
            );
        }
    }

    #[test]
    fn decompress_block_dxt1_opaque_mode_index_2_weights_toward_c0() {
        let c0 = 0xF800u16.to_le_bytes();
        let c1 = 0x001Fu16.to_le_bytes();
        // indices: texel 0 = code 2, texel 1 = code 3, rest = 0.
        let indices: u32 = 0b10 | (0b11 << 2);
        let block = [c0[0], c0[1], c1[0], c1[1], indices as u8, 0, 0, 0];
        let mut out = vec![0u8; 4 * 4 * 4];
        decompress_block_dxt1(0, 0, 4, 4, &block, &mut out);

        let px = |x: usize| &out[x * 4..x * 4 + 4];
        // index 2 = (2*c0 + c1)/3: red-heavy, i.e. R > B.
        assert!(
            px(0)[0] > px(0)[2],
            "index 2 should weight toward c0 (red): got {:?}",
            px(0)
        );
        // index 3 = (c0 + 2*c1)/3: blue-heavy, i.e. B > R.
        assert!(
            px(1)[2] > px(1)[0],
            "index 3 should weight toward c1 (blue): got {:?}",
            px(1)
        );
    }

    #[test]
    fn decode_dxt1_decodes_a_valid_single_block() {
        let c0 = 0xF800u16.to_le_bytes();
        let c1 = 0x001Fu16.to_le_bytes();
        let block = [c0[0], c0[1], c1[0], c1[1], 0, 0, 0, 0];
        let out = decode_dxt1(4, 4, &block).expect("a full single block should decode");
        assert_eq!(out.len(), 4 * 4 * 4);
    }

    #[test]
    fn decode_dxt1_rejects_huge_dimensions_backed_by_no_data() {
        assert!(decode_dxt1(60917, 21259, &[]).is_none());
    }

    #[test]
    fn decode_dxt1_rejects_short_buffer() {
        // A 4x4 image needs one 8-byte block; give it 4.
        assert!(decode_dxt1(4, 4, &[0u8; 4]).is_none());
    }
}
