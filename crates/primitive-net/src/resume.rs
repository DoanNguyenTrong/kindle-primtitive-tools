//! Pinned downloads may keep partial bytes after an interrupted request and resume on the next attempt.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
use ureq::{http::Response, Body};

use super::{allowed, body_error, refused_url, request, Download, NetError, Policy};

/// Downloads a package of the exact catalog size and SHA-256. On a transient failure the partial
/// bytes are available for the next call. The destination is replaced only after verification.
pub fn download_resumable(
    url: &str,
    dest: &Path,
    policy: &Policy,
    expected_size: u64,
    expected_sha256: &str,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<Download, NetError> {
    if !allowed(url, policy) {
        return Err(refused_url(url));
    }
    if expected_size > policy.max_bytes {
        return Err(NetError::TooLarge {
            limit: policy.max_bytes,
        });
    }
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(NetError::InvalidChecksum);
    }

    let parent = dest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    // Packages share the download directory. Its lock serializes updates to the partial and
    // identity files without creating a lock file that could outlive a crashed installer.
    let lock = File::open(parent)?;
    lock.try_lock().map_err(io::Error::from)?;

    let part = dest.with_extension("part");
    let identity_path = dest.with_extension("part-id");
    let identity = format!(
        "{expected_size}\n{}\n",
        expected_sha256.to_ascii_lowercase()
    );
    let mut out = open_partial(&part, &identity_path, identity.as_bytes(), expected_size)?;

    let result = transfer(
        url,
        policy,
        &mut out,
        expected_size,
        expected_sha256,
        &mut progress,
    );
    match result {
        Ok(final_url) => {
            out.sync_all()?;
            drop(out);
            fs::rename(&part, dest)?;
            remove_file(&identity_path)?;
            Ok(Download {
                bytes: expected_size,
                final_url,
            })
        }
        Err(error) if can_retry(&error) => {
            out.sync_all()?;
            Err(error)
        }
        Err(error) => {
            drop(out);
            remove_file(&part)?;
            remove_file(&identity_path)?;
            Err(error)
        }
    }
}

fn can_retry(error: &NetError) -> bool {
    matches!(
        error,
        NetError::Timeout
            | NetError::Transport(_)
            | NetError::Incomplete { .. }
            | NetError::Status(408 | 425 | 429 | 500..=599)
    )
}

fn file_length(path: &Path) -> io::Result<Option<u64>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "download state is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn open_partial(part: &Path, identity_path: &Path, identity: &[u8], size: u64) -> io::Result<File> {
    let can_resume = file_length(part)?.is_some_and(|len| len <= size)
        && file_length(identity_path)? == Some(identity.len() as u64);
    if can_resume {
        let mut saved = Vec::new();
        File::open(identity_path)?
            .take(identity.len() as u64 + 1)
            .read_to_end(&mut saved)?;
        if saved == identity {
            return File::options().read(true).write(true).open(part);
        }
    }

    remove_file(part)?;
    remove_file(identity_path)?;
    let mut marker = File::create_new(identity_path)?;
    marker.write_all(identity)?;
    marker.sync_all()?;
    File::options()
        .create_new(true)
        .read(true)
        .write(true)
        .open(part)
}

fn transfer(
    url: &str,
    policy: &Policy,
    out: &mut File,
    size: u64,
    sha256: &str,
    progress: &mut impl FnMut(u64, Option<u64>),
) -> Result<String, NetError> {
    let mut offset = out.metadata()?.len();
    if offset == size && offset > 0 {
        // A previous attempt may have stopped after receiving its last byte. Verify those bytes
        // locally before requesting anything else.
        let mut hasher = Sha256::new();
        hash_prefix(out, offset, &mut hasher)?;
        if matches_checksum(hasher, sha256) {
            progress(size, Some(size));
            return Ok(url.to_string());
        }
        out.set_len(0)?;
        offset = 0;
    }

    let (mut final_url, mut resp) = request(url, policy, Some(offset))?;
    let status = resp.status().as_u16();
    if offset > 0
        && (matches!(status, 400 | 416) || (status == 206 && !valid_range(&resp, offset, size)))
    {
        // A rejected or inconsistent range can never be appended. Ask for a full response once.
        (final_url, resp) = request(url, policy, Some(0))?;
        offset = 0;
    }
    let offset = match resp.status().as_u16() {
        200 => 0,
        206 if offset > 0 => offset,
        status => return Err(NetError::Status(status)),
    };
    match receive(resp, out, offset, size, sha256, progress) {
        Err(NetError::ChecksumMismatch) if offset > 0 => {
            // The source may have changed between requests. Discard the assembled bytes and try
            // one full download. The catalog checksum still applies to that response.
            out.set_len(0)?;
            let (final_url, resp) = request(url, policy, Some(0))?;
            if resp.status().as_u16() != 200 {
                return Err(NetError::Status(resp.status().as_u16()));
            }
            receive(resp, out, 0, size, sha256, progress)?;
            Ok(final_url)
        }
        result => result.map(|()| final_url),
    }
}

