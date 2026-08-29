use anyhow::{Context, Result, bail};
use byteorder::{BE, ByteOrder, ReadBytesExt};
use lzxd::{Lzxd, WindowSize};
use std::io::{Cursor, Read, Seek, SeekFrom};

const MAX_XEX_IMAGE_SIZE: usize = 512 * 1024 * 1024;

const XEX_MAGIC: [u8; 4] = *b"XEX2";

// Optional header field IDs.
// <https://free60.org/System-Software/Formats/XEX/#header-ids>

/// Points to the launch title's resource table (XDBF blob(s) among them).
pub(crate) const FIELD_RESOURCE_INFO: u32 = 0x0000_02FF;
/// The image's encryption/compression descriptor.
pub(crate) const FIELD_BASE_FILE_FORMAT: u32 = 0x0000_03FF;
/// free60's "Load Address" field. Despite the name, this is *not* what
/// Xenia uses to resolve resource-table VAs - kept here only for
/// reference. See `FIELD_IMAGE_BASE_ADDRESS` below.
#[allow(dead_code)]
pub(crate) const FIELD_ORIGINAL_BASE_ADDRESS: u32 = 0x0001_0001;
/// `XEX_HEADER_IMAGE_BASE_ADDRESS` (free60's "Base Address"). When
/// present, overrides `SecurityInfo.load_address` as the VA
/// resource-table entries are relative to (see `parse_xex_layout`'s
/// `load_address` handling below). Stored inline, unlike
/// `ResourceInfo`/`BaseFileFormat` which are header-relative offsets.
pub(crate) const FIELD_IMAGE_BASE_ADDRESS: u32 = 0x0001_0201;

/// `XEX_HEADER_EXECUTION_ID` (free60's "Execution ID"). Header-relative
/// offset to a `TitleExecutionInfo`-shaped block - see
/// `super::TitleExecutionInfo::from_xex`. Here we only need `title_id`
/// (the 4th field), to match the game-data XDBF blob's resource-table
/// entry by name - see `read_title_id` below.
pub(crate) const FIELD_EXECUTION_INFO: u32 = 0x0004_0006;

/// Sanity cap on the optional header's field count, so a corrupt
/// `field_count` doesn't cause an enormous read loop.
const MAX_XEX_HEADER_FIELDS: u32 = 4096;

/// Offset within the `SecurityInfo` header where the encrypted session key
/// is stored.
const SECURITY_INFO_SESSION_KEY_OFFSET: u64 = 0x150;

