//! Parses an Xbox 360 console keyvault to extract the RSA console
//! signing key and console certificate.
//!
//! The certificate at offset 0x9C8 matches the "Console Security
//! Certificate" structure documented at
//! `<https://free60.org/System-Software/Formats/STFS/#signatures>`

use anyhow::{Context, ensure};
use rsa::traits::PublicKeyParts;
use rsa::{BigUint, RsaPrivateKey};

/// Offsets into a raw keyvault dump.
mod offset {
    pub(super) const EXPONENT: usize = 0x29C;
    /// Modulus (0x80 bytes), followed by P/Q/DP/DQ/InverseQ (0x40
    /// bytes each), read as one contiguous 0x2A0-byte region.
    pub(super) const MODULUS: usize = 0x2A8;
    pub(super) const CERTIFICATE: usize = 0x9C8;
    pub(super) const CERTIFICATE_LEN: usize = 0x1A8;
}

const MODULUS_LEN: usize = 0x80;
const PRIME_LEN: usize = 0x40;

/// PKCS#1 signature length for a 1024-bit console signing key.
pub(crate) const SIGNATURE_LEN: usize = MODULUS_LEN;

/// Modulus + P + Q + DP + DQ + `InverseQ`, read as one contiguous block.
const PRIVATE_KEY_BLOCK_LEN: usize = MODULUS_LEN + PRIME_LEN * 5;

/// The Xbox 360 console signing key, extracted from a keyvault and
/// converted into a form the `rsa` crate can sign with.
pub(crate) struct ConsoleSigningKey {
    pub(crate) private_key: RsaPrivateKey,
    /// Copied verbatim into the CON header at offset 0x4 when
    /// console-signing.
    pub(crate) certificate: [u8; offset::CERTIFICATE_LEN],
}

/// Xbox 360 bignums are stored as a reversed sequence of 8-byte qwords
/// relative to standard big-endian order. Swaps qword `i` with qword
/// `N-1-i` in place; with `reverse_bytes` set, also reverses the byte
/// order within each qword (needed for the signature field).
///
/// This is its own inverse: two applications with the same
/// `reverse_bytes` restore the original bytes.
fn bn_qw_swap(data: &mut [u8], reverse_bytes: bool) {
    assert!(
        data.len().is_multiple_of(8),
        "bn_qw_swap: length must be a multiple of 8"
    );
    let len = data.len();
    let mut i = 0;
    while i < len / 2 {
        for k in 0..8 {
            data.swap(i + k, len - i - 8 + k);
        }
        i += 8;
    }
    if reverse_bytes {
        for chunk in data.chunks_mut(8) {
            chunk.reverse();
        }
    }
}

impl ConsoleSigningKey {
    pub(crate) fn parse(kv: &[u8]) -> Result<Self, anyhow::Error> {
        ensure!(
            kv.len() >= offset::CERTIFICATE + offset::CERTIFICATE_LEN,
            "keyvault: buffer too short ({} bytes) to contain a certificate",
            kv.len()
        );
        ensure!(
            kv.len() >= offset::MODULUS + PRIVATE_KEY_BLOCK_LEN,
            "keyvault: buffer too short ({} bytes) to contain a private key",
            kv.len()
        );
        let exponent = u32::from_be_bytes(
            kv[offset::EXPONENT..offset::EXPONENT + 4]
                .try_into()
                .expect("4-byte slice"),
        );
        let mut block = [0u8; PRIVATE_KEY_BLOCK_LEN];
        block.copy_from_slice(&kv[offset::MODULUS..offset::MODULUS + PRIVATE_KEY_BLOCK_LEN]);
        let (modulus, rest) = block.split_at_mut(MODULUS_LEN);
        let (p, rest) = rest.split_at_mut(PRIME_LEN);
        let (q, rest) = rest.split_at_mut(PRIME_LEN);
        // dp/dq/qinv aren't needed - `RsaPrivateKey::from_p_q` derives
        // them itself from p, q, and e, then cross-checks against the
        // modulus we pass in.
        let (dp, rest) = rest.split_at_mut(PRIME_LEN);
        let (dq, qinv) = rest.split_at_mut(PRIME_LEN);
        let _ = (dp, dq, qinv);
        bn_qw_swap(modulus, false);
        bn_qw_swap(p, false);
        bn_qw_swap(q, false);
        let n = BigUint::from_bytes_be(modulus);
        let p = BigUint::from_bytes_be(p);
        let q = BigUint::from_bytes_be(q);
        let e = BigUint::from(exponent);
        let mut private_key = RsaPrivateKey::from_p_q(p, q, e)
            .context("keyvault: p/q/e from the keyvault don't form a valid RSA key")?;
        private_key
            .precompute()
            .context("keyvault: RSA precompute failed")?;
        ensure!(
            *private_key.n() == n,
            "keyvault: modulus derived from P*Q doesn't match the modulus stored in the \
             keyvault - the private key is corrupt, or bn_qw_swap decoded the fields wrong"
        );
        let mut certificate = [0u8; offset::CERTIFICATE_LEN];
        certificate.copy_from_slice(
            &kv[offset::CERTIFICATE..offset::CERTIFICATE + offset::CERTIFICATE_LEN],
        );
        Ok(Self {
            private_key,
            certificate,
        })
    }
}