fn valid_range(resp: &Response<Body>, offset: u64, size: u64) -> bool {
    if is_encoded(resp) || announced_length(resp).is_some_and(|length| length != size - offset) {
        return false;
    }
    let ranges = resp.headers().get_all("content-range");
    let mut ranges = ranges.iter();
    let Some(header) = ranges.next().and_then(|header| header.to_str().ok()) else {
        return false;
    };
    if ranges.next().is_some() {
        return false;
    }
    let Some((span, total)) = header
        .strip_prefix("bytes ")
        .and_then(|range| range.split_once('/'))
    else {
        return false;
    };
    let Some((start, end)) = span.split_once('-') else {
        return false;
    };
    start.parse::<u64>().ok() == Some(offset)
        && end.parse::<u64>().ok() == size.checked_sub(1)
        && total.parse::<u64>().ok() == Some(size)
}

fn is_encoded(resp: &Response<Body>) -> bool {
    resp.headers()
        .get_all("content-encoding")
        .iter()
        .any(|header| !header.as_bytes().eq_ignore_ascii_case(b"identity"))
}

fn announced_length(resp: &Response<Body>) -> Option<u64> {
    resp.headers()
        .get("content-length")
        .and_then(|header| header.to_str().ok())
        .and_then(|length| length.parse().ok())
}

fn receive(
    resp: Response<Body>,
    out: &mut File,
    offset: u64,
    size: u64,
    sha256: &str,
    progress: &mut impl FnMut(u64, Option<u64>),
) -> Result<(), NetError> {
    if is_encoded(&resp) {
        return Err(NetError::Transport(
            "the server sent an encoded file".into(),
        ));
    }
    let remaining = size - offset;
    if let Some(length) = announced_length(&resp) {
        if length != remaining {
            return Err(NetError::UnexpectedLength {
                expected: remaining,
                found: length,
            });
        }
    }

    let mut hasher = Sha256::new();
    if offset == 0 {
        out.set_len(0)?;
        out.seek(SeekFrom::Start(0))?;
    } else {
        hash_prefix(out, offset, &mut hasher)?;
    }
    let mut bytes = offset;
    progress(bytes, Some(size));
    let mut body = resp.into_body();
    let mut reader = body
        .with_config()
        .limit(remaining.saturating_add(1))
        .reader();
    let mut buffer = [0; 32 * 1024];
    loop {
        let capacity = (size - bytes).saturating_add(1).min(buffer.len() as u64) as usize;
        let read = reader
            .read(&mut buffer[..capacity])
            .map_err(|error| body_error(error, size))?;
        if read == 0 {
            break;
        }
        if read as u64 > size - bytes {
            return Err(NetError::TooLarge { limit: size });
        }
        out.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        bytes += read as u64;
        progress(bytes, Some(size));
    }
    if bytes != size {
        return Err(NetError::Incomplete {
            expected: size,
            received: bytes,
        });
    }
    if !matches_checksum(hasher, sha256) {
        return Err(NetError::ChecksumMismatch);
    }
    Ok(())
}

fn hash_prefix(file: &mut File, mut bytes: u64, hasher: &mut Sha256) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut buffer = [0; 32 * 1024];
    while bytes > 0 {
        let size = bytes.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..size])?;
        hasher.update(&buffer[..size]);
        bytes -= size as u64;
    }
    Ok(())
}

