use super::obfuscate::{obfuscate, unobfuscate};
use anyhow::{Context, ensure};
use binrw::{BinRead, BinWrite};
use byteorder::{BE, ByteOrder};
use std::io::Cursor;

/// Plaintext record size (bytes). On disk it's wrapped as
/// `16 (header HMAC) + 8 (confounder) + RECORD_LEN = 404` total.
///
/// Kept as an independent constant even though `XProfileAccountWire`'s
/// field layout is what actually determines this on the wire - see
/// `wire_layout_matches_record_len` below, which cross-checks the two.
const RECORD_LEN: usize = 0x17c;

mod live_flag {
    pub(super) const PASSWORD_PROTECTED: u32 = 0x1000_0000;
    pub(super) const XBOX_LIVE_ENABLED: u32 = 0x2000_0000;
    pub(super) const RECOVERING: u32 = 0x4000_0000;
}

mod cached_flag {
    pub(super) const MEMBERSHIP_TYPE: u32 = 0x001f_0000;
    pub(super) const COUNTRY: u32 = 0x0000_ff00;
    pub(super) const LANGUAGE: u32 = 0x3e00_0000;
}

pub(crate) struct XProfileAccount {
    pub(crate) live_flags: u32,
    reserved: u32,
    pub(crate) gamertag: String,
    pub(crate) xuid_online: u64,
    pub(crate) cached_user_flags: u32,
    pub(crate) online_service_network_id: u32,
    /// Raw 4-button passcode bytes. Not mapped to `XOnlinePassCodeType`
    /// here - see `Enums.cs` if that's needed later.
    pub(crate) passcode: [u8; 4],
    pub(crate) online_domain: String,
    pub(crate) online_kerberos_realm: String,
    pub(crate) online_key: [u8; 16],
    pub(crate) user_passport_membername: String,
    pub(crate) user_passport_password: String,
    pub(crate) owner_passport_membername: String,
    /// Which `XeKeys` key this file decrypted with - retail or devkit.
    /// Reused on `to_array` so the round trip stays on the same key.
    pub(crate) is_devkit: bool,
}

/// On-wire layout of the plaintext (post-`unobfuscate`) Account record -
/// every field of `XProfileAccount` except `is_devkit`, which isn't part
/// of the record itself (it's *how* the record was unwrapped, decided by
/// which of the two fixed keys `unobfuscate` succeeded with).
///
/// Every field here is contiguous with the next - no gaps, no
/// `pad_before`/`pad_after` needed anywhere - so field order in this
/// struct *is* the wire layout.
#[derive(BinRead, BinWrite, Debug, Clone)]
#[brw(big)]
struct XProfileAccountWire {
    live_flags: u32,
    reserved: u32,
    /// 32 bytes, UTF-16BE, null-terminated within the fixed width.
    #[br(map = |b: [u8; 32]| read_utf16_be_fixed(&b))]
    #[bw(map = |s: &String| { let mut out = [0u8; 32]; write_utf16_be_fixed(&mut out, s); out })]
    gamertag: String,
    xuid_online: u64,
    cached_user_flags: u32,
    online_service_network_id: u32,
    passcode: [u8; 4],
    /// 20 bytes, fixed-width ASCII, null-terminated within the width.
    #[br(map = |b: [u8; 20]| read_ascii_fixed(&b))]
    #[bw(map = |s: &String| { let mut out = [0u8; 20]; write_ascii_fixed(&mut out, s); out })]
    online_domain: String,
    /// 24 bytes, same encoding as `online_domain`.
    #[br(map = |b: [u8; 24]| read_ascii_fixed(&b))]
    #[bw(map = |s: &String| { let mut out = [0u8; 24]; write_ascii_fixed(&mut out, s); out })]
    online_kerberos_realm: String,
    online_key: [u8; 16],
    /// 114 bytes, same encoding as `online_domain`.
    #[br(map = |b: [u8; 114]| read_ascii_fixed(&b))]
    #[bw(map = |s: &String| { let mut out = [0u8; 114]; write_ascii_fixed(&mut out, s); out })]
    user_passport_membername: String,
    /// 32 bytes, same encoding as `online_domain`.
    #[br(map = |b: [u8; 32]| read_ascii_fixed(&b))]
    #[bw(map = |s: &String| { let mut out = [0u8; 32]; write_ascii_fixed(&mut out, s); out })]
    user_passport_password: String,
    /// 114 bytes, same encoding as `online_domain`.
    #[br(map = |b: [u8; 114]| read_ascii_fixed(&b))]
    #[bw(map = |s: &String| { let mut out = [0u8; 114]; write_ascii_fixed(&mut out, s); out })]
    owner_passport_membername: String,
}

