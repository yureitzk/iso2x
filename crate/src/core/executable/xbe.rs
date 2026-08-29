use super::TitleExecutionInfo;
use anyhow::Error;
use binrw::BinRead;
use std::io::{Read, Seek, SeekFrom};

pub struct XbeHeader {
    // We only need these fields to get the cert address
    pub dw_base_addr: u32,
    pub dw_certificate_addr: u32,
    pub fields: XbeHeaderFields,
}

#[derive(Clone, Default, Debug)]
pub struct XbeHeaderFields {
    pub execution_info: Option<TitleExecutionInfo>,
}

/// The two XBE header fields this crate needs: `dwBaseAddr` (0x104) and
/// `dwCertificateAddr` (0x118, per <https://xboxdevwiki.net/Xbe>).
#[derive(BinRead, Debug, Clone, Copy)]
#[br(little, magic = b"XBEH")]
struct XbeHeaderWire {
    #[br(pad_before = 256)]
    dw_base_addr: u32,
    #[br(pad_before = 16)]
    dw_certificate_addr: u32,
}

impl XbeHeader {
    pub fn read<R: Read + Seek>(mut reader: R) -> Result<XbeHeader, Error> {
        let wire = XbeHeaderWire::read(&mut reader)
            .map_err(|e| anyhow::anyhow!("missing 'XBEH' magic bytes in XBE header: {e}"))?;

        // Cursor is at magic(4) + 256 + base_addr(4) + 16 +
        // cert_addr(4) = 284 bytes in; `offset` is where that started,
        // i.e. the start of this header (cert_address below is
        // relative to dw_base_addr, which is itself relative to the
        // header start, not the file start).
        let offset = reader.stream_position()? - 284;
        let cert_address = wire.dw_certificate_addr.saturating_sub(wire.dw_base_addr);
        reader.seek(SeekFrom::Start(offset + u64::from(cert_address)))?;

        Ok(XbeHeader {
            dw_base_addr: wire.dw_base_addr,
            dw_certificate_addr: wire.dw_certificate_addr,
            fields: XbeHeaderFields {
                execution_info: Some(TitleExecutionInfo::from_xbe(reader)?),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn valid_xbe_bytes(title_id: u32, version: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"XBEH");
        buf.extend(std::iter::repeat_n(0u8, 256));
        let base_addr: u32 = 0x10000;
        buf.extend_from_slice(&base_addr.to_le_bytes());
        buf.extend(std::iter::repeat_n(0u8, 16));
        let cert_addr: u32 = base_addr + 284;
        buf.extend_from_slice(&cert_addr.to_le_bytes());
        assert_eq!(buf.len(), 284, "magic(4)+256+base_addr(4)+16+cert_addr(4)");

        buf.extend(std::iter::repeat_n(0u8, 8));
        buf.extend_from_slice(&title_id.to_le_bytes());
        buf.extend(std::iter::repeat_n(0u8, 160));
        buf.extend_from_slice(&version.to_le_bytes());
        buf
    }

    #[test]
    fn read_parses_a_valid_minimal_header() {
        let buf = valid_xbe_bytes(0x5849_0001, 1);
        let header = XbeHeader::read(Cursor::new(buf)).expect("should parse");
        let info = header
            .fields
            .execution_info
            .expect("execution info should be present");
        assert_eq!(info.title_id, 0x5849_0001);
        assert_eq!(info.version, 1);
    }

    #[test]
    fn read_rejects_missing_magic() {
        let mut buf = valid_xbe_bytes(1, 1);
        buf[3] = b'X'; // "XBEX" instead of "XBEH"
        assert!(XbeHeader::read(Cursor::new(buf)).is_err());
    }

    #[test]
    fn read_never_panics_on_any_truncation_of_a_valid_header() {
        let base = valid_xbe_bytes(1, 1);
        for len in 0..base.len() {
            let truncated = base[..len].to_vec();
            let result = std::panic::catch_unwind(|| XbeHeader::read(Cursor::new(truncated)));
            assert!(
                result.is_ok(),
                "read() must not panic on a header truncated to {len} bytes"
            );
        }
    }

    #[test]
    fn read_never_panics_on_any_single_byte_corruption_of_a_valid_header() {
        let base = valid_xbe_bytes(1, 1);
        for offset in 0..base.len() {
            let mut mutated = base.clone();
            mutated[offset] ^= 0xFF;
            let result = std::panic::catch_unwind(|| XbeHeader::read(Cursor::new(mutated)));
            assert!(
                result.is_ok(),
                "read() must not panic on a byte flip at offset {offset}"
            );
        }
    }

    #[test]
    fn read_does_not_panic_when_certificate_address_precedes_base_address() {
        let mut buf = valid_xbe_bytes(1, 1);
        buf[280..284].copy_from_slice(&0u32.to_le_bytes()); // dw_certificate_addr
        let result = std::panic::catch_unwind(|| XbeHeader::read(Cursor::new(buf)));
        assert!(result.is_ok());
    }

    #[test]
    fn read_does_not_panic_on_a_certificate_address_far_past_the_buffer_end() {
        let mut buf = valid_xbe_bytes(1, 1);
        buf[280..284].copy_from_slice(&u32::MAX.to_le_bytes()); // dw_certificate_addr
        let result = std::panic::catch_unwind(|| XbeHeader::read(Cursor::new(buf)));
        assert!(result.is_ok());
    }

    #[test]
    #[ignore = "writes fuzz corpus seed files; run explicitly with --ignored"]
    fn write_fuzz_corpus_seed_for_xbe_header() {
        let bytes = valid_xbe_bytes(0x5849_0001, 1);
        let dir = "fuzz/corpus/xbe_header";
        std::fs::create_dir_all(dir).expect("corpus directory should be creatable");
        std::fs::write(format!("{dir}/seed-minimal-xbe"), &bytes)
            .expect("seed file should be writable");
    }
}