fn matches_checksum(hasher: Sha256, sha256: &str) -> bool {
    hasher.finalize().iter().enumerate().all(|(index, byte)| {
        let digits = &sha256.as_bytes()[index * 2..index * 2 + 2];
        let hex = b"0123456789abcdef";
        digits[0].eq_ignore_ascii_case(&hex[usize::from(byte >> 4)])
            && digits[1].eq_ignore_ascii_case(&hex[usize::from(byte & 15)])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    const PACKAGE: &[u8] = b"verified package bytes for resumable downloads";
    const OTHER_PACKAGE: &[u8] = b"updated package bytes for resumable downloads!";
    const CUT: usize = 13;
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("primitive-resume-{}-{number}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path.join("app.zip"))
        }

        fn partial(&self) -> Vec<u8> {
            fs::read(self.0.with_extension("part")).unwrap()
        }

        fn present(&self) -> bool {
            self.0.with_extension("part").exists() || self.0.with_extension("part-id").exists()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    fn local_policy() -> Policy {
        Policy {
            allowed_hosts: vec!["127.0.0.1".into()],
            timeout: Duration::from_secs(2),
            allow_plain_http: true,
            ..Policy::store(1024)
        }
    }

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        [
            format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").as_bytes(),
            body,
        ]
        .concat()
    }

    fn full(body: &[u8]) -> Vec<u8> {
        response(
            "200 OK",
            &format!("Content-Length: {}\r\n", body.len()),
            body,
        )
    }

    fn interrupted(body: &[u8]) -> Vec<u8> {
        response(
            "200 OK",
            &format!("Content-Length: {}\r\n", body.len()),
            &body[..CUT],
        )
    }

    fn partial(start: usize, body: &[u8]) -> Vec<u8> {
        response(
            "206 Partial Content",
            &format!(
                "Content-Length: {}\r\nContent-Range: bytes {start}-{}/{}\r\n",
                body.len() - start,
                body.len() - 1,
                body.len()
            ),
            &body[start..],
        )
    }

    fn server(replies: Vec<Vec<u8>>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/app.zip", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            replies
                .into_iter()
                .map(|reply| {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = String::new();
                    {
                        let mut reader = BufReader::new(&mut stream);
                        let mut line = String::new();
                        loop {
                            line.clear();
                            let n = reader.read_line(&mut line).unwrap();
                            assert!(n > 0);
                            if line == "\r\n" {
                                break;
                            }
                            request.push_str(&line);
                        }
                    }
                    stream.write_all(&reply).unwrap();
                    request.to_ascii_lowercase()
                })
                .collect()
        });
        (url, task)
    }

    fn retry(
        url: &str,
        fixture: &Fixture,
        body: &[u8],
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<Download, NetError> {
        download_resumable(
            url,
            &fixture.0,
            &local_policy(),
            body.len() as u64,
            &sha256(body),
            progress,
        )
    }

    fn start_interrupted(url: &str, fixture: &Fixture) {
        let error = retry(url, fixture, PACKAGE, |_, _| {}).unwrap_err();
        assert!(can_retry(&error), "{error:?}");
        assert_eq!(fixture.partial(), PACKAGE[..CUT]);
    }

    fn requested_range(request: &str, offset: usize) -> bool {
        request
            .lines()
            .any(|header| header == format!("range: bytes={offset}-"))
    }

    #[test]
    fn an_interrupted_download_resumes_and_reports_progress_from_the_saved_offset() {
        let fixture = Fixture::new();
        let (url, task) = server(vec![interrupted(PACKAGE), partial(CUT, PACKAGE)]);
        start_interrupted(&url, &fixture);
        assert!(!fixture.0.exists());
        let mut offsets = Vec::new();
        let got = retry(&url, &fixture, PACKAGE, |offset, total| {
            offsets.push((offset, total))
        })
        .unwrap();
        assert_eq!(got.bytes, PACKAGE.len() as u64);
        assert_eq!(offsets.first(), Some(&(CUT as u64, Some(got.bytes))));
        assert_eq!(offsets.last(), Some(&(got.bytes, Some(got.bytes))));
        let requests = task.join().unwrap();
        assert!(!requests[0].contains("\nrange:"));
        assert!(requested_range(&requests[1], CUT));
        assert!(requests[1].contains("\naccept-encoding: identity"));
        assert_eq!(fs::read(&fixture.0).unwrap(), PACKAGE);
        assert!(!fixture.present());
    }

    #[test]
    fn a_full_response_to_a_range_request_replaces_the_saved_prefix() {
        let fixture = Fixture::new();
        let (url, task) = server(vec![interrupted(PACKAGE), full(PACKAGE)]);
        start_interrupted(&url, &fixture);
        let mut offsets = Vec::new();
        retry(&url, &fixture, PACKAGE, |offset, _| offsets.push(offset)).unwrap();
        assert_eq!(offsets.first(), Some(&0));
        assert!(requested_range(&task.join().unwrap()[1], CUT));
        assert_eq!(fs::read(&fixture.0).unwrap(), PACKAGE);
    }

    #[test]
    fn a_refused_or_inconsistent_range_gets_one_full_retry() {
        let conflicting_length = format!(
            "Content-Range: bytes {CUT}-{}/{}\r\nContent-Length: 1\r\n",
            PACKAGE.len() - 1,
            PACKAGE.len(),
        );
        for rejected in [
            response("416 Range Not Satisfiable", "Content-Length: 0\r\n", b""),
            partial(CUT + 1, PACKAGE),
            response("206 Partial Content", &conflicting_length, b"x"),
        ] {
            let fixture = Fixture::new();
            let (url, task) = server(vec![interrupted(PACKAGE), rejected, full(PACKAGE)]);
            start_interrupted(&url, &fixture);
            retry(&url, &fixture, PACKAGE, |_, _| {}).unwrap();
            let requests = task.join().unwrap();
            assert!(requested_range(&requests[1], CUT));
            assert!(!requests[2].contains("\nrange:"));
            assert_eq!(fs::read(&fixture.0).unwrap(), PACKAGE);
        }
    }

    #[test]
    fn a_changed_suffix_cannot_be_published_even_with_a_valid_range() {
        let fixture = Fixture::new();
        let mut wrong = PACKAGE.to_vec();
        wrong[CUT] ^= 1;
        let (url, task) = server(vec![
            interrupted(PACKAGE),
            partial(CUT, &wrong),
            full(PACKAGE),
        ]);
        start_interrupted(&url, &fixture);
        retry(&url, &fixture, PACKAGE, |_, _| {}).unwrap();
        let requests = task.join().unwrap();
        assert!(requested_range(&requests[1], CUT));
        assert!(!requests[2].contains("\nrange:"));
        assert_eq!(fs::read(&fixture.0).unwrap(), PACKAGE);
    }

    #[test]
    fn a_different_catalog_digest_starts_a_full_request() {
        assert_eq!(PACKAGE.len(), OTHER_PACKAGE.len());
        let fixture = Fixture::new();
        let (url, task) = server(vec![interrupted(PACKAGE), full(OTHER_PACKAGE)]);
        start_interrupted(&url, &fixture);
        retry(&url, &fixture, OTHER_PACKAGE, |_, _| {}).unwrap();
        assert!(!task.join().unwrap()[1].contains("\nrange:"));
        assert_eq!(fs::read(&fixture.0).unwrap(), OTHER_PACKAGE);
        assert!(!fixture.present());
    }

    #[test]
    fn a_bad_complete_hash_leaves_the_existing_destination_and_discards_temporary_bytes() {
        let fixture = Fixture::new();
        fs::write(&fixture.0, b"existing verified file").unwrap();
        let (url, task) = server(vec![full(OTHER_PACKAGE)]);
        let error = retry(&url, &fixture, PACKAGE, |_, _| {}).unwrap_err();
        assert!(matches!(error, NetError::ChecksumMismatch));
        task.join().unwrap();
        assert_eq!(fs::read(&fixture.0).unwrap(), b"existing verified file");
        assert!(!fixture.present());
    }

    #[test]
    fn a_resumed_request_cannot_follow_an_unapproved_redirect() {
        let fixture = Fixture::new();
        let (url, task) = server(vec![
            interrupted(PACKAGE),
            response(
                "302 Found",
                "Location: http://localhost:1/app.zip\r\nContent-Length: 0\r\n",
                b"",
            ),
        ]);
        start_interrupted(&url, &fixture);
        let error = retry(&url, &fixture, PACKAGE, |_, _| {}).unwrap_err();
        assert!(matches!(error, NetError::NotAllowed(_)));
        assert!(requested_range(&task.join().unwrap()[1], CUT));
        assert!(!fixture.0.exists());
    }

    #[test]
    fn a_full_partial_is_verified_and_published_without_a_new_network_request() {
        let fixture = Fixture::new();
        let identity = format!("{}\n{}\n", PACKAGE.len(), sha256(PACKAGE));
        fs::write(fixture.0.with_extension("part"), PACKAGE).unwrap();
        fs::write(fixture.0.with_extension("part-id"), identity).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/app.zip", listener.local_addr().unwrap());
        retry(&url, &fixture, PACKAGE, |_, _| {}).unwrap();
        assert_eq!(fs::read(&fixture.0).unwrap(), PACKAGE);
        assert!(!fixture.present());
    }

    #[test]
    fn concurrent_downloads_cannot_change_the_same_partial() {
        let fixture = Fixture::new();
        let lock = File::open(fixture.0.parent().unwrap()).unwrap();
        lock.try_lock().unwrap();
        let error = retry("http://127.0.0.1:1/app.zip", &fixture, PACKAGE, |_, _| {}).unwrap_err();
        assert!(matches!(error, NetError::Io(error) if error.kind() == io::ErrorKind::WouldBlock));
        assert!(!fixture.present());
    }
}
