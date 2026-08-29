use crate::core::executable::TitleExecutionInfo;
use crate::core::signing::{ConsoleSigningKey, sign_pkcs1_sha1};
use crate::core::title::ContentType;
use byteorder::{BE, ByteOrder, LE};
use sha1::{Digest, Sha1};

/// License table + header digest region signed for STFS/GoD headers.
/// `<https://free60.org/System-Software/Formats/STFS>`
const SIGNED_REGION: std::ops::Range<usize> = 0x022c..0x0344;

const EMPTY_LIVE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/empty_live.bin"
));

pub struct ConHeaderBuilder {
    buffer: Vec<u8>,
}

impl Default for ConHeaderBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ConHeaderBuilder {
    pub fn new() -> Self {
        Self {
            buffer: Vec::from(EMPTY_LIVE),
        }
    }

    /// Buffer must be >= `0x344` bytes.
    pub(crate) fn from_buffer(buffer: Vec<u8>) -> Self {
        Self { buffer }
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.buffer[offset] = value;
    }

    fn write_u16_be(&mut self, offset: usize, value: u16) {
        BE::write_u16(&mut self.buffer[offset..], value);
    }

    fn write_u24_be(&mut self, offset: usize, value: u32) {
        BE::write_u24(&mut self.buffer[offset..], value);
    }

    fn write_u32_be(&mut self, offset: usize, value: u32) {
        BE::write_u32(&mut self.buffer[offset..], value);
    }

    fn write_u32_le(&mut self, offset: usize, value: u32) {
        LE::write_u32(&mut self.buffer[offset..], value);
    }

    fn write_bytes(&mut self, offset: usize, buf: &[u8]) {
        self.buffer[offset..offset + buf.len()].copy_from_slice(buf);
    }

    fn write_utf16_be(&mut self, offset: usize, s: &str) {
        for (i, c) in s.encode_utf16().chain([0]).enumerate() {
            self.write_u16_be(offset + i * 2, c);
        }
    }

    /// Escape hatch for fields with no dedicated setter (STFS volume
    /// descriptor, file-table block count/num, topHashTableHash).
    pub(crate) fn with_raw_bytes(mut self, offset: usize, buf: &[u8]) -> Self {
        self.write_bytes(offset, buf);
        self
    }

    /// `GoD` header offsets (not the standard STFS volume descriptor).
    pub fn with_block_counts(mut self, blocks_allocated: u32, blocks_not_allocated: u16) -> Self {
        self.write_u24_be(0x0392, blocks_allocated);
        self.write_u16_be(0x0395, blocks_not_allocated);
        self
    }

    pub fn with_content_type(mut self, content_type: ContentType) -> Self {
        self.write_u32_be(0x0344, content_type as u32);
        self
    }

    /// `GoD` header offsets. `part_count` is little-endian (unlike the rest).
    pub fn with_data_parts_info(mut self, part_count: u32, parts_total_size: u64) -> Self {
        self.write_u32_le(0x03a0, part_count); // sic!
        self.write_u32_be(
            0x03a4,
            u32::try_from(parts_total_size / 0x0100)
                .expect("data parts total size exceeds u32 range"),
        );
        self
    }

    /// Writes the full `0x354..0x36C` execution-info block, including
    /// `SaveGameID` at `0x368`. Prefer this over `with_save_game_id` when a
    /// `TitleExecutionInfo` is on hand.
    pub fn with_execution_info(mut self, exe_info: &TitleExecutionInfo) -> Self {
        self.write_u32_be(0x0354, exe_info.media_id);
        self.write_u32_be(0x0360, exe_info.title_id);
        self.write_u8(0x0364, exe_info.platform);
        self.write_u8(0x0365, exe_info.executable_type);
        self.write_u8(0x0366, exe_info.disc_number);
        self.write_u8(0x0367, exe_info.disc_count);
        self.write_u32_be(0x0368, exe_info.save_game_id);
        self
    }

