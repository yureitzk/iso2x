use anyhow::Context;

/// Raw hex representation of the "attach.xbe" taken from Cerbios codebase.
pub(crate) fn attach_xbe() -> Vec<u8> {
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/attach.xbe")).to_vec()
}

fn to_u32_le(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap())
}

fn write_u32_le(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) struct XbeHeader {
    pub(crate) base_address: u32,
    pub(crate) certificate_address: u32,
    pub(crate) number_of_sections: u32,
    pub(crate) section_headers_address: u32,
}

/// Parses the fixed 376-byte XBE header at the start of `buf`.
/// `<https://xboxdevwiki.net/Xbe#XBE_Image_Header>`
pub(crate) fn read_xbe_header(buf: &[u8]) -> Result<XbeHeader, anyhow::Error> {
    anyhow::ensure!(
        buf.len() >= 0x178,
        "buffer too short to contain an XBE header"
    );
    anyhow::ensure!(&buf[0..4] == b"XBEH", "not an Xbox executable (bad magic)");
    Ok(XbeHeader {
        base_address: to_u32_le(buf, 0x0104),
        certificate_address: to_u32_le(buf, 0x0118),
        number_of_sections: to_u32_le(buf, 0x011c),
        section_headers_address: to_u32_le(buf, 0x0120),
    })
}

/// `<https://xboxdevwiki.net/Xbe#Certificate>`
const CERT_SIZE: usize = 464;
const CERT_TITLE_ID_OFFSET: usize = 0x08;
const CERT_TITLE_NAME_OFFSET: usize = 0x0C; // 40 x UTF-16LE code units
const CERT_TITLE_NAME_LEN: usize = 80;
/// `allowed_media_types` (u32 bitfield, see `allowed_media` below).
const CERT_ALLOWED_MEDIA_OFFSET: usize = 0x9C;

pub(crate) mod allowed_media {
    pub(crate) const HARD_DISK: u32 = 0x0000_0001;
    pub(crate) const MEDIA_BOARD: u32 = 0x0000_0200;
    pub(crate) const NONSECURE_HARD_DISK: u32 = 0x4000_0000;
}

fn read_xbe_certificate(buf: &[u8], header: &XbeHeader) -> Result<[u8; CERT_SIZE], anyhow::Error> {
    let addr = header
        .certificate_address
        .checked_sub(header.base_address)
        .context("certificate_address is before base_address")? as usize;
    let slice = buf
        .get(addr..addr + CERT_SIZE)
        .context("XBE certificate address is out of bounds")?;
    let mut cert = [0u8; CERT_SIZE];
    cert.copy_from_slice(slice);
    Ok(cert)
}

struct XbeSection {
    name: String,
    raw_address: u32,
    raw_size: u32,
}

fn read_xbe_section_name(buf: &[u8], address: usize) -> Result<String, anyhow::Error> {
    let slice = buf
        .get(address..address + 20)
        .context("XBE section-name address is out of bounds")?;
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    Ok(String::from_utf8_lossy(&slice[..end]).into_owned())
}

fn read_xbe_section(
    buf: &[u8],
    header: &XbeHeader,
    index: u32,
) -> Result<XbeSection, anyhow::Error> {
    const SECTION_HEADER_SIZE: usize = 56;
    let address = (header.section_headers_address - header.base_address) as usize
        + (index as usize * SECTION_HEADER_SIZE);
    let raw = buf
        .get(address..address + SECTION_HEADER_SIZE)
        .context("XBE section header is out of bounds")?;
    let section_name_address = to_u32_le(raw, 0x0014);
    Ok(XbeSection {
        name: read_xbe_section_name(buf, (section_name_address - header.base_address) as usize)?,
        raw_address: to_u32_le(raw, 0x000c),
        raw_size: to_u32_le(raw, 0x0010),
    })
}

