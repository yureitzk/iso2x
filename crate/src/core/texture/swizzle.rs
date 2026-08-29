//! Xbox tiled ("swizzled") texture layout - generic addressing math, no
//! knowledge of any particular pixel format or container. Uses Morton
//! (Z-order) interleaving of the x/y/z bits.
//!
//! The `decode_*_swizzled` functions below all share the same
//! `unswizzle_bpp`/`unswizzle_rect` addressing math and differ only in
//! per-pixel byte width and channel unpacking, one per texture format
//! code matched in [`super::xpr::decode_xpr_to_png`].

/// Un-swizzles a tiled ARGB image and reorders it into PNG-ready RGBA.
pub(crate) fn decode_argb_swizzled(width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let linear = unswizzle_bpp(width, height, data, 4)?;

    // Stored as [B, G, R, A] - convert to RGBA for PNG.
    let mut out = vec![0u8; linear.len()];
    for px in 0..(width as usize) * (height as usize) {
        let i = px * 4;
        out[i] = linear[i + 2];
        out[i + 1] = linear[i + 1];
        out[i + 2] = linear[i];
        out[i + 3] = linear[i + 3];
    }
    Some(out)
}

/// Un-swizzles a tiled RGB (no-alpha) image - byte order `[B, G, R,
/// unused]` per texel; output alpha is always 255. XPR format code
/// `0x07`.
pub(crate) fn decode_rgb_swizzled(width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let linear = unswizzle_bpp(width, height, data, 4)?;

    let mut out = vec![0u8; linear.len()];
    for px in 0..(width as usize) * (height as usize) {
        let i = px * 4;
        out[i] = linear[i + 2]; // R
        out[i + 1] = linear[i + 1]; // G
        out[i + 2] = linear[i]; // B
        out[i + 3] = 255; // 4th byte is unused padding, not alpha
    }
    Some(out)
}

/// Un-swizzles a tiled RGBA image stored in `[A, B, G, R]` byte order per
/// texel - distinct from `decode_argb_swizzled`'s `[B, G, R, A]`. XPR
/// format code `0x3C`.
pub(crate) fn decode_rgba_swizzled(width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let linear = unswizzle_bpp(width, height, data, 4)?;

    let mut out = vec![0u8; linear.len()];
    for px in 0..(width as usize) * (height as usize) {
        let i = px * 4;
        out[i] = linear[i + 3]; // R
        out[i + 1] = linear[i + 2]; // G
        out[i + 2] = linear[i + 1]; // B
        out[i + 3] = linear[i]; // A
    }
    Some(out)
}

/// Un-swizzles a tiled, uncompressed R5G6B5 (16-bit, no alpha) image.
/// XPR format code `0x05`. Uses `unpack_565_shift_only` (plain left
/// shift), not the bit-replicated expansion `dxt1::unpack_565` does for
/// DXT1's embedded endpoint colors - the two channel-expansion methods
/// aren't interchangeable between formats.
// c (packed pixel), r/g/b (color channels), and o (output offset) are
// conventional, readable names for this domain - not worth spelling out.
#[allow(clippy::many_single_char_names)]
pub(crate) fn decode_r5g6b5_swizzled(width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let linear = unswizzle_bpp(width, height, data, 2)?;

    let pixel_bytes = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    let mut out = vec![0u8; pixel_bytes];
    for px in 0..(width as usize) * (height as usize) {
        let c = u16::from_le_bytes([linear[px * 2], linear[px * 2 + 1]]);
        let (r, g, b) = unpack_565_shift_only(c);
        let o = px * 4;
        out[o] = r;
        out[o + 1] = g;
        out[o + 2] = b;
        out[o + 3] = 255;
    }
    Some(out)
}

/// Shift-only RGB565 -> 8-bit-per-channel expansion: `channel << (8 -
/// bit_width)` per channel, no bit replication - unlike
/// [`super::dxt1::unpack_565`].
fn unpack_565_shift_only(c: u16) -> (u8, u8, u8) {
    let r5 = ((c >> 11) & 0x1F) as u8;
    let g6 = ((c >> 5) & 0x3F) as u8;
    let b5 = (c & 0x1F) as u8;
    (r5 << 3, g6 << 2, b5 << 3)
}

