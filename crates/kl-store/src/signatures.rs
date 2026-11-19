//! Detached Ed25519 proofs for the catalog index and package archives.
//!
//! The index proof binds the exact index bytes and each listing digest. The package proof binds the
//! app ID, version, architecture, and expected ZIP SHA-256 and size. Verify the ZIP against those same
//! expected values before unpacking. Callers select the trusted key independently of the catalog.

use crate::{catalog::valid_app_id, refresh::MAX_INDEX, verify::check_archive_size, Version};
use ring::signature::{Ed25519KeyPair, UnparsedPublicKey, ED25519};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt};

pub const INDEX_SIGNATURE_FILENAME: &str = "index.sig.json";
pub const MAX_INDEX_SIDECAR_BYTES: u64 = 1024 * 1024;
pub const MAX_SIGNATURE_BYTES: u64 = 256;

const PROOF_SCHEMA: u32 = 1;
const INDEX_CONTEXT: &[u8] = b"Primitive Kindle Store index signature v1\0";
const PACKAGE_CONTEXT: &[u8] = b"Primitive Kindle Store package signature v1\0";

#[derive(Debug, PartialEq, Eq)]
pub enum SignatureError {
    InvalidKey,
    InvalidIndexProof,
    UnsupportedSchema,
    InvalidSignature,
    SignatureMismatch,
    MissingListing,
    ListingMismatch,
    InvalidPackageIdentity,
    InvalidAssetUrl,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidKey => "the Store trust file does not contain a valid Ed25519 public key",
            Self::InvalidIndexProof => "the signed catalog proof is malformed",
            Self::UnsupportedSchema => "the signed catalog proof has an unsupported schema",
            Self::InvalidSignature => "the Store signature is malformed",
            Self::SignatureMismatch => "the Store signature did not verify with the trusted key",
            Self::MissingListing => "the signed catalog does not include this listing",
            Self::ListingMismatch => "the listing does not match the signed catalog",
            Self::InvalidPackageIdentity => "the signed package identity is invalid",
            Self::InvalidAssetUrl => {
                "the Store could not locate a detached signature for the package URL"
            }
        })
    }
}

impl std::error::Error for SignatureError {}

/// One independently provisioned Ed25519 public key, encoded as 64 hexadecimal digits.
#[derive(Clone)]
pub struct TrustedKey([u8; 32]);

impl TrustedKey {
    pub fn from_hex(input: &str) -> Result<Self, SignatureError> {
        let key = decode_hex(input.trim_ascii()).ok_or(SignatureError::InvalidKey)?;
        if weak_or_noncanonical_key(key) {
            return Err(SignatureError::InvalidKey);
        }
        Ok(Self(key))
    }

    pub fn verify_index(
        &self,
        index: &[u8],
        sidecar: &[u8],
    ) -> Result<VerifiedIndex, SignatureError> {
        if sidecar.len() as u64 > MAX_INDEX_SIDECAR_BYTES {
            return Err(SignatureError::InvalidIndexProof);
        }
        let proof: IndexProof =
            serde_json::from_slice(sidecar).map_err(|_| SignatureError::InvalidIndexProof)?;
        if proof.schema != PROOF_SCHEMA {
            return Err(SignatureError::UnsupportedSchema);
        }
        let digests = proof
            .listings
            .into_iter()
            .map(|(id, digest)| {
                decode_hex(&digest)
                    .map(|digest| (id, digest))
                    .ok_or(SignatureError::InvalidIndexProof)
            })
            .collect::<Result<_, _>>()?;
        self.verify(&index_message(index, &digests)?, &proof.signature)?;
        Ok(VerifiedIndex(digests))
    }

    pub fn verify_package(
        &self,
        package: PackageIdentity<'_>,
        signature: &str,
    ) -> Result<(), SignatureError> {
        self.verify(&package_message(package)?, signature)
    }

    fn verify(&self, message: &[u8], signature: &str) -> Result<(), SignatureError> {
        if signature.len() as u64 > MAX_SIGNATURE_BYTES {
            return Err(SignatureError::InvalidSignature);
        }
        let signature =
            decode_hex::<64>(signature.trim_ascii()).ok_or(SignatureError::InvalidSignature)?;
        UnparsedPublicKey::new(&ED25519, self.0)
            .verify(message, &signature)
            .map_err(|_| SignatureError::SignatureMismatch)
    }
}

