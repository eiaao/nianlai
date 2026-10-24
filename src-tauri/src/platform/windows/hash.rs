use crate::{history::content::Fingerprint, platform::Result};
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};

pub fn sha256(bytes: &[u8]) -> Result<Fingerprint> {
    if bytes.len() > u32::MAX as usize {
        return Err("hash_input_too_large".into());
    }
    let mut digest = [0; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut digest) }
        .ok()
        .map_err(|_| "hash_failed")?;
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sha256_matches_known_vector() {
        let digest = sha256(b"abc").unwrap();
        let text: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            text,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