/// Converts a standard big-endian PKCS#1v1.5 signature into the
/// qword-swapped wire format the Xbox 360 expects on disk.
pub(crate) fn signature_to_wire_format(mut signature: [u8; SIGNATURE_LEN]) -> [u8; SIGNATURE_LEN] {
    bn_qw_swap(&mut signature, true);
    signature
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_is_involutive_no_byte_reverse() {
        let mut data: Vec<u8> = (0..0x80u32).map(|i| i as u8).collect();
        let original = data.clone();
        bn_qw_swap(&mut data, false);
        assert_ne!(data, original, "swap should have changed something");
        bn_qw_swap(&mut data, false);
        assert_eq!(data, original, "applying twice should restore the original");
    }

    #[test]
    fn swap_is_involutive_with_byte_reverse() {
        let mut data: Vec<u8> = (0..0x100u32).map(|i| i as u8).collect();
        let original = data.clone();
        bn_qw_swap(&mut data, true);
        assert_ne!(data, original);
        bn_qw_swap(&mut data, true);
        assert_eq!(data, original);
    }

    #[test]
    fn swap_matches_hand_traced_example() {
        let mut data: Vec<u8> = vec![
            1, 2, 3, 4, 5, 6, 7, 8, // A
            9, 10, 11, 12, 13, 14, 15, 16, // B
            17, 18, 19, 20, 21, 22, 23, 24, // C
        ];
        bn_qw_swap(&mut data, false);
        assert_eq!(
            data,
            vec![
                17, 18, 19, 20, 21, 22, 23, 24, // C
                9, 10, 11, 12, 13, 14, 15, 16, // B (untouched, middle qword)
                1, 2, 3, 4, 5, 6, 7, 8, // A
            ]
        );
    }

    #[test]
    fn parse_round_trips_a_synthetic_keyvault_and_signs() {
        use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
        use rsa::signature::{RandomizedSigner, Verifier};
        use rsa::traits::PrivateKeyParts;
        use sha1::Sha1;

        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 1024).expect("keygen");
        let n = key.n().to_bytes_be();
        let p = key.primes()[0].to_bytes_be();
        let q = key.primes()[1].to_bytes_be();
        assert_eq!(n.len(), MODULUS_LEN, "test assumes a 1024-bit modulus");
        assert_eq!(p.len(), PRIME_LEN);
        assert_eq!(q.len(), PRIME_LEN);

        let mut kv = vec![0u8; offset::MODULUS + PRIVATE_KEY_BLOCK_LEN];
        kv[offset::EXPONENT..offset::EXPONENT + 4].copy_from_slice(&0x10001u32.to_be_bytes());

        let mut n_wire = n.clone();
        let mut p_wire = p.clone();
        let mut q_wire = q.clone();
        bn_qw_swap(&mut n_wire, false);
        bn_qw_swap(&mut p_wire, false);
        bn_qw_swap(&mut q_wire, false);

        let base = offset::MODULUS;
        kv[base..base + MODULUS_LEN].copy_from_slice(&n_wire);
        kv[base + MODULUS_LEN..base + MODULUS_LEN + PRIME_LEN].copy_from_slice(&p_wire);
        kv[base + MODULUS_LEN + PRIME_LEN..base + MODULUS_LEN + 2 * PRIME_LEN]
            .copy_from_slice(&q_wire);
        // dp/dq/qinv left zeroed - parse() doesn't read them.

        kv.resize(offset::CERTIFICATE + offset::CERTIFICATE_LEN, 0);
        for (i, b) in kv[offset::CERTIFICATE..offset::CERTIFICATE + offset::CERTIFICATE_LEN]
            .iter_mut()
            .enumerate()
        {
            *b = i as u8;
        }

        let parsed = ConsoleSigningKey::parse(&kv).expect("should parse synthetic keyvault");
        assert_eq!(*parsed.private_key.n(), *key.n());
        assert_eq!(
            parsed.certificate,
            kv[offset::CERTIFICATE..offset::CERTIFICATE + offset::CERTIFICATE_LEN]
        );

        let signing_key = SigningKey::<Sha1>::new(parsed.private_key);
        let msg = b"hello from a converted xbox 360 stfs header";
        let signature: Signature = signing_key.sign_with_rng(&mut rng, msg);
        let verifying_key = VerifyingKey::<Sha1>::new(key.to_public_key());
        verifying_key
            .verify(msg, &signature)
            .expect("signature from the parsed key should verify against the original key");
    }
}
