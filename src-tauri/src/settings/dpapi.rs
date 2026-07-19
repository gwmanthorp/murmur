//! Windows DPAPI (per-user, UI-forbidden) for encrypting the API key at rest.

use base64::Engine as _;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

pub fn encrypt(plaintext: &str) -> Option<String> {
    if plaintext.is_empty() {
        return Some(String::new());
    }
    let bytes = plaintext.as_bytes();
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .ok()?;
        let encrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
        Some(base64::engine::general_purpose::STANDARD.encode(encrypted))
    }
}

pub fn decrypt(blob_b64: &str) -> Option<String> {
    if blob_b64.is_empty() {
        return Some(String::new());
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob_b64)
        .ok()?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .ok()?;
        let decrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData as *mut core::ffi::c_void)));
        String::from_utf8(decrypted).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let secret = "gsk_test_1234567890abcdef";
        let blob = encrypt(secret).expect("encrypt failed");
        assert!(!blob.contains(secret));
        assert_ne!(blob, secret);
        assert_eq!(decrypt(&blob).expect("decrypt failed"), secret);
    }

    #[test]
    fn empty_string_passthrough() {
        assert_eq!(encrypt("").unwrap(), "");
        assert_eq!(decrypt("").unwrap(), "");
    }

    #[test]
    fn garbage_blob_fails_gracefully() {
        assert!(decrypt("not-base64!!!").is_none());
        assert!(decrypt("aGVsbG8=").is_none()); // valid base64, not a DPAPI blob
    }
}