    pub fn with_game_icon(mut self, png_bytes: Option<&[u8]>) -> Self {
        let png_bytes = png_bytes.unwrap_or(&[]);
        assert!(png_bytes.len() <= 0x0400);
        let png_len = u32::try_from(png_bytes.len()).expect("checked <= 0x0400 above");
        self.write_u32_be(0x1712, png_len);
        self.write_u32_be(0x1716, png_len);
        self.write_bytes(0x171a, png_bytes);
        self.write_bytes(0x571a, png_bytes);
        self
    }

    pub fn with_game_title(mut self, game_title: &str) -> Self {
        self.write_utf16_be(0x0411, game_title);
        self.write_utf16_be(0x1691, game_title);
        self
    }

    /// 8 bytes at `0x03ad`. Unknown/unconfirmed semantics;
    /// treat as an opaque round-trippable field. Zeroed by default.
    pub fn with_online_creator(mut self, online_creator: [u8; 8]) -> Self {
        self.write_bytes(0x03ad, &online_creator);
        self
    }

    /// Save Game ID at `0x0368` (4 bytes). Usually `0` outside savegame
    /// content; not enforced here. Prefer `with_execution_info` when a
    /// full `TitleExecutionInfo` is on hand.
    pub fn with_save_game_id(mut self, save_game_id: u32) -> Self {
        self.write_u32_be(0x0368, save_game_id);
        self
    }

    /// `GoD` header offset (differs from STFS Top Hash Table Hash).
    pub fn with_mht_hash(mut self, mht_hash: &[u8; 20]) -> Self {
        self.write_bytes(0x037d, mht_hash);
        self
    }

    /// Device ID at `0x03fd` (20 bytes; same in STFS and `GoD`). Zeroed by
    /// default. Usually paired with the Device ID Transfer flag (bit 6 at
    /// `0x1711`) on console-signed packages; not enforced here.
    pub fn with_device_id(mut self, device_id: &[u8; 20]) -> Self {
        self.write_bytes(0x03fd, device_id);
        self
    }

    /// Console ID at `0x036c` (5 bytes), usually matching the cert's
    /// Owner Console ID at `0x006`. Zeroed by default; not enforced.
    pub fn with_console_id(mut self, console_id: [u8; 5]) -> Self {
        self.write_bytes(0x036c, &console_id);
        self
    }

    /// Profile ID (XUID) at `0x0371` (8 bytes). Display-only; real
    /// ownership needs a matching license entry (see
    /// `with_additional_license_entry` / `finalize_signed`).
    pub fn with_profile_id(mut self, profile_id: [u8; 8]) -> Self {
        self.write_bytes(0x0371, &profile_id);
        self
    }

    /// Extra license-table entry (`entry_index` 1..16; 0 is reserved for
    /// the console-bound entry `finalize_signed` writes). Each entry:
    /// 8-byte License ID, 4-byte Bits, 4-byte Flags. Call before
    /// `finalize_signed`.
    ///
    /// # Panics
    ///
    /// If `entry_index` is `0` or `>= 16`.
    pub fn with_additional_license_entry(
        mut self,
        entry_index: usize,
        license_id: [u8; 8],
        bits: u32,
        flags: u32,
    ) -> Self {
        assert!(
            (1..16).contains(&entry_index),
            "license entry index must be in 1..16 - index 0 is reserved for the \
             console entry finalize_signed always writes"
        );
        let off = 0x022c + entry_index * 0x10;
        self.write_bytes(off, &license_id);
        self.write_u32_be(off + 8, bits);
        self.write_u32_be(off + 0xc, flags);
        self
    }

    /// Digest over `[0x344, buffer.len())`.
    fn write_digest(&mut self) {
        self.buffer[0x035b] = 0;
        self.buffer[0x035f] = 0;
        self.buffer[0x0391] = 0;
        let digest: [u8; 20] = Sha1::digest(&self.buffer[0x0344..]).into();
        self.write_bytes(0x032c, &digest);
    }

    /// Unsigned finalize: `'LIVE'` magic, digest only.
    pub fn finalize(mut self) -> Vec<u8> {
        self.write_digest();
        self.buffer
    }

