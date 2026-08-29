//! Layout: `[16-byte header HMAC][8-byte confounder][payload]`, RC4-keyed
//! off two HMAC-SHA1 derivations. Not console-bound - one of two fixed
//! keys (retail/devkit), same as upstream.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// `XeKeys` HMAC key for the Account file - unrelated to the AES key of
/// the same name in `xex_crypto.rs` (different algorithm, different
/// purpose - kept distinctly named to avoid confusion between the two).
const ACCOUNT_RETAIL_KEY: [u8; 16] = [
    0xE1, 0xBC, 0x15, 0x9C, 0x73, 0xB1, 0xEA, 0xE9, 0xAB, 0x31, 0x70, 0xF3, 0xAD, 0x47, 0xEB, 0xF3,
];
const ACCOUNT_DEVKIT_KEY: [u8; 16] = [
    0xDA, 0xB6, 0x9A, 0xD9, 0x8E, 0x28, 0x76, 0x4F, 0x97, 0x7E, 0xE2, 0x48, 0x7E, 0x4F, 0x3F, 0x68,
];

fn hvp_key(dev: bool) -> &'static [u8; 16] {
    if dev {
        &ACCOUNT_DEVKIT_KEY
    } else {
        &ACCOUNT_RETAIL_KEY
    }
}

/// First 16 bytes of HMAC-SHA1(key, data) - this module only ever needs
/// `digestSize = 0x10`, so the truncation is baked in rather than parameterized.
fn hmac16(key: &[u8], data: &[u8]) -> [u8; 16] {
    let mut mac = HmacSha1::new_from_slice(key).expect("HMAC-SHA1 accepts any key length");
    mac.update(data);
    let full = mac.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// Standard RC4 KSA + PRGA, `XORed` into `data` in place.
#[allow(clippy::cast_possible_truncation)] // `i` only ever ranges 0..256, the length of `s`
fn rc4_xor_in_place(data: &mut [u8], key: &[u8]) {
    let mut s = [0u8; 256];
    for (i, b) in s.iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut j = 0usize;
    for i in 0..256 {
        j = (j + s[i] as usize + key[i % key.len()] as usize) % 256;
        s.swap(i, j);
    }
    let (mut i, mut j) = (0usize, 0usize);
    for byte in data.iter_mut() {
        i = (i + 1) % 256;
        j = (j + s[i] as usize) % 256;
        s.swap(i, j);
        let k = s[(s[i] as usize + s[j] as usize) % 256];
        *byte ^= k;
    }
}

/// `None` on HMAC mismatch (wrong key or corrupt data) - matches the C#
/// `null` return, used by callers to fall back retail -> devkit.
pub(super) fn unobfuscate(encrypted: &[u8], dev: bool) -> Option<Vec<u8>> {
    if encrypted.len() < 0x18 {
        return None;
    }
    let key = hvp_key(dev);
    let base_key = &encrypted[..0x10];
    let mut body = encrypted[0x10..].to_vec();

    let rc4_key = hmac16(key, base_key);
    rc4_xor_in_place(&mut body, &rc4_key);

    if hmac16(key, &body).as_slice() != base_key {
        return None;
    }

    Some(body[8..].to_vec())
}

pub(super) fn obfuscate(plaintext: &[u8], dev: bool) -> Vec<u8> {
    let key = hvp_key(dev);

    let mut body = vec![0u8; 8 + plaintext.len()];
    rand::thread_rng().fill_bytes(&mut body[..8]); // confounder
    body[8..].copy_from_slice(plaintext);

    let header_key = hmac16(key, &body);
    let rc4_key = hmac16(key, &header_key);
    rc4_xor_in_place(&mut body, &rc4_key);

    let mut out = Vec::with_capacity(0x10 + body.len());
    out.extend_from_slice(&header_key);
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscate_round_trips() {
        let plaintext = b"hello from a profile account file test vector!";
        let wire = obfuscate(plaintext, false);
        let recovered = unobfuscate(&wire, false).expect("should decrypt with the same key");
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn wrong_key_fails_verification() {
        let wire = obfuscate(b"retail data", false);
        assert!(unobfuscate(&wire, true).is_none());
    }

    #[test]
    fn short_buffer_is_rejected() {
        assert!(unobfuscate(&[0u8; 4], false).is_none());
    }
}
