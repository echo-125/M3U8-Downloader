use aes::Aes128;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

use crate::core::error::CoreError;

type Aes128CbcDec = cbc::Decryptor<Aes128>;

pub fn implicit_iv(media_sequence: u64, segment_index: usize) -> [u8; 16] {
    let sequence = media_sequence + segment_index as u64;
    let mut iv = [0_u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

pub fn decrypt_aes128_cbc(
    encrypted: &[u8],
    key: &[u8],
    iv: [u8; 16],
) -> Result<Vec<u8>, CoreError> {
    if key.len() != 16 {
        return Err(CoreError::InvalidKey);
    }
    if encrypted.is_empty() || !encrypted.len().is_multiple_of(16) {
        return Err(CoreError::Decrypt);
    }
    Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(encrypted)
        .map_err(|_| CoreError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::{BlockEncryptMut, KeyIvInit};
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    fn encrypt_for_test(plaintext: &[u8], key: &[u8], iv: [u8; 16]) -> Vec<u8> {
        Aes128CbcEnc::new(key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plaintext)
    }

    #[test]
    fn derives_implicit_iv_from_media_sequence() {
        assert_eq!(
            implicit_iv(7, 3),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10]
        );
    }

    #[test]
    fn decrypts_padded_data() {
        let key = [1_u8; 16];
        let iv = [2_u8; 16];
        let encrypted = encrypt_for_test(b"cat catch", &key, iv);
        let decrypted = decrypt_aes128_cbc(&encrypted, &key, iv).unwrap();
        assert_eq!(decrypted, b"cat catch");
    }

    #[test]
    fn rejects_invalid_key_length() {
        assert!(matches!(
            decrypt_aes128_cbc(&[0; 16], &[1; 15], [2; 16]),
            Err(CoreError::InvalidKey)
        ));
    }
}