    /// Console-signed finalize: `'CON '` magic, cert, license table
    /// (entry 0 = console-bound `0xF0`), and RSA-PKCS1v1.5/SHA1 over
    /// `SIGNED_REGION`.
    ///
    /// # Errors
    ///
    /// If signing fails (see `sign_pkcs1_sha1`).
    pub fn finalize_signed(mut self, key: &ConsoleSigningKey) -> Result<Vec<u8>, anyhow::Error> {
        self.write_u32_be(0x0000, 0x434F_4E20); // 'CON '
        self.write_bytes(0x0004, &key.certificate);
        self.write_digest();
        self.write_u8(0x022c, 0xF0);
        self.write_bytes(0x022c + 3, &key.certificate[2..7]);
        let to_sign = self.buffer[SIGNED_REGION].to_vec();
        let signature = sign_pkcs1_sha1(key, &to_sign)?;
        self.write_bytes(0x01ac, &signature);
        Ok(self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::RsaPrivateKey;
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;

    fn synthetic_key() -> (ConsoleSigningKey, RsaPrivateKey) {
        let mut rng = rand::thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 1024).expect("keygen");
        let certificate: [u8; 0x1A8] = std::array::from_fn(|i| i as u8);
        let key = ConsoleSigningKey {
            private_key: private_key.clone(),
            certificate,
        };
        (key, private_key)
    }

    #[test]
    fn finalize_writes_a_digest_matching_the_post_header_region() {
        let buf = ConHeaderBuilder::new().finalize();
        let expected: [u8; 20] = Sha1::digest(&buf[0x0344..]).into();
        assert_eq!(&buf[0x032c..0x032c + 20], &expected[..]);
    }

    #[test]
    fn finalize_signed_writes_con_magic_and_certificate() {
        let (key, _) = synthetic_key();
        let buf = ConHeaderBuilder::new()
            .finalize_signed(&key)
            .expect("signing should succeed");
        assert_eq!(&buf[0x0000..0x0004], b"CON ");
        assert_eq!(&buf[0x0004..0x0004 + 0x1A8], &key.certificate[..]);
    }

    #[test]
    fn finalize_signed_writes_console_bound_license_entry_at_index_zero() {
        let (key, _) = synthetic_key();
        let buf = ConHeaderBuilder::new()
            .finalize_signed(&key)
            .expect("signing should succeed");
        assert_eq!(buf[0x022c], 0xF0);
        assert_eq!(&buf[0x022c + 3..0x022c + 8], &key.certificate[2..7]);
    }

    #[test]
    fn finalize_signed_embeds_a_signature_that_verifies_over_signed_region() {
        let (key, original) = synthetic_key();
        let buf = ConHeaderBuilder::new()
            .with_content_type(ContentType::GamesOnDemand)
            .finalize_signed(&key)
            .expect("signing should succeed");

        let wire_sig: [u8; 0x80] = buf[0x01ac..0x01ac + 0x80]
            .try_into()
            .expect("signature field is 0x80 bytes");
        let standard_sig_bytes = crate::core::signing::keyvault::signature_to_wire_format(wire_sig);
        let signature = Signature::try_from(standard_sig_bytes.as_slice())
            .expect("should decode as a pkcs1v15 signature");

        let verifying_key = VerifyingKey::<Sha1>::new(original.to_public_key());
        verifying_key
            .verify(&buf[SIGNED_REGION], &signature)
            .expect("embedded signature should verify over SIGNED_REGION of the finished buffer");
    }

    #[test]
    #[should_panic(expected = "license entry index must be in 1..16")]
    fn additional_license_entry_rejects_index_zero() {
        let _ = ConHeaderBuilder::new().with_additional_license_entry(0, [0; 8], 0, 0);
    }

    #[test]
    #[should_panic(expected = "license entry index must be in 1..16")]
    fn additional_license_entry_rejects_index_sixteen() {
        let _ = ConHeaderBuilder::new().with_additional_license_entry(16, [0; 8], 0, 0);
    }

    #[test]
    fn data_parts_info_writes_part_count_little_endian_and_size_big_endian() {
        let buf = ConHeaderBuilder::new()
            .with_data_parts_info(0x0000_0002, 0x0000_0100)
            .finalize();
        assert_eq!(&buf[0x03a0..0x03a4], &[0x02, 0x00, 0x00, 0x00]);
        assert_eq!(&buf[0x03a4..0x03a8], &[0x00, 0x00, 0x00, 0x01]);
    }
}