/// Locates the `$$XTIMAGE` section (the title's dashboard thumbnail).
/// `None` is not an error - plenty of real XBEs have no image section.
fn read_xtimage_bytes(
    buf: &[u8],
    header: &XbeHeader,
) -> Result<Option<(XbeSection, Vec<u8>)>, anyhow::Error> {
    for i in 0..header.number_of_sections {
        let section = read_xbe_section(buf, header, i)?;
        if section.name == "$$XTIMAGE" {
            let start = section.raw_address as usize;
            let end = start + section.raw_size as usize;
            let data = buf
                .get(start..end)
                .context("XTIMAGE section is out of bounds")?
                .to_vec();
            return Ok(Some((section, data)));
        }
    }
    Ok(None)
}

fn write_image_section(
    out: &mut [u8],
    section: &XbeSection,
    image_bytes: &[u8],
) -> Result<(), anyhow::Error> {
    anyhow::ensure!(
        out.len() >= 1068,
        "attach stub is too short to patch an image section"
    );
    let image_address = to_u32_le(out, 1060);
    let base_size = section.raw_size + image_address;
    write_u32_le(out, 268, base_size);
    write_u32_le(out, 1056, section.raw_size);
    write_u32_le(out, 1064, section.raw_size);
    let dest = out
        .get_mut(image_address as usize..image_address as usize + image_bytes.len())
        .context("attach stub's image slot is too small for this title's thumbnail")?;
    dest.copy_from_slice(image_bytes);
    Ok(())
}

pub(crate) fn build_attach_xbe(source_xbe_bytes: &[u8]) -> Result<Vec<u8>, anyhow::Error> {
    let mut out = attach_xbe();
    let stub_header = read_xbe_header(&out)?;
    let mut stub_cert = read_xbe_certificate(&out, &stub_header)?;

    let source_header = read_xbe_header(source_xbe_bytes)?;
    let source_cert = read_xbe_certificate(source_xbe_bytes, &source_header)?;

    stub_cert[CERT_TITLE_NAME_OFFSET..CERT_TITLE_NAME_OFFSET + CERT_TITLE_NAME_LEN]
        .copy_from_slice(
            &source_cert[CERT_TITLE_NAME_OFFSET..CERT_TITLE_NAME_OFFSET + CERT_TITLE_NAME_LEN],
        );
    stub_cert[CERT_TITLE_ID_OFFSET..CERT_TITLE_ID_OFFSET + 4]
        .copy_from_slice(&source_cert[CERT_TITLE_ID_OFFSET..CERT_TITLE_ID_OFFSET + 4]);

    let cert_addr = (stub_header.certificate_address - stub_header.base_address) as usize;
    out[cert_addr..cert_addr + CERT_SIZE].copy_from_slice(&stub_cert);

    if let Some((section, image_bytes)) = read_xtimage_bytes(source_xbe_bytes, &source_header)? {
        write_image_section(&mut out, &section, &image_bytes)?;
    }

    Ok(out)
}