#[derive(Debug)]
pub(crate) enum CompressionKind {
    None,
    /// `(data_size, zero_size)` runs: copy `data_size` bytes from the
    /// compressed stream, then emit `zero_size` zero bytes, repeat.
    Basic(Vec<(u32, u32)>),
    /// LZX ("normal"). `window_size` is the raw header value (e.g.
    /// `0x8000` for a 32 KiB window); `first_block_size` is the size in
    /// bytes of the first compressed block at `code_offset`.
    Normal {
        window_size: u32,
        first_block_size: u32,
    },
    /// Unrecognized `compression_type` value. Kept only for the derived
    /// `Debug` impl (diagnostics) - not read anywhere else.
    Unsupported(#[allow(dead_code)] u16),
}

pub(crate) struct XexLayout {
    /// File offset the XEX header itself starts at - all optional
    /// header field offsets are relative to this, not to 0.
    pub(crate) header_offset: u64,
    /// File offset where the (possibly compressed) image body starts.
    pub(crate) code_offset: u32,
    /// Header-relative offset to the resource table, if present.
    pub(crate) resource_info_offset: Option<u32>,
    pub(crate) encrypted: bool,
    /// The encrypted session key from the security info header. Needed
    /// to decrypt the body if `encrypted` is true.
    pub(crate) encrypted_session_key: Option<[u8; 16]>,
    pub(crate) compression: CompressionKind,
    /// VA of the decompressed image's first byte.
    pub(crate) load_address: Option<u32>,
    /// The launch title's title ID, from the `ExecutionInfo` optional
    /// header block, if present. Used by [`crate::core::thumbnail`] to
    /// pick the resource-table entry matching this title (formatted as
    /// 8 uppercase hex digits) instead of trying every entry.
    pub(crate) title_id: Option<u32>,
}

pub(crate) fn parse_xex_layout<R: Read + Seek>(mut r: R) -> Result<XexLayout> {
    let header_offset = r.stream_position()?;

    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if magic != XEX_MAGIC {
        bail!("missing 'XEX2' magic bytes in XEX header");
    }

    let _module_flags = r.read_u32::<BE>()?;
    let code_offset = r.read_u32::<BE>()?;
    let _reserved = r.read_u32::<BE>()?;
    let security_info_offset = r.read_u32::<BE>()?;
    let field_count = r.read_u32::<BE>()?.min(MAX_XEX_HEADER_FIELDS);

    let mut resource_info_offset = None;
    let mut file_format_offset = None;
    let mut image_base_address_override = None;
    let mut execution_info_offset = None;

    for _ in 0..field_count {
        let key = r.read_u32::<BE>()?;
        let value = r.read_u32::<BE>()?;

        match key {
            FIELD_RESOURCE_INFO => resource_info_offset = Some(value),
            FIELD_BASE_FILE_FORMAT => file_format_offset = Some(value),
            FIELD_IMAGE_BASE_ADDRESS => image_base_address_override = Some(value),
            FIELD_EXECUTION_INFO => execution_info_offset = Some(value),
            _ => {}
        }
    }

    // Default: SecurityInfo's LoadAddress (Xenia: xex_security_info()->
    // load_address, at +0x110 within the security info header - always
    // present, since SecurityInfo itself is mandatory).
    let mut load_address = None;
    if security_info_offset != 0 {
        let la_off = header_offset + u64::from(security_info_offset) + 0x110;
        if r.seek(SeekFrom::Start(la_off)).is_ok()
            && let Ok(la) = r.read_u32::<BE>()
        {
            load_address = Some(la);
        }
    }
    // Override with FIELD_IMAGE_BASE_ADDRESS if present - matches Xenia's
    // XexModule::Load() exactly (SecurityInfo default, optional-header
    // override, never the other way around).
    if let Some(value) = image_base_address_override {
        load_address = Some(value);
    }

    let (encrypted, compression) = if let Some(off) = file_format_offset {
        parse_file_format_info(&mut r, header_offset, off)?
    } else {
        (false, CompressionKind::None)
    };

    let encrypted_session_key = if encrypted && security_info_offset != 0 {
        read_session_key(&mut r, header_offset, security_info_offset)?
    } else {
        None
    };

    // Best-effort: a missing or malformed ExecutionInfo block just means
    // the resource-table lookup falls back to trying every entry - not
    // fatal to layout parsing as a whole.
    let title_id =
        execution_info_offset.and_then(|off| read_title_id(&mut r, header_offset, off).ok());

    Ok(XexLayout {
        header_offset,
        code_offset,
        resource_info_offset,
        encrypted,
        encrypted_session_key,
        compression,
        load_address,
        title_id,
    })
}

/// Reads just the `title_id` field out of the `ExecutionInfo` optional
/// header block. The block's layout (per `super::TitleExecutionInfo::
/// from_xex`) is `media_id(u32) + version(u32) + base_version(u32) +
/// title_id(u32) + ...`, so `title_id` sits 12 bytes into the block.
fn read_title_id<R: Read + Seek>(r: &mut R, header_offset: u64, field_value: u32) -> Result<u32> {
    r.seek(SeekFrom::Start(header_offset + u64::from(field_value) + 12))?;
    Ok(r.read_u32::<BE>()?)
}

/// Reads the encrypted session key from the `SecurityInfo` header (see
/// `SECURITY_INFO_SESSION_KEY_OFFSET`).
fn read_session_key<R: Read + Seek>(
    r: &mut R,
    header_offset: u64,
    security_info_offset: u32,
) -> Result<Option<[u8; 16]>> {
    let key_offset =
        header_offset + u64::from(security_info_offset) + SECURITY_INFO_SESSION_KEY_OFFSET;
    r.seek(SeekFrom::Start(key_offset))?;
    let mut key = [0u8; 16];
    r.read_exact(&mut key)?;
    Ok(Some(key))
}

/// Reads the `BaseFileFormat` block: `size(u32) + encryption_type(u16) +
/// compression_type(u16)`, followed by compression-specific data filling
/// the rest of `size`.
fn parse_file_format_info<R: Read + Seek>(
    r: &mut R,
    header_offset: u64,
    field_value: u32,
) -> Result<(bool, CompressionKind)> {
    r.seek(SeekFrom::Start(header_offset + u64::from(field_value)))?;
    let block_size = r.read_u32::<BE>()?;
    let encryption_type = r.read_u16::<BE>()?;
    let compression_type = r.read_u16::<BE>()?;
    let remaining = block_size.saturating_sub(8) as usize;

    let compression = match compression_type {
        0 => CompressionKind::None,
        1 => {
            let mut pairs = Vec::new();
            let mut consumed = 0usize;
            // A run-list this long implies a corrupt header long before
            // it implies a real (if enormous) title.
            while consumed + 8 <= remaining && pairs.len() < 1_000_000 {
                let data_size = r.read_u32::<BE>()?;
                let zero_size = r.read_u32::<BE>()?;
                consumed += 8;
                if data_size == 0 && zero_size == 0 {
                    break;
                }
                pairs.push((data_size, zero_size));
            }
            CompressionKind::Basic(pairs)
        }
        2 => {
            if remaining < 8 {
                bail!("XEX BaseFileFormat block too short for LZX ('normal') header");
            }
            let window_size = r.read_u32::<BE>()?;
            let first_block_size = r.read_u32::<BE>()?;
            // A 20-byte SHA1 hash of the first block follows - only
            // needed to verify integrity, not to decompress, so it's
            // left unread.
            CompressionKind::Normal {
                window_size,
                first_block_size,
            }
        }
        other => CompressionKind::Unsupported(other),
    };

    Ok((encryption_type != 0, compression))
}

/// Reads the raw (possibly encrypted) body bytes from the XEX.
/// The caller is responsible for decryption before decompression.
pub(crate) fn read_raw_body(xex_bytes: &[u8], code_offset: u32) -> Result<Vec<u8>> {
    xex_bytes
        .get(code_offset as usize..)
        .map(<[u8]>::to_vec)
        .context("XEX code offset out of bounds")
}

/// `target_len`, when given, lets a caller that only needs a prefix of
/// the decompressed image (see [`crate::core::thumbnail`]) stop early
/// instead of paying for the full image - worth doing especially in
/// wasm, where an instance's linear memory never shrinks back down once
/// grown.
pub(crate) fn decompress_none(body: &[u8], target_len: Option<usize>) -> Vec<u8> {
    match target_len {
        Some(len) if len < body.len() => body[..len].to_vec(),
        _ => body.to_vec(),
    }
}

pub(crate) fn decompress_basic(
    body: &[u8],
    runs: &[(u32, u32)],
    target_len: Option<usize>,
) -> Result<Vec<u8>> {
    let mut input = body;
    let mut out = Vec::new();
    // Applied inside each run's resize() below, not just via the loop's
    // break: a single zero-fill run can itself vastly exceed target_len,
    // so the cap has to bound that resize() directly.
    let cap = target_len.map_or(MAX_XEX_IMAGE_SIZE, |len| len.min(MAX_XEX_IMAGE_SIZE));

    for &(data_size, zero_size) in runs {
        if out.len() >= cap {
            break;
        }

        let data_size = data_size as usize;
        let zero_size = zero_size as usize;

        let chunk = input
            .get(..data_size)
            .context("XEX basic-compressed data run truncated")?;
        out.extend_from_slice(chunk);
        out.resize(out.len().saturating_add(zero_size).min(cap), 0);
        input = &input[data_size..];

        if out.len() >= MAX_XEX_IMAGE_SIZE {
            bail!("decompressed XEX image exceeds sanity limit");
        }
    }

    Ok(out)
}

/// Maps a XEX `BaseFileFormat` window-size value to `lzxd`'s window-size
/// enum. Values outside the documented 32 KiB - 2 MiB power-of-two range
/// are treated as unsupported rather than guessed at.
pub(crate) fn window_size_from_xex(value: u32) -> Option<WindowSize> {
    Some(match value {
        0x0000_8000 => WindowSize::KB32,
        0x0001_0000 => WindowSize::KB64,
        0x0002_0000 => WindowSize::KB128,
        0x0004_0000 => WindowSize::KB256,
        0x0008_0000 => WindowSize::KB512,
        0x0010_0000 => WindowSize::MB1,
        0x0020_0000 => WindowSize::MB2,
        _ => return None,
    })
}

const LZXD_CHUNK_SIZE: usize = 0x8000;

/// Decompresses a normal LZX-compressed XEX image body.
///
/// Each block starts with a 24-byte header containing the next block's
/// size (4-byte big-endian) and SHA1 (20 bytes, unused here). The rest
/// contains 2-byte big-endian chunk lengths followed by LZXD data,
/// terminated by a zero-length prefix. A single `Lzxd` instance is shared
/// across the image so its window persists between chunks.
pub(crate) fn decompress_normal(
    body: &[u8],
    window_size: u32,
    first_block_size: u32,
    target_len: Option<usize>,
) -> Result<Vec<u8>> {
    let window = window_size_from_xex(window_size).context("unsupported XEX LZX window size")?;
    let mut lzxd = Lzxd::new(window);
    let mut out = Vec::new();
    let mut block_offset = 0u64;
    let mut block_size = first_block_size;

    'blocks: while block_size != 0 {
        if target_len.is_some_and(|target| out.len() >= target) {
            break;
        }
        if out.len() >= MAX_XEX_IMAGE_SIZE {
            bail!("decompressed XEX image exceeds sanity limit");
        }

        let block_size_usize = block_size as usize;
        if block_size_usize < 24 {
            bail!("XEX compressed block too small to hold a header");
        }

        let block_start = usize::try_from(block_offset).context("XEX block offset out of range")?;
        let block_end = block_start
            .checked_add(block_size_usize)
            .context("XEX block end overflow")?;
        let block = body
            .get(block_start..block_end)
            .context("XEX compressed block out of bounds")?;

        let (header, chunk_data) = block.split_at(24);
        let next_block_size = BE::read_u32(&header[0..4]);
        // header[4..24]: this block's own SHA1 hash, unused here.

        let mut pos = 0usize;
        while pos + 2 <= chunk_data.len() {
            let chunk_len = usize::from(BE::read_u16(&chunk_data[pos..pos + 2]));
            pos += 2;
            if chunk_len == 0 {
                // Zero-length prefix terminates this block's chunk
                // stream - stop, don't keep parsing past it.
                break;
            }
            let chunk = chunk_data
                .get(pos..pos + chunk_len)
                .context("XEX LZX chunk length runs past its block")?;
            pos += chunk_len;

            let decompressed = lzxd
                .decompress_next(chunk, LZXD_CHUNK_SIZE)
                .map_err(|e| anyhow::anyhow!("LZX chunk decompression failed: {e:?}"))?;
            out.extend_from_slice(decompressed);

            if target_len.is_some_and(|target| out.len() >= target) {
                break 'blocks;
            }
        }

        block_offset = block_end as u64;
        block_size = next_block_size;
    }

