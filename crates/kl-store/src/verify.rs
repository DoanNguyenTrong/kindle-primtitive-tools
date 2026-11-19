//! Checking a downloaded file against the listing: its size and its SHA-256, read in pieces so a large file is not held
//! in memory.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const MAX_ARCHIVE_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    InvalidArchiveSize(u64),
    Size { expected: u64, found: u64 },
    Hash { expected: String, found: String },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::InvalidArchiveSize(0) => write!(f, "the catalog lists an empty package"),
            VerifyError::InvalidArchiveSize(_) => write!(f, "the package exceeds the Store's 200 MiB download limit"),
            VerifyError::Size { expected, found } => write!(f, "the download is {found} bytes, the listing says {expected}"),
            VerifyError::Hash { .. } => write!(f, "the download does not match its SHA-256 in the listing"),
        }
    }
}

pub fn check_archive_size(size: u64) -> Result<(), VerifyError> {
    if (1..=MAX_ARCHIVE_BYTES).contains(&size) {
        Ok(())
    } else {
        Err(VerifyError::InvalidArchiveSize(size))
    }
}

/// The lower-case hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let digest = hasher.finalize();
    Ok((digest.iter().map(|b| format!("{b:02x}")).collect(), total))
}

/// Checks size first (cheap), then the hash. The expected hash is compared in lower case.
pub fn verify_file(path: &Path, expected_sha256: &str, expected_size: u64) -> Result<(), VerifyError> {
    check_archive_size(expected_size)?;
    let measured = path.metadata().map_or(0, |metadata| metadata.len());
    if measured != expected_size {
        return Err(VerifyError::Size { expected: expected_size, found: measured });
    }
    let (found, size) = sha256_file(path).map_err(|_| VerifyError::Size { expected: expected_size, found: 0 })?;
    if size != expected_size {
        return Err(VerifyError::Size { expected: expected_size, found: size });
    }
    if found != expected_sha256.to_ascii_lowercase() {
        return Err(VerifyError::Hash { expected: expected_sha256.to_string(), found });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("kl-verify-{}-{name}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn hashes_a_known_input() {
        // SHA-256 of "abc".
        let p = temp_file("abc", b"abc");
        let (hash, size) = sha256_file(&p).unwrap();
        assert_eq!(hash, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(size, 3);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn archive_sizes_match_the_catalog_and_packager_limit() {
        assert_eq!(check_archive_size(0), Err(VerifyError::InvalidArchiveSize(0)));
        assert!(check_archive_size(MAX_ARCHIVE_BYTES).is_ok());
        let oversize = MAX_ARCHIVE_BYTES + 1;
        assert_eq!(verify_file(Path::new("unused"), "", oversize), Err(VerifyError::InvalidArchiveSize(oversize)));
    }

    #[test]
    fn accepts_a_match_and_refuses_a_changed_file() {
        let p = temp_file("match", b"abc");
        let good = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD";
        assert!(verify_file(&p, good, 3).is_ok());
        assert!(matches!(verify_file(&p, &"0".repeat(64), 3), Err(VerifyError::Hash { .. })));
        assert!(matches!(verify_file(&p, good, 4), Err(VerifyError::Size { expected: 4, found: 3 })));
        let _ = std::fs::remove_file(p);
    }
}