fn weak_or_noncanonical_key(mut y: [u8; 32]) -> bool {
    // Clear the x sign bit before checking the compressed y coordinate. The two order-8
    // coordinates are the curve constants also used by libsodium's ge25519_has_small_order.
    const ORDER_EIGHT_Y: [&str; 2] = [
        "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
        "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
    ];
    y[31] &= 0x7f;
    let zero_or_one = y[0] <= 1 && y[1..].iter().all(|&byte| byte == 0);
    // The field modulus is 2^255 - 19. y = modulus - 1 has order 2.
    let at_field_limit = y[0] >= 0xec && y[31] == 0x7f && y[1..31].iter().all(|&byte| byte == 0xff);
    zero_or_one
        || at_field_limit
        || ORDER_EIGHT_Y
            .iter()
            .any(|value| decode_hex(value) == Some(y))
}

#[derive(Debug)]
pub struct VerifiedIndex(BTreeMap<String, [u8; 32]>);

impl VerifiedIndex {
    pub fn listing_count(&self) -> usize {
        self.0.len()
    }

    pub fn contains_listing(&self, id: &str) -> bool {
        self.0.contains_key(id)
    }

    pub fn verify_listing(&self, id: &str, listing: &[u8]) -> Result<(), SignatureError> {
        let expected = self.0.get(id).ok_or(SignatureError::MissingListing)?;
        let actual: [u8; 32] = Sha256::digest(listing).into();
        if actual != *expected {
            return Err(SignatureError::ListingMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct PackageIdentity<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub arch: &'a str,
    pub sha256: &'a str,
    pub size: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexProof {
    schema: u32,
    #[serde(deserialize_with = "unique_listings")]
    listings: BTreeMap<String, String>,
    signature: String,
}

fn unique_listings<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct Listings;

    impl<'de> Visitor<'de> for Listings {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map of unique app IDs to listing digests")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut listings = BTreeMap::new();
            while let Some((id, digest)) = map.next_entry()? {
                if listings.insert(id, digest).is_some() {
                    return Err(de::Error::custom("duplicate signed app ID"));
                }
            }
            Ok(listings)
        }
    }

    deserializer.deserialize_map(Listings)
}

/// Create an index proof with the same encoding the Kindle verifies.
pub fn sign_index(
    key: &Ed25519KeyPair,
    index: &[u8],
    listings: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>, SignatureError> {
    let digests: BTreeMap<_, [u8; 32]> = listings
        .iter()
        .map(|(id, bytes)| (id.clone(), Sha256::digest(bytes).into()))
        .collect();
    let signature = key.sign(&index_message(index, &digests)?);
    let proof = IndexProof {
        schema: PROOF_SCHEMA,
        listings: digests
            .into_iter()
            .map(|(id, hash)| (id, encode_hex(&hash)))
            .collect(),
        signature: encode_hex(signature.as_ref()),
    };
    let sidecar = serde_json::to_vec(&proof).map_err(|_| SignatureError::InvalidIndexProof)?;
    if sidecar.len() as u64 > MAX_INDEX_SIDECAR_BYTES {
        return Err(SignatureError::InvalidIndexProof);
    }
    Ok(sidecar)
}

/// Sign the measured ZIP fingerprint and its catalog identity.
pub fn sign_package(
    key: &Ed25519KeyPair,
    package: PackageIdentity<'_>,
) -> Result<String, SignatureError> {
    Ok(encode_hex(key.sign(&package_message(package)?).as_ref()))
}

fn index_message(
    index: &[u8],
    digests: &BTreeMap<String, [u8; 32]>,
) -> Result<Vec<u8>, SignatureError> {
    let listing_count =
        u32::try_from(digests.len()).map_err(|_| SignatureError::InvalidIndexProof)?;
    if index.len() as u64 > MAX_INDEX {
        return Err(SignatureError::InvalidIndexProof);
    }
    let mut message = INDEX_CONTEXT.to_vec();
    message.extend_from_slice(&(index.len() as u64).to_be_bytes());
    message.extend_from_slice(&Sha256::digest(index));
    message.extend_from_slice(&listing_count.to_be_bytes());
    for (id, digest) in digests {
        if !valid_app_id(id) {
            return Err(SignatureError::InvalidIndexProof);
        }
        message.push(id.len() as u8);
        message.extend_from_slice(id.as_bytes());
        message.extend_from_slice(digest);
    }
    Ok(message)
}

fn package_message(package: PackageIdentity<'_>) -> Result<Vec<u8>, SignatureError> {
    if !valid_app_id(package.id)
        || !matches!(package.arch, "armhf" | "armel")
        || package.version.len() > u8::MAX as usize
        || Version::parse(package.version).is_none()
        || check_archive_size(package.size).is_err()
    {
        return Err(SignatureError::InvalidPackageIdentity);
    }
    let digest = decode_hex::<32>(package.sha256).ok_or(SignatureError::InvalidPackageIdentity)?;
    let mut message = PACKAGE_CONTEXT.to_vec();
    for field in [package.id, package.version, package.arch] {
        message.push(field.len() as u8);
        message.extend_from_slice(field.as_bytes());
    }
    message.extend_from_slice(&package.size.to_be_bytes());
    message.extend_from_slice(&digest);
    Ok(message)
}

/// Derive the detached sidecar's location without changing the HTTPS host or query.
/// The network caller still checks the result against the Store's host allowlist.
pub fn package_signature_url(url: &str) -> Result<String, SignatureError> {
    let invalid = SignatureError::InvalidAssetUrl;
    let uri: http::Uri = url.parse().map_err(|_| SignatureError::InvalidAssetUrl)?;
    let authority = uri.authority().ok_or(SignatureError::InvalidAssetUrl)?;
    if uri.scheme_str() != Some("https")
        || authority.as_str().contains('@')
        || (authority.as_str() != authority.host() && authority.port_u16() != Some(443))
        || uri.path().ends_with('/')
        || url.contains('#')
    {
        return Err(invalid);
    }
    let path = match uri.query() {
        Some(query) => format!("{}.sig?{query}", uri.path()),
        None => format!("{}.sig", uri.path()),
    };
    let mut parts = uri.into_parts();
    parts.path_and_query = Some(path.parse().map_err(|_| SignatureError::InvalidAssetUrl)?);
    http::Uri::from_parts(parts)
        .map(|uri| uri.to_string())
        .map_err(|_| SignatureError::InvalidAssetUrl)
}

pub fn encode_hex(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(data.len() * 2);
    for &byte in data {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0xf) as usize] as char);
    }
    encoded
}