    Ok(out)
}

/// Reads the `ResourceInfo` block: `size(u32)` followed by
/// `(size - 4) / 16` entries of `{ name[8], address(u32 VA), size(u32) }`.
/// `name` is returned raw, unmatched - see [`crate::core::thumbnail`] for
/// how entries are picked.
pub(crate) fn read_resource_table(
    xex_bytes: &[u8],
    header_offset: u64,
    field_value: u32,
) -> Result<Vec<([u8; 8], u32, u32)>> {
    let mut r = Cursor::new(xex_bytes);
    r.seek(SeekFrom::Start(header_offset + u64::from(field_value)))?;
    let block_size = r.read_u32::<BE>()?;
    let entries_bytes = block_size.saturating_sub(4);
    // Sanity cap: a resource table listing tens of thousands of entries
    // implies a corrupt header, not a real title.
    let entry_count = (entries_bytes / 16).min(65536);

    let mut resources = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        let mut name = [0u8; 8];
        r.read_exact(&mut name)?;
        let address = r.read_u32::<BE>()?;
        let size = r.read_u32::<BE>()?;
        resources.push((name, address, size));
    }

    Ok(resources)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompress_basic_expands_data_and_zero_runs() {
        let body = [b'A', b'A', b'A', b'A', b'B', b'B'];
        let runs = vec![(4, 3), (2, 0)];
        let out = decompress_basic(&body, &runs, None).unwrap();
        assert_eq!(out, b"AAAA\x00\x00\x00BB");
    }

    #[test]
    fn decompress_basic_stops_early_once_target_len_reached() {
        let body = [b'A', b'A', b'A', b'A', b'B', b'B'];
        let runs = vec![(4, 3), (2, 0)];
        // Only the first run is needed to reach 4 bytes.
        let out = decompress_basic(&body, &runs, Some(4)).unwrap();
        assert_eq!(out, b"AAAA");
    }

    #[test]
    fn decompress_basic_caps_a_single_oversized_zero_run_at_target_len() {
        // A long padding run - the realistic case "basic" compression
        // collapses into one pair - must be capped within its own
        // resize(), not just by a check between runs.
        let body = [b'A', b'A'];
        let runs = vec![(2, 10_000_000)];
        let out = decompress_basic(&body, &runs, Some(8)).unwrap();
        assert_eq!(out.len(), 8);
        assert_eq!(&out[..2], b"AA");
        assert!(out[2..].iter().all(|&b| b == 0));
    }

    #[test]
    fn window_size_from_xex_recognizes_known_sizes_and_rejects_others() {
        assert!(window_size_from_xex(0x8000).is_some());
        assert!(window_size_from_xex(0x1234).is_none());
    }

    #[test]
    fn decompress_normal_errors_on_chunk_length_past_block_end() {
        let mut block = vec![0xFFu8, 0xFF];
        block.resize(26, 0);
        assert!(decompress_normal(&block, 0x8000, 26, None).is_err());
    }

    #[test]
    fn read_session_key_reads_from_correct_offset() {
        let security_info_offset = 0x100u32;
        let key_offset = security_info_offset as usize + 0x150;
        let mut buf = vec![0u8; key_offset + 16];
        buf[key_offset..key_offset + 16].copy_from_slice(&[0xAA; 16]);

        let mut cursor = Cursor::new(&buf[..]);
        let header_offset = 0u64;
        let result = read_session_key(&mut cursor, header_offset, security_info_offset).unwrap();
        assert_eq!(result, Some([0xAA; 16]));
    }
}
