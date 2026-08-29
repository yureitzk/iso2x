//! DXT3 (S3TC/BC2) block decompression.
//!
//! A DXT3 block is 16 bytes: 8 bytes of explicit, non-interpolated 4-bit
//! alpha (one nibble per texel, row-major), followed by an 8-byte
//! DXT1-shaped color block (`c0`, `c1`, 2-bit-per-texel palette indices -
//! see [`super::dxt1`]). DXT3's color block is always 4-color/opaque
//! (no punch-through-alpha branch like DXT1), since alpha has its own
//! dedicated bits.
//!
//! <https://en.wikipedia.org/wiki/S3_Texture_Compression#DXT2_and_DXT3>

use super::dxt1::{lerp3, unpack_565};

/// Decodes a whole DXT3-compressed image into interleaved 8-bit RGBA.
pub(crate) fn decode_dxt3(width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let blocks_w = width.div_ceil(4);
    let blocks_h = height.div_ceil(4);

    let required_bytes = (blocks_w as usize)
        .checked_mul(blocks_h as usize)?
        .checked_mul(16)?;
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
            let block = data.get(pos..pos + 16)?;
            pos += 16;
            decompress_block_dxt3(bx * 4, by * 4, width, height, block, &mut out);
        }
    }

    Some(out)
}

// The alpha nibble is masked with 0xF before the cast, so it always fits
// in a u8.
//
// x/y (pixel coords) and r/g/b/a (color channels) are the conventional,
// most-readable names for this domain - not worth spelling out.
#[allow(clippy::cast_possible_truncation, clippy::many_single_char_names)]
fn decompress_block_dxt3(bx: u32, by: u32, width: u32, height: u32, block: &[u8], out: &mut [u8]) {
    let (alpha_block, color_block) = block.split_at(8);
    let c0 = u16::from_le_bytes([color_block[0], color_block[1]]);
    let c1 = u16::from_le_bytes([color_block[2], color_block[3]]);
    let (r0, g0, b0) = unpack_565(c0);
    let (r1, g1, b1) = unpack_565(c1);
    let (r2, g2, b2) = lerp3(1, r0, r1, g0, g1, b0, b1);
    let (r3, g3, b3) = lerp3(2, r0, r1, g0, g1, b0, b1);
    let indices = u32::from_le_bytes([
        color_block[4],
        color_block[5],
        color_block[6],
        color_block[7],
    ]);
    // 16 four-bit alpha nibbles, row-major, packed as a u64 for per-texel
    // extraction below (texel index = 4*i+j).
    let alpha_bits = u64::from_le_bytes(alpha_block.try_into().unwrap());

    for i in 0..4u32 {
        for j in 0..4u32 {
            let (x, y) = (bx + j, by + i);
            if x >= width || y >= height {
                continue;
            }
            let code = (indices >> (2 * (4 * i + j))) & 3;
            let (r, g, b) = match code {
                0 => (r0, g0, b0),
                1 => (r1, g1, b1),
                2 => (r2, g2, b2),
                _ => (r3, g3, b3),
            };
            let nibble = ((alpha_bits >> (4 * (4 * i + j))) & 0xF) as u8;
            // 4-bit -> 8-bit via nibble replication, e.g. 0xA -> 0xAA.
            let a = nibble | (nibble << 4);

            let idx = ((y * width + x) * 4) as usize;
            out[idx] = r;
            out[idx + 1] = g;
            out[idx + 2] = b;
            out[idx + 3] = a;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompress_block_dxt3_full_alpha_white_block() {
        let mut block = [0xFFu8; 16];
        block[8] = 0xFF;
        block[9] = 0xFF;
        block[10] = 0xFF;
        block[11] = 0xFF;
        block[12] = 0x00;
        block[13] = 0x00;
        block[14] = 0x00;
        block[15] = 0x00;
        let mut out = vec![0u8; 4 * 4 * 4];
        decompress_block_dxt3(0, 0, 4, 4, &block, &mut out);
        for px in out.chunks_exact(4) {
            assert_eq!(px, [255, 255, 255, 255]);
        }
    }

    #[test]
    fn decompress_block_dxt3_alpha_nibbles_extracted_in_texel_order() {
        let mut block = [0u8; 16];
        block[0] = 0x21; // texel(0,0)=0x1, texel(1,0)=0x2
        block[1] = 0x43; // texel(2,0)=0x3, texel(3,0)=0x4
        block[8] = 0xFF; // c0 = white
        block[9] = 0xFF;
        block[10] = 0xFF; // c1 = white too, so color is white regardless
        block[11] = 0xFF;
        let mut out = vec![0u8; 4 * 4 * 4];
        decompress_block_dxt3(0, 0, 4, 4, &block, &mut out);
        let alpha_at = |x: usize, y: usize| out[(y * 4 + x) * 4 + 3];
        assert_eq!(alpha_at(0, 0), 0x11); // nibble 0x1 replicated -> 0x11
        assert_eq!(alpha_at(1, 0), 0x22);
        assert_eq!(alpha_at(2, 0), 0x33);
        assert_eq!(alpha_at(3, 0), 0x44);
    }

    #[test]
    fn decode_dxt3_rejects_short_buffer() {
        // A 4x4 image needs one 16-byte block; give it 8.
        assert!(decode_dxt3(4, 4, &[0u8; 8]).is_none());
    }

    #[test]
    fn decode_dxt3_rejects_huge_dimensions_backed_by_no_data() {
        assert!(decode_dxt3(60917, 21259, &[]).is_none());
    }
}