fn decode_hex<const N: usize>(hex: &str) -> Option<[u8; N]> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    if hex.len() != N * 2 {
        return None;
    }
    let mut bytes = [0; N];
    let (pairs, _) = hex.as_bytes().as_chunks::<2>();
    for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
        *byte = value(high)? << 4 | value(low)?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{verify_file, VerifyError};
    use ring::{rand::SystemRandom, signature::KeyPair};

    fn key() -> (Ed25519KeyPair, TrustedKey) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let trusted = TrustedKey::from_hex(&encode_hex(key.public_key().as_ref())).unwrap();
        (key, trusted)
    }

    const INDEX: &[u8] = b"{\"schema\":1,\"apps\":[\"notes\",\"words\"]}\n";

    fn catalog() -> BTreeMap<String, Vec<u8>> {
        [
            ("words".into(), b"{\"id\":\"words\"}".to_vec()),
            ("notes".into(), b"{\"id\":\"notes\"}".to_vec()),
        ]
        .into()
    }

    #[test]
    fn verifies_exact_index_and_listing_bytes_with_the_pinned_key() {
        let (key, trusted) = key();
        let listings = catalog();
        let sidecar = sign_index(&key, INDEX, &listings).unwrap();
        let verified = trusted.verify_index(INDEX, &sidecar).unwrap();
        assert!(verified.contains_listing("notes"));
        assert_eq!(verified.verify_listing("notes", &listings["notes"]), Ok(()));
        assert_eq!(
            verified.verify_listing("notes", &listings["words"]),
            Err(SignatureError::ListingMismatch)
        );
        assert_eq!(
            verified.verify_listing("other", &listings["notes"]),
            Err(SignatureError::MissingListing)
        );
        assert_eq!(
            trusted
                .verify_index(&INDEX[..INDEX.len() - 1], &sidecar)
                .unwrap_err(),
            SignatureError::SignatureMismatch
        );
        let (other_key, _) = self::key();
        let wrong_key = sign_index(&other_key, INDEX, &listings).unwrap();
        assert_eq!(
            trusted.verify_index(INDEX, &wrong_key).unwrap_err(),
            SignatureError::SignatureMismatch
        );
    }

    #[test]
    fn refuses_modified_proofs_and_duplicate_or_unknown_fields() {
        let (key, trusted) = key();
        let sidecar = sign_index(&key, INDEX, &catalog()).unwrap();
        let mut proof: serde_json::Value = serde_json::from_slice(&sidecar).unwrap();
        let original_digest = proof["listings"]["notes"].as_str().unwrap().to_owned();
        proof["listings"]["notes"] = "00".repeat(32).into();
        assert_eq!(
            trusted
                .verify_index(INDEX, &serde_json::to_vec(&proof).unwrap())
                .unwrap_err(),
            SignatureError::SignatureMismatch
        );
        proof["schema"] = 2.into();
        assert_eq!(
            trusted
                .verify_index(INDEX, &serde_json::to_vec(&proof).unwrap())
                .unwrap_err(),
            SignatureError::UnsupportedSchema
        );
        let text = std::str::from_utf8(&sidecar).unwrap();
        let duplicate = text.replacen(
            "\"listings\":{",
            &format!("\"listings\":{{\"notes\":\"{original_digest}\","),
            1,
        );
        assert_eq!(
            trusted
                .verify_index(INDEX, duplicate.as_bytes())
                .unwrap_err(),
            SignatureError::InvalidIndexProof
        );
        let extra = format!("{{\"unknown\":true,{}", &text[1..]);
        assert_eq!(
            trusted.verify_index(INDEX, extra.as_bytes()).unwrap_err(),
            SignatureError::InvalidIndexProof
        );
        assert!(TrustedKey::from_hex("00z").is_err());
    }

    #[test]
    fn trust_provisioning_rejects_weak_and_noncanonical_public_points() {
        let zero = [0; 32];
        let mut identity = zero;
        identity[0] = 1;
        let mut order_two = [0xff; 32];
        order_two[0] = 0xec;
        let mut field_modulus = order_two;
        field_modulus[0] = 0xed;
        let order_eight =
            decode_hex("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a").unwrap();
        for point in [zero, identity, order_two, order_eight, field_modulus] {
            assert!(matches!(
                TrustedKey::from_hex(&encode_hex(&point)),
                Err(SignatureError::InvalidKey)
            ));
        }
    }

    #[test]
    fn package_claims_cannot_change_or_reuse_an_index_signature() {
        let (key, trusted) = key();
        let body = b"zip bytes";
        let digest = encode_hex(&Sha256::digest(body));
        let package = PackageIdentity {
            id: "notes",
            version: "1.0.0",
            arch: "armhf",
            sha256: &digest,
            size: body.len() as u64,
        };
        let signature = sign_package(&key, package).unwrap();
        assert_eq!(trusted.verify_package(package, &signature), Ok(()));

        let wrong_digest = "00".repeat(32);
        let changed = [
            PackageIdentity {
                id: "words",
                ..package
            },
            PackageIdentity {
                version: "1.0.1",
                ..package
            },
            PackageIdentity {
                arch: "armel",
                ..package
            },
            PackageIdentity {
                sha256: &wrong_digest,
                ..package
            },
            PackageIdentity {
                size: package.size + 1,
                ..package
            },
        ];
        for different in changed {
            assert_eq!(
                trusted.verify_package(different, &signature),
                Err(SignatureError::SignatureMismatch)
            );
        }
        let (_, wrong_key) = self::key();
        assert_eq!(
            wrong_key.verify_package(package, &signature),
            Err(SignatureError::SignatureMismatch)
        );

        let sidecar = sign_index(&key, INDEX, &catalog()).unwrap();
        let mut proof: IndexProof = serde_json::from_slice(&sidecar).unwrap();
        assert_eq!(
            trusted.verify_package(package, &proof.signature),
            Err(SignatureError::SignatureMismatch)
        );
        proof.signature = signature;
        assert_eq!(
            trusted
                .verify_index(INDEX, &serde_json::to_vec(&proof).unwrap())
                .unwrap_err(),
            SignatureError::SignatureMismatch
        );

        let path = std::env::temp_dir().join(format!("kl-signed-package-{}", std::process::id()));
        std::fs::write(&path, body).unwrap();
        assert_eq!(verify_file(&path, package.sha256, package.size), Ok(()));
        std::fs::write(&path, b"bad bytes").unwrap();
        assert!(matches!(
            verify_file(&path, package.sha256, package.size),
            Err(VerifyError::Hash { .. })
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn sidecar_urls_inherit_only_the_https_endpoint_and_query() {
        let asset = "https://github.com/release/file.zip?download=1&name=a%23b";
        assert_eq!(
            package_signature_url(asset).unwrap(),
            "https://github.com/release/file.zip.sig?download=1&name=a%23b"
        );
        assert_eq!(
            package_signature_url("https://github.com:443/file.zip").unwrap(),
            "https://github.com:443/file.zip.sig"
        );
        for invalid in [
            "file.zip",
            "http://github.com/file.zip",
            "https://github.com/file.zip#fragment",
            "https://github.com:8443/file.zip",
            "https://github.com:/file.zip",
            "https://username@github.com/file.zip",
            "https://github.com/release/",
            "https://github.com",
        ] {
            assert_eq!(
                package_signature_url(invalid),
                Err(SignatureError::InvalidAssetUrl),
                "{invalid}"
            );
        }
    }
}