/// Un-swizzles a tiled, uncompressed A4R4G4B4 (16-bit) image. Each
/// nibble goes into the top 4 bits of its output byte (not
/// bit-replicated like `unpack_565`). XPR format code `0x04`.
pub(crate) fn decode_a4r4g4b4_swizzled(width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let linear = unswizzle_bpp(width, height, data, 2)?;

    let pixel_bytes = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    let mut out = vec![0u8; pixel_bytes];
    for px in 0..(width as usize) * (height as usize) {
        let byte0 = linear[px * 2];
        let byte1 = linear[px * 2 + 1];
        let blue = (byte0 & 0x0F) << 4;
        let green = byte0 & 0xF0;
        let red = (byte1 & 0x0F) << 4;
        let alpha = byte1 & 0xF0;
        let o = px * 4;
        out[o] = red;
        out[o + 1] = green;
        out[o + 2] = blue;
        out[o + 3] = alpha;
    }
    Some(out)
}

/// Un-swizzles `data` into a linear buffer with `bpp` bytes per pixel, no
/// channel reinterpretation - shared by every `decode_*_swizzled`
/// function above, which each just interpret the resulting bytes
/// differently.
fn unswizzle_bpp(width: u32, height: u32, data: &[u8], bpp: u32) -> Option<Vec<u8>> {
    let pixel_bytes = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(bpp as usize)?;

    if data.len() < pixel_bytes {
        return None;
    }
    // Guards against overflow when height == 0 lets a huge width through
    // (pixel_bytes is 0 either way, so the check above doesn't catch it).
    let pitch = width.checked_mul(bpp)?;
    let mut linear = vec![0u8; pixel_bytes];
    unswizzle_rect(data, width, height, &mut linear, pitch, bpp);
    Some(linear)
}

fn unswizzle_rect(src: &[u8], width: u32, height: u32, dst: &mut [u8], pitch: u32, bpp: u32) {
    unswizzle_box(src, width, height, 1, dst, pitch, 0, bpp);
}

#[allow(clippy::too_many_arguments)]
fn unswizzle_box(
    src: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    dst: &mut [u8],
    row_pitch: u32,
    slice_pitch: u32,
    bpp: u32,
) {
    // Avoids a slow no-op loop when one dimension is 0 but another is huge
    // (found by fuzzing: width == 0, height near u32::MAX hung for ~4B iters).
    if width == 0 || height == 0 || depth == 0 {
        return;
    }
    let (mask_x, mask_y, mask_z) = generate_swizzle_masks(width, height, depth);
    let mut dst_base = 0u32;
    for _z in 0..depth {
        for y in 0..height {
            for x in 0..width {
                let src_offset = swizzled_offset(x, y, 0, mask_x, mask_y, mask_z, bpp) as usize;
                let dst_offset = (dst_base + y * row_pitch + x * bpp) as usize;
                if src_offset + bpp as usize <= src.len() && dst_offset + bpp as usize <= dst.len()
                {
                    dst[dst_offset..dst_offset + bpp as usize]
                        .copy_from_slice(&src[src_offset..src_offset + bpp as usize]);
                }
            }
        }
        dst_base += slice_pitch;
    }
}

/// Builds the per-axis bitmasks that interleave x/y/z into a swizzled offset.
fn generate_swizzle_masks(width: u32, height: u32, depth: u32) -> (u32, u32, u32) {
    let (mut x, mut y, mut z) = (0u32, 0u32, 0u32);
    let mut bit = 1u32;
    let mut mask_bit = 1u32;

    loop {
        let mut done = true;
        if bit < width {
            x |= mask_bit;
            mask_bit <<= 1;
            done = false;
        }
        if bit < height {
            y |= mask_bit;
            mask_bit <<= 1;
            done = false;
        }
        if bit < depth {
            z |= mask_bit;
            mask_bit <<= 1;
            done = false;
        }
        bit <<= 1;
        if done {
            break;
        }
    }

    (x, y, z)
}

fn swizzled_offset(x: u32, y: u32, z: u32, mask_x: u32, mask_y: u32, mask_z: u32, bpp: u32) -> u32 {
    bpp * (fill_pattern(mask_x, x) | fill_pattern(mask_y, y) | fill_pattern(mask_z, z))
}