/// Patches `allowed_media_types` and/or `title_name` directly in a
/// complete `default.xbe` buffer's certificate, in place.
///
/// `buf` must be a whole, valid XBE file (as produced by extraction, not
/// a partial/streamed chunk), since this needs to read the header to
/// locate the certificate before it can patch it.
///
/// `title_utf16le` is raw UTF-16LE bytes for the new title - truncated
/// (or zero-padded, if shorter) to `CERT_TITLE_NAME_LEN` (80 bytes / 40
/// code units), same as the cert field's fixed width. Pass `None` to
/// leave the title untouched even if `patch_allowed_media` is set.
pub(crate) fn patch_xbe_cert_in_place(
    buf: &mut [u8],
    patch_allowed_media: bool,
    title_utf16le: Option<&[u8]>,
) -> Result<(), anyhow::Error> {
    let header = read_xbe_header(buf)?;
    let cert_addr = header
        .certificate_address
        .checked_sub(header.base_address)
        .context("certificate_address is before base_address")? as usize;
    anyhow::ensure!(
        buf.len() >= cert_addr + CERT_SIZE,
        "XBE certificate is out of bounds"
    );

    if patch_allowed_media {
        let off = cert_addr + CERT_ALLOWED_MEDIA_OFFSET;
        let current = to_u32_le(buf, off);
        let patched = current
            | allowed_media::HARD_DISK
            | allowed_media::NONSECURE_HARD_DISK
            | allowed_media::MEDIA_BOARD;
        write_u32_le(buf, off, patched);
    }

    if let Some(name) = title_utf16le {
        let off = cert_addr + CERT_TITLE_NAME_OFFSET;
        let n = name.len().min(CERT_TITLE_NAME_LEN);
        buf[off..off + CERT_TITLE_NAME_LEN].fill(0);
        buf[off..off + n].copy_from_slice(&name[..n]);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_xbe(cert_addr_from_base: u32) -> Vec<u8> {
        let cert_addr = cert_addr_from_base as usize;
        let mut buf = vec![0u8; cert_addr + CERT_SIZE];
        buf[0..4].copy_from_slice(b"XBEH");
        write_u32_le(&mut buf, 0x0104, 0);
        write_u32_le(&mut buf, 0x0118, cert_addr_from_base);
        write_u32_le(&mut buf, 0x011c, 0);
        write_u32_le(&mut buf, 0x0120, 0);
        buf
    }

    #[test]
    fn patch_allowed_media_ors_in_expected_bits() {
        let mut buf = fake_xbe(0x200);
        let off = 0x200 + CERT_ALLOWED_MEDIA_OFFSET;
        write_u32_le(&mut buf, off, 0x0000_0004); // pre-existing DVD_CD flag

        patch_xbe_cert_in_place(&mut buf, true, None).unwrap();

        let patched = to_u32_le(&buf, off);
        assert_eq!(
            patched,
            0x0000_0004
                | allowed_media::HARD_DISK
                | allowed_media::NONSECURE_HARD_DISK
                | allowed_media::MEDIA_BOARD
        );
    }

    #[test]
    fn patch_title_name_truncates_to_field_width() {
        let mut buf = fake_xbe(0x200);
        let off = 0x200 + CERT_TITLE_NAME_OFFSET;
        for b in &mut buf[off..off + CERT_TITLE_NAME_LEN] {
            *b = 0xAA; // pre-fill so zero-padding below is actually exercised
        }

        let long_name: Vec<u8> = std::iter::repeat_n(0x41u8, 200).collect();
        patch_xbe_cert_in_place(&mut buf, false, Some(&long_name)).unwrap();

        let name_bytes = &buf[off..off + CERT_TITLE_NAME_LEN];
        assert!(name_bytes.iter().all(|&b| b == 0x41));
    }

    #[test]
    fn patch_title_name_shorter_than_field_zero_pads_remainder() {
        let mut buf = fake_xbe(0x200);
        let off = 0x200 + CERT_TITLE_NAME_OFFSET;
        for b in &mut buf[off..off + CERT_TITLE_NAME_LEN] {
            *b = 0xAA;
        }

        let short_name = [0x54u8, 0x00, 0x65, 0x00]; // "Te" in UTF-16LE
        patch_xbe_cert_in_place(&mut buf, false, Some(&short_name)).unwrap();

        let name_bytes = &buf[off..off + CERT_TITLE_NAME_LEN];
        assert_eq!(&name_bytes[..4], &short_name[..]);
        assert!(name_bytes[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn no_patch_options_leaves_cert_untouched() {
        let mut buf = fake_xbe(0x200);
        let before = buf.clone();
        patch_xbe_cert_in_place(&mut buf, false, None).unwrap();
        assert_eq!(buf, before);
    }

    #[test]
    fn rejects_undersized_buffer() {
        let mut buf = fake_xbe(0x200);
        buf.truncate(0x200 + CERT_SIZE - 1);
        assert!(patch_xbe_cert_in_place(&mut buf, true, None).is_err());
    }

    #[test]
    fn build_attach_xbe_output_starts_with_xbe_magic_and_parses() {
        let source = fake_xbe(0x200);
        let out = build_attach_xbe(&source).unwrap();
        assert_eq!(&out[0..4], b"XBEH");
        assert!(read_xbe_header(&out).is_ok());
    }

    #[test]
    fn build_attach_xbe_copies_title_id_and_title_name_from_source() {
        let mut source = fake_xbe(0x200);
        let cert_off = 0x200;
        write_u32_le(&mut source, cert_off + CERT_TITLE_ID_OFFSET, 0x4156_0001);
        let name = [0x54u8, 0x00, 0x65, 0x00, 0x73, 0x00, 0x74, 0x00]; // "Test" UTF-16LE
        source[cert_off + CERT_TITLE_NAME_OFFSET..cert_off + CERT_TITLE_NAME_OFFSET + name.len()]
            .copy_from_slice(&name);

        let out = build_attach_xbe(&source).unwrap();
        let out_header = read_xbe_header(&out).unwrap();
        let out_cert_addr = (out_header.certificate_address - out_header.base_address) as usize;

        assert_eq!(
            to_u32_le(&out, out_cert_addr + CERT_TITLE_ID_OFFSET),
            0x4156_0001
        );
        assert_eq!(
            &out[out_cert_addr + CERT_TITLE_NAME_OFFSET
                ..out_cert_addr + CERT_TITLE_NAME_OFFSET + name.len()],
            &name[..]
        );
    }

    #[test]
    fn build_attach_xbe_leaves_stub_allowed_media_types_untouched_by_the_source() {
        let mut source_a = fake_xbe(0x200);
        let mut source_b = fake_xbe(0x200);
        write_u32_le(
            &mut source_a,
            0x200 + CERT_ALLOWED_MEDIA_OFFSET,
            0x0000_0004,
        );
        write_u32_le(
            &mut source_b,
            0x200 + CERT_ALLOWED_MEDIA_OFFSET,
            0xffff_ffff,
        );

        let out_a = build_attach_xbe(&source_a).unwrap();
        let out_b = build_attach_xbe(&source_b).unwrap();
        assert_eq!(out_a, out_b);
    }

    #[test]
    fn build_attach_xbe_is_deterministic_for_the_same_source() {
        let source = fake_xbe(0x200);
        let out1 = build_attach_xbe(&source).unwrap();
        let out2 = build_attach_xbe(&source).unwrap();
        assert_eq!(out1, out2);
    }

    #[test]
    fn build_attach_xbe_rejects_source_with_bad_magic() {
        let mut source = fake_xbe(0x200);
        source[0..4].copy_from_slice(b"NOPE");
        assert!(build_attach_xbe(&source).is_err());
    }

    #[test]
    fn build_attach_xbe_rejects_undersized_source() {
        let mut source = fake_xbe(0x200);
        source.truncate(0x200 + CERT_SIZE - 1);
        assert!(build_attach_xbe(&source).is_err());
    }

    #[test]
    fn write_image_section_writes_bytes_at_the_stub_image_address_and_updates_sizes() {
        let mut out = attach_xbe();
        let image_address = to_u32_le(&out, 1060);
        let section = XbeSection {
            name: "$$XTIMAGE".to_string(),
            raw_address: 0, // not read by write_image_section
            raw_size: 4,
        };
        let image_bytes = [0xDEu8, 0xAD, 0xBE, 0xEF];

        write_image_section(&mut out, &section, &image_bytes).unwrap();

        assert_eq!(
            &out[image_address as usize..image_address as usize + 4],
            &image_bytes[..]
        );
        assert_eq!(to_u32_le(&out, 1056), 4);
        assert_eq!(to_u32_le(&out, 1064), 4);
    }

    #[test]
    fn write_image_section_rejects_a_stub_buffer_that_is_too_short() {
        let mut out = vec![0u8; 100]; // well under the 1068-byte minimum
        let section = XbeSection {
            name: "$$XTIMAGE".to_string(),
            raw_address: 0,
            raw_size: 4,
        };
        assert!(write_image_section(&mut out, &section, &[0xDE, 0xAD, 0xBE, 0xEF]).is_err());
    }
}