impl XProfileAccount {
    /// Tries the retail key, then devkit - mirrors the C# constructor.
    pub(crate) fn parse(account_buffer: &[u8]) -> Result<Self, anyhow::Error> {
        let (data, is_devkit) = match unobfuscate(account_buffer, false) {
            Some(d) => (d, false),
            None => (
                unobfuscate(account_buffer, true).context("account file is corrupted")?,
                true,
            ),
        };
        ensure!(
            data.len() == RECORD_LEN,
            "account record has unexpected length {} (expected {RECORD_LEN})",
            data.len()
        );

        let wire = XProfileAccountWire::read(&mut Cursor::new(&data))
            .map_err(|e| anyhow::anyhow!("xprofile: failed to parse account record: {e}"))?;

        Ok(Self {
            live_flags: wire.live_flags,
            reserved: wire.reserved,
            gamertag: wire.gamertag,
            xuid_online: wire.xuid_online,
            cached_user_flags: wire.cached_user_flags,
            online_service_network_id: wire.online_service_network_id,
            passcode: wire.passcode,
            online_domain: wire.online_domain,
            online_kerberos_realm: wire.online_kerberos_realm,
            online_key: wire.online_key,
            user_passport_membername: wire.user_passport_membername,
            user_passport_password: wire.user_passport_password,
            owner_passport_membername: wire.owner_passport_membername,
            is_devkit,
        })
    }

    /// Inverse of `parse` - re-obfuscates with the same key it was read with.
    pub(crate) fn to_array(&self) -> Vec<u8> {
        let wire = XProfileAccountWire {
            live_flags: self.live_flags,
            reserved: self.reserved,
            gamertag: self.gamertag.clone(),
            xuid_online: self.xuid_online,
            cached_user_flags: self.cached_user_flags,
            online_service_network_id: self.online_service_network_id,
            passcode: self.passcode,
            online_domain: self.online_domain.clone(),
            online_kerberos_realm: self.online_kerberos_realm.clone(),
            online_key: self.online_key,
            user_passport_membername: self.user_passport_membername.clone(),
            user_passport_password: self.user_passport_password.clone(),
            owner_passport_membername: self.owner_passport_membername.clone(),
        };

        let mut data = Vec::with_capacity(RECORD_LEN);
        wire.write(&mut Cursor::new(&mut data))
            .expect("writing a fixed-size record into an in-memory Vec<u8> cannot fail");
        debug_assert_eq!(data.len(), RECORD_LEN);

        obfuscate(&data, self.is_devkit)
    }

    pub(crate) fn recovering(&self) -> bool {
        self.live_flags & live_flag::RECOVERING != 0
    }
    pub(crate) fn set_recovering(&mut self, v: bool) {
        self.set_live_flag(live_flag::RECOVERING, v);
    }

    pub(crate) fn xbox_live_enabled(&self) -> bool {
        self.live_flags & live_flag::XBOX_LIVE_ENABLED != 0
    }
    pub(crate) fn set_xbox_live_enabled(&mut self, v: bool) {
        self.set_live_flag(live_flag::XBOX_LIVE_ENABLED, v);
    }

    pub(crate) fn password_protected(&self) -> bool {
        self.live_flags & live_flag::PASSWORD_PROTECTED != 0
    }
    pub(crate) fn set_password_protected(&mut self, v: bool) {
        self.set_live_flag(live_flag::PASSWORD_PROTECTED, v);
    }

    fn set_live_flag(&mut self, mask: u32, v: bool) {
        if v {
            self.live_flags |= mask;
        } else {
            self.live_flags &= !mask;
        }
    }

    /// Raw `XOnlineTierType`/`XOnlineCountry`/`XOnlineLanguage` bits - left
    /// unmapped for now, see `Enums.cs` for the full tables if needed.
    pub(crate) fn membership_type_raw(&self) -> u32 {
        self.cached_user_flags & cached_flag::MEMBERSHIP_TYPE
    }
    pub(crate) fn set_membership_type_raw(&mut self, v: u32) {
        self.set_cached_flag(cached_flag::MEMBERSHIP_TYPE, v);
    }

    pub(crate) fn country_raw(&self) -> u32 {
        self.cached_user_flags & cached_flag::COUNTRY
    }
    pub(crate) fn set_country_raw(&mut self, v: u32) {
        self.set_cached_flag(cached_flag::COUNTRY, v);
    }

    pub(crate) fn language_raw(&self) -> u32 {
        self.cached_user_flags & cached_flag::LANGUAGE
    }
    pub(crate) fn set_language_raw(&mut self, v: u32) {
        self.set_cached_flag(cached_flag::LANGUAGE, v);
    }

    fn set_cached_flag(&mut self, mask: u32, value: u32) {
        self.cached_user_flags = (self.cached_user_flags & !mask) | (value & mask);
    }
}