/// Scatters `value`'s low bits into the positions marked by `pattern`.
fn fill_pattern(pattern: u32, value: u32) -> u32 {
    let mut result = 0u32;
    let mut bit = 1u32;
    let mut value = value;
    while value != 0 {
        if pattern & bit != 0 {
            result |= if value & 1 != 0 { bit } else { 0 };
            value >>= 1;
        }
        bit <<= 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_pattern_interleaves_low_bits_into_masked_positions() {
        assert_eq!(fill_pattern(0b0101, 0b11), 0b0101);
        assert_eq!(fill_pattern(0b0101, 0b01), 0b0001);
        assert_eq!(fill_pattern(0b0101, 0b10), 0b0100);
    }

    #[test]
    fn generate_swizzle_masks_for_4x4_2d_texture_splits_bits_evenly() {
        let (mx, my, mz) = generate_swizzle_masks(4, 4, 1);
        assert_eq!(mx, 0b0101);
        assert_eq!(my, 0b1010);
        assert_eq!(mz, 0);
    }

    #[test]
    fn decode_r5g6b5_swizzled_2x2_produces_expected_pixel_colors() {
        let px = |r5: u16, g6: u16, b5: u16| -> u16 { (r5 << 11) | (g6 << 5) | b5 };
        let mut pixels = [0u8; 8];
        pixels[0..2].copy_from_slice(&px(31, 0, 0).to_le_bytes()); // pure red
        pixels[2..4].copy_from_slice(&px(0, 63, 0).to_le_bytes()); // pure green
        pixels[4..6].copy_from_slice(&px(0, 0, 31).to_le_bytes()); // pure blue
        pixels[6..8].copy_from_slice(&px(31, 63, 31).to_le_bytes()); // white
        let out = decode_r5g6b5_swizzled(2, 2, &pixels).unwrap();
        assert_eq!(&out[0..4], [248, 0, 0, 255]);
        assert_eq!(&out[4..8], [0, 252, 0, 255]);
        assert_eq!(&out[8..12], [0, 0, 248, 255]);
        assert_eq!(&out[12..16], [248, 252, 248, 255]);
    }

    #[test]
    fn decode_r5g6b5_swizzled_uses_shift_only_not_bit_replication() {
        let px: u16 = 16 << 11; // R5 = 16, G6 = 0, B5 = 0
        let pixels = px.to_le_bytes();
        let out = decode_r5g6b5_swizzled(1, 1, &pixels).unwrap();
        assert_eq!(
            out[0], 128,
            "shift-only expansion expected, not bit-replicated"
        );
    }

    #[test]
    fn decode_a4r4g4b4_swizzled_2x2_produces_expected_pixel_colors() {
        // byte0 = [green_hi:4][blue:4], byte1 = [alpha_hi:4][red:4]
        let px = |a: u8, r: u8, g: u8, b: u8| -> [u8; 2] { [(g << 4) | b, (a << 4) | r] };
        let mut pixels = [0u8; 8];
        pixels[0..2].copy_from_slice(&px(0xF, 0xF, 0x0, 0x0)); // opaque red
        pixels[2..4].copy_from_slice(&px(0xF, 0x0, 0xF, 0x0)); // opaque green
        pixels[4..6].copy_from_slice(&px(0xF, 0x0, 0x0, 0xF)); // opaque blue
        pixels[6..8].copy_from_slice(&px(0x0, 0xF, 0xF, 0xF)); // transparent white
        let out = decode_a4r4g4b4_swizzled(2, 2, &pixels).unwrap();
        assert_eq!(&out[0..4], [0xF0, 0x00, 0x00, 0xF0]);
        assert_eq!(&out[4..8], [0x00, 0xF0, 0x00, 0xF0]);
        assert_eq!(&out[8..12], [0x00, 0x00, 0xF0, 0xF0]);
        assert_eq!(&out[12..16], [0xF0, 0xF0, 0xF0, 0x00]);
    }

    #[test]
    fn decode_rgb_swizzled_ignores_fourth_byte_and_forces_opaque() {
        let pixel = [10u8, 20, 30, 0xAA]; // B,G,R,junk
        let out = decode_rgb_swizzled(1, 1, &pixel).unwrap();
        assert_eq!(out, vec![30, 20, 10, 255]);
    }

    #[test]
    fn decode_rgba_swizzled_uses_abgr_byte_order() {
        let pixel = [200u8, 10, 20, 30]; // A,B,G,R
        let out = decode_rgba_swizzled(1, 1, &pixel).unwrap();
        assert_eq!(out, vec![30, 20, 10, 200]);
    }

    #[test]
    fn decode_argb_swizzled_rejects_huge_dimensions_backed_by_no_data() {
        assert!(decode_argb_swizzled(60917, 21259, &[]).is_none());
    }

    #[test]
    fn unswizzle_bpp_returns_immediately_for_zero_width_with_huge_height() {
        let out = unswizzle_bpp(0, 4_293_525_759, &[], 4);
        assert_eq!(out, Some(Vec::new()));
    }
}
