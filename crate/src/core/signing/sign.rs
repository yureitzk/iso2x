//! SHA1 + PKCS#1v1.5 signing, producing signatures in Xbox 360 wire
//! format for an STFS/GoD header's signature field.

use super::keyvault::{ConsoleSigningKey, SIGNATURE_LEN, signature_to_wire_format};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use sha1::Sha1;

/// Signs `message` (the license table, header hash, and header size
/// fields, bytes `[0x22C, 0x344)` of a CON header) and returns the
/// signature in console wire-format order - 0x80 bytes, written at
/// header offset 0x1AC.
/// `<https://free60.org/System-Software/Formats/STFS/#signatures>`
pub(crate) fn sign_pkcs1_sha1(
    key: &ConsoleSigningKey,
    message: &[u8],
) -> Result<[u8; SIGNATURE_LEN], anyhow::Error> {
    let signing_key = SigningKey::<Sha1>::new(key.private_key.clone());
    let mut rng = rand::thread_rng();
    let signature = signing_key.sign_with_rng(&mut rng, message);
    let bytes = signature.to_bytes();
    let arr: [u8; SIGNATURE_LEN] = bytes
        .as_ref()
        .try_into()
        .map_err(|_| anyhow::anyhow!("unexpected signature length {}", bytes.len()))?;
    Ok(signature_to_wire_format(arr))
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
        let key = ConsoleSigningKey {
            private_key: private_key.clone(),
            certificate: [0u8; 0x1A8],
        };
        (key, private_key)
    }

    #[test]
    fn signature_has_expected_wire_length() {
        let (key, _) = synthetic_key();
        let sig = sign_pkcs1_sha1(&key, b"license table + header digest region")
            .expect("signing should succeed");
        assert_eq!(sig.len(), SIGNATURE_LEN);
    }

    #[test]
    fn signature_verifies_against_the_original_key_once_wire_format_is_undone() {
        let (key, original) = synthetic_key();
        let msg = b"license table + header digest region";
        let wire_sig = sign_pkcs1_sha1(&key, msg).expect("signing should succeed");
        let standard_sig_bytes = signature_to_wire_format(wire_sig);

        let signature = Signature::try_from(standard_sig_bytes.as_slice())
            .expect("should decode as a pkcs1v15 signature");
        let verifying_key = VerifyingKey::<Sha1>::new(original.to_public_key());
        verifying_key
            .verify(msg, &signature)
            .expect("signature should verify against the original key");
    }

    #[test]
    fn signature_does_not_verify_against_a_different_message() {
        let (key, original) = synthetic_key();
        let wire_sig = sign_pkcs1_sha1(&key, b"message A").expect("signing should succeed");
        let standard_sig_bytes = signature_to_wire_format(wire_sig);
        let signature = Signature::try_from(standard_sig_bytes.as_slice())
            .expect("should decode as a pkcs1v15 signature");
        let verifying_key = VerifyingKey::<Sha1>::new(original.to_public_key());

        assert!(
            verifying_key.verify(b"message B", &signature).is_err(),
            "a signature over message A must not verify against message B"
        );
    }

    #[test]
    fn rejects_a_signature_whose_length_does_not_match_signature_len() {
        let mut rng = rand::thread_rng();
        let small_key = RsaPrivateKey::new(&mut rng, 512).expect("keygen");
        let key = ConsoleSigningKey {
            private_key: small_key,
            certificate: [0u8; 0x1A8],
        };

        let err = sign_pkcs1_sha1(&key, b"test message")
            .expect_err("a 512-bit key should produce a 0x40-byte signature, not 0x80");
        assert!(
            err.to_string().contains("unexpected signature length"),
            "unexpected error message: {err}"
        );
    }
}