/// Big-endian UTF-16, stops at the first null code unit (mirrors
/// Horizon's `RemoveNullBytes`).
fn read_utf16_be_fixed(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(BE::read_u16)
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Truncates or null-pads to fit exactly `out.len()` bytes.
fn write_utf16_be_fixed(out: &mut [u8], s: &str) {
    out.fill(0);
    let max_units = out.len() / 2;
    for (i, unit) in s.encode_utf16().take(max_units).enumerate() {
        BE::write_u16(&mut out[i * 2..], unit);
    }
}

/// Fixed-width ASCII field, stops at the first null byte.
fn read_ascii_fixed(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn write_ascii_fixed(out: &mut [u8], s: &str) {
    out.fill(0);
    let bytes = s.as_bytes();
    let n = bytes.len().min(out.len());
    out[..n].copy_from_slice(&bytes[..n]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> XProfileAccount {
        XProfileAccount {
            live_flags: 0,
            reserved: 0,
            gamertag: "Test Gamer".to_string(),
            xuid_online: 0xE000_0123_4567_8901,
            cached_user_flags: 0,
            online_service_network_id: 0,
            passcode: [0; 4],
            online_domain: String::new(),
            online_kerberos_realm: String::new(),
            online_key: [0; 16],
            user_passport_membername: "user@example.com".to_string(),
            user_passport_password: String::new(),
            owner_passport_membername: String::new(),
            is_devkit: false,
        }
    }

    #[test]
    fn round_trips_through_to_array_and_parse() {
        let account = sample();
        let wire = account.to_array();
        let parsed = XProfileAccount::parse(&wire).expect("should parse what we just wrote");

        assert_eq!(parsed.gamertag, account.gamertag);
        assert_eq!(parsed.xuid_online, account.xuid_online);
        assert_eq!(
            parsed.user_passport_membername,
            account.user_passport_membername
        );
        assert_eq!(parsed.is_devkit, account.is_devkit);
    }

    #[test]
    fn live_flag_accessors_round_trip() {
        let mut account = sample();
        assert!(!account.xbox_live_enabled());
        account.set_xbox_live_enabled(true);
        account.set_recovering(true);
        assert!(account.xbox_live_enabled());
        assert!(account.recovering());
        assert!(!account.password_protected());
    }

    #[test]
    fn wire_layout_matches_record_len() {
        let account = sample();
        let raw = account.to_array();
        let data = unobfuscate(&raw, false).expect("sample() always writes with is_devkit=false");
        assert_eq!(data.len(), RECORD_LEN);

        assert_eq!(BE::read_u32(&data[0x00..]), account.live_flags);
        assert_eq!(BE::read_u32(&data[0x04..]), account.reserved);
        assert_eq!(
            read_utf16_be_fixed(&data[0x08..0x08 + 32]),
            account.gamertag
        );
        assert_eq!(BE::read_u64(&data[0x28..]), account.xuid_online);
        assert_eq!(BE::read_u32(&data[0x30..]), account.cached_user_flags);
        assert_eq!(
            BE::read_u32(&data[0x34..]),
            account.online_service_network_id
        );
        assert_eq!(&data[0x38..0x38 + 4], &account.passcode);
        assert_eq!(
            read_ascii_fixed(&data[0x3c..0x3c + 20]),
            account.online_domain
        );
        assert_eq!(
            read_ascii_fixed(&data[0x50..0x50 + 24]),
            account.online_kerberos_realm
        );
        assert_eq!(&data[0x68..0x68 + 16], &account.online_key);
        assert_eq!(
            read_ascii_fixed(&data[0x78..0x78 + 114]),
            account.user_passport_membername
        );
        assert_eq!(
            read_ascii_fixed(&data[0xea..0xea + 32]),
            account.user_passport_password
        );
        assert_eq!(
            read_ascii_fixed(&data[0x10a..0x10a + 114]),
            account.owner_passport_membername
        );
    }

    #[test]
    fn wire_round_trips_and_is_exactly_record_len() {
        let account = sample();
        let wire = XProfileAccountWire {
            live_flags: account.live_flags,
            reserved: account.reserved,
            gamertag: account.gamertag.clone(),
            xuid_online: account.xuid_online,
            cached_user_flags: account.cached_user_flags,
            online_service_network_id: account.online_service_network_id,
            passcode: account.passcode,
            online_domain: account.online_domain.clone(),
            online_kerberos_realm: account.online_kerberos_realm.clone(),
            online_key: account.online_key,
            user_passport_membername: account.user_passport_membername.clone(),
            user_passport_password: account.user_passport_password.clone(),
            owner_passport_membername: account.owner_passport_membername.clone(),
        };

        let mut buf = Vec::new();
        wire.write(&mut Cursor::new(&mut buf))
            .expect("writing a fixed-size record into an in-memory Vec<u8> cannot fail");
        assert_eq!(buf.len(), RECORD_LEN);

        let parsed = XProfileAccountWire::read(&mut Cursor::new(&buf))
            .expect("should parse what was just written");
        assert_eq!(parsed.gamertag, account.gamertag);
        assert_eq!(parsed.xuid_online, account.xuid_online);
        assert_eq!(
            parsed.user_passport_membername,
            account.user_passport_membername
        );
    }
}
