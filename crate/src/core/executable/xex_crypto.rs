use aes::Aes128;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// The static global Xbox 360 Retail Executable Key, `xe_xex2_retail_key`
/// in Xenia's XEX loader:
/// <https://github.com/xenia-project/xenia/blob/master/src/xenia/cpu/xex_module.cc>
pub(crate) const RETAIL_KEY: [u8; 16] = [
    0x20, 0xB1, 0x85, 0xA5, 0x9D, 0x28, 0xFD, 0xC3, 0x40, 0x58, 0x3F, 0xBB, 0x08, 0x96, 0xBF, 0x91,
];

/// The all-zero devkit key.
pub(crate) const DEVKIT_KEY: [u8; 16] = [0u8; 16];

/// Decrypts the XEX body in-place using `xex_key` to unwrap the embedded
/// session key.
///
/// `xex_key`: `RETAIL_KEY` or `DEVKIT_KEY` - callers that don't know
/// which one a title uses should try `RETAIL_KEY` first and, if the
/// result doesn't look like a valid PE image, retry the whole
/// decrypt-then-decompress pipeline with `DEVKIT_KEY`.
/// `encrypted_session_key`: 16 bytes read from offset 0x150 of the `SecurityInfo` header.
/// `body`: A mutable reference to the compressed XEX body blocks.
pub(crate) fn decrypt_xex_body(
    xex_key: &[u8; 16],
    encrypted_session_key: [u8; 16],
    body: &mut [u8],
) -> Result<(), anyhow::Error> {
    let zero_iv = [0u8; 16];

    // Unwrap the session key with CBC + zero IV, matching Xenia's
    // `aes_decrypt_buffer` (ECB would be equivalent for one block, but
    // this keeps the same code path as the multi-block body decrypt below).
    let mut session_key = encrypted_session_key;
    {
        let dec = Aes128CbcDec::new(xex_key.into(), (&zero_iv).into());
        dec.decrypt_padded_mut::<NoPadding>(&mut session_key)
            .map_err(|_| anyhow::anyhow!("session key unwrap failed"))?;
    }

    // Only decrypt whole 16-byte blocks; any trailing partial block is
    // left untouched.
    let len = body.len() - (body.len() % 16);
    if len == 0 {
        return Ok(());
    }
    let dec = Aes128CbcDec::new((&session_key).into(), (&zero_iv).into());
    dec.decrypt_padded_mut::<NoPadding>(&mut body[..len])
        .map_err(|_| anyhow::anyhow!("XEX body decrypt failed"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::Aes128;
    use cbc::cipher::BlockEncryptMut;

    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    fn wrap_session_key(xex_key: &[u8; 16], session_key: [u8; 16]) -> [u8; 16] {
        let mut buf = session_key;
        let enc = Aes128CbcEnc::new(xex_key.into(), (&[0u8; 16]).into());
        enc.encrypt_padded_mut::<NoPadding>(&mut buf, 16)
            .expect("encrypting one aligned block should succeed");
        buf
    }

    fn encrypt_body(session_key: [u8; 16], plaintext: &[u8]) -> Vec<u8> {
        let mut buf = plaintext.to_vec();
        let enc = Aes128CbcEnc::new((&session_key).into(), (&[0u8; 16]).into());
        let len = buf.len();
        enc.encrypt_padded_mut::<NoPadding>(&mut buf, len)
            .expect("encrypting whole-block-aligned data should succeed");
        buf
    }

    #[test]
    fn decrypts_a_round_tripped_body_with_the_retail_key() {
        let session_key = [0x42u8; 16];
        let plaintext = [0x11u8; 32];
        let encrypted_session_key = wrap_session_key(&RETAIL_KEY, session_key);
        let mut body = encrypt_body(session_key, &plaintext);

        decrypt_xex_body(&RETAIL_KEY, encrypted_session_key, &mut body)
            .expect("decrypt should succeed");
        assert_eq!(body, plaintext);
    }

    #[test]
    fn decrypts_a_round_tripped_body_with_the_devkit_key() {
        let session_key = [0x7Au8; 16];
        let plaintext = [0x99u8; 48];
        let encrypted_session_key = wrap_session_key(&DEVKIT_KEY, session_key);
        let mut body = encrypt_body(session_key, &plaintext);

        decrypt_xex_body(&DEVKIT_KEY, encrypted_session_key, &mut body)
            .expect("decrypt should succeed");
        assert_eq!(body, plaintext);
    }

    #[test]
    fn wrong_xex_key_decrypts_without_error_but_produces_wrong_plaintext() {
        let session_key = [0x42u8; 16];
        let plaintext = [0x11u8; 16];
        let encrypted_session_key = wrap_session_key(&RETAIL_KEY, session_key);
        let mut body = encrypt_body(session_key, &plaintext);

        decrypt_xex_body(&DEVKIT_KEY, encrypted_session_key, &mut body)
            .expect("decrypt should still return Ok - there's no padding to fail on");
        assert_ne!(body, plaintext);
    }

    #[test]
    fn leaves_a_trailing_partial_block_untouched() {
        let session_key = [0x11u8; 16];
        let plaintext = [0xAAu8; 16];
        let encrypted_session_key = wrap_session_key(&RETAIL_KEY, session_key);
        let mut body = encrypt_body(session_key, &plaintext);
        let trailing = [0xDEu8, 0xAD, 0xBE, 0xEF, 0x00];
        body.extend_from_slice(&trailing);

        decrypt_xex_body(&RETAIL_KEY, encrypted_session_key, &mut body)
            .expect("decrypt should succeed");
        assert_eq!(&body[..16], &plaintext[..]);
        assert_eq!(&body[16..], &trailing[..]);
    }

    #[test]
    fn body_shorter_than_one_block_is_left_untouched() {
        let session_key = [0x11u8; 16];
        let encrypted_session_key = wrap_session_key(&RETAIL_KEY, session_key);
        let mut body = [0x01u8, 0x02, 0x03];

        decrypt_xex_body(&RETAIL_KEY, encrypted_session_key, &mut body)
            .expect("decrypt should succeed");
        assert_eq!(body, [0x01, 0x02, 0x03]);
    }
}
