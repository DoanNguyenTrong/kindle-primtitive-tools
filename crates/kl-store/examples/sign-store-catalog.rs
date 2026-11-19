//! Sign prepared local Store artifacts with an independently managed Ed25519 key.

use kl_store::{
    catalog::{valid_app_id, Index, Listing},
    refresh::{MAX_INDEX, MAX_LISTING},
    signatures::{self, PackageIdentity, INDEX_SIGNATURE_FILENAME},
    unpack, verify,
};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1).collect()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<OsString>) -> Result<()> {
    let [command, key, path] = args.as_slice() else {
        return Err("usage: sign-store-catalog <keygen|index|package> PRIVATE_KEY PATH".into());
    };
    let (key, path) = (Path::new(key), Path::new(path));
    match command.to_str() {
        Some("keygen") => keygen(key, path),
        Some("index") => sign_site(&read_key(key)?, path),
        Some("package") => sign_archive(&read_key(key)?, path),
        _ => Err("expected keygen, index, or package".into()),
    }
}

fn read_bounded(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "signing input exceeds its limit",
        ));
    }
    Ok(bytes)
}

fn read_key(path: &Path) -> Result<Ed25519KeyPair> {
    let bytes = read_bounded(path, 1024)?;
    Ed25519KeyPair::from_pkcs8(&bytes).map_err(|_| "invalid Ed25519 PKCS#8 key".into())
}

fn keygen(private: &Path, public: &Path) -> Result<()> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "could not generate an Ed25519 key")?;
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "invalid generated key")?;
    let trusted = signatures::encode_hex(key.public_key().as_ref()) + "\n";
    write_new(private, pkcs8.as_ref(), true)?;
    if let Err(error) = write_new(public, trusted.as_bytes(), false) {
        fs::remove_file(private)?;
        return Err(error.into());
    }
    Ok(())
}

fn sign_site(key: &Ed25519KeyPair, site: &Path) -> Result<()> {
    let index_json = read_bounded(&site.join("index.json"), MAX_INDEX)?;
    let index: Index = serde_json::from_slice(&index_json)?;
    if index.schema != 1 {
        return Err("the catalog index must use schema 1".into());
    }
    let mut listings = BTreeMap::new();
    for card in index.apps {
        let id = card.id;
        if !valid_app_id(&id) || listings.contains_key(&id) {
            return Err("the index contains an invalid or duplicate app ID".into());
        }
        let bytes = read_bounded(&site.join("apps").join(format!("{id}.json")), MAX_LISTING)?;
        let listing: Listing = serde_json::from_slice(&bytes)?;
        if listing.schema != 1 || listing.id != id {
            return Err(format!("the listing for {id} is invalid").into());
        }
        listings.insert(id, bytes);
    }
    let proof = signatures::sign_index(key, &index_json, &listings)?;
    write_new(&site.join(INDEX_SIGNATURE_FILENAME), &proof, false)?;
    Ok(())
}

fn sign_archive(key: &Ed25519KeyPair, path: &Path) -> Result<()> {
    if path.extension().is_none_or(|extension| extension != "zip") {
        return Err("expected a .zip package archive".into());
    }
    verify::check_archive_size(path.metadata()?.len()).map_err(|error| error.to_string())?;
    let manifest = unpack::read_manifest(path).map_err(|error| error.to_string())?;
    let (sha256, size) = verify::sha256_file(path)?;
    let package = PackageIdentity {
        id: &manifest.id,
        version: &manifest.version,
        arch: &manifest.arch,
        sha256: &sha256,
        size,
    };
    let proof = signatures::sign_package(key, package)? + "\n";
    write_new(&path.with_extension("zip.sig"), proof.as_bytes(), false)?;
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if private { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    if private {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "use a Unix host to generate a signing key",
        ));
    }
    let mut file = options.open(path)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}
