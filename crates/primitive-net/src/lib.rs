//! Downloads for PrimitiveOS apps. A download goes only to hosts on an allow list, over HTTPS; every redirect is
//! checked against the same list before it is followed (the client's own redirects are off); the size is limited, both
//! by the announced length and while reading; and progress is reported as the bytes arrive.

use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use ureq::http::Uri;

mod resume;
pub use resume::download_resumable;

/// What a download may reach.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Exact host names, lower case (`github.com`). A subdomain is not included.
    pub allowed_hosts: Vec<String>,
    pub max_bytes: u64,
    pub timeout: Duration,
    pub max_redirects: u32,
    /// Plain `http://` is refused unless this is set. Only tests set it (a local server has no certificate).
    pub allow_plain_http: bool,
}

impl Policy {
    /// The store's policy: GitHub's download hosts and the catalog's Pages site.
    pub fn store(max_bytes: u64) -> Policy {
        Policy {
            allowed_hosts: vec![
                "github.com".into(),
                "objects.githubusercontent.com".into(),
                // GitHub sends a release download here, on a signed link, after the github.com redirect.
                "release-assets.githubusercontent.com".into(),
                "raw.githubusercontent.com".into(),
                "primitiveos.github.io".into(),
                "doannguyentrong.github.io".into(),
            ],
            max_bytes,
            timeout: Duration::from_secs(120),
            max_redirects: 5,
            allow_plain_http: false,
        }
    }
}

#[derive(Debug)]
pub enum NetError {
    /// The URL or a redirect violates the policy. Contains only the host or a generic label.
    NotAllowed(String),
    TooLarge { limit: u64 },
    Status(u16),
    TooManyRedirects,
    Timeout,
    Transport(String),
    Io(std::io::Error),
    InvalidChecksum,
    ChecksumMismatch,
    UnexpectedLength { expected: u64, found: u64 },
    Incomplete { expected: u64, received: u64 },
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::NotAllowed(host) => write!(f, "the download URL is not allowed: {host}"),
            NetError::TooLarge { limit } => write!(f, "the download is larger than {limit} bytes"),
            NetError::Status(s) => write!(f, "the server answered with status {s}"),
            NetError::TooManyRedirects => write!(f, "too many redirects"),
            NetError::Timeout => write!(f, "the download took too long"),
            NetError::Transport(e) => write!(f, "the download failed: {e}"),
            NetError::Io(e) => write!(f, "cannot write the download: {e}"),
            NetError::InvalidChecksum => write!(f, "the catalog does not provide a valid SHA-256"),
            NetError::ChecksumMismatch => write!(f, "the download does not match its SHA-256 in the catalog"),
            NetError::UnexpectedLength { expected, found } => write!(f, "the server announced {found} bytes, expected {expected}"),
            NetError::Incomplete { expected, received } => write!(f, "the download stopped after {received} of {expected} bytes"),
        }
    }
}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        NetError::Io(e)
    }
}

/// The host of a URL, lower case, without user information or port. `None` for anything that is not `scheme://host…`.
pub fn host_of(url: &str) -> Option<(String, String)> {
    let uri = url.parse::<Uri>().ok()?;
    Some((uri.scheme_str()?.to_ascii_lowercase(), normalize_host(uri.host()?)))
}

fn normalize_host(host: &str) -> String {
    let literal = host.strip_prefix('[').and_then(|host| host.strip_suffix(']'));
    literal.filter(|host| host.parse::<std::net::Ipv6Addr>().is_ok()).unwrap_or(host).to_ascii_lowercase()
}

/// Whether `url` may be fetched under `policy`: exact approved host and no credentials.
/// Production HTTPS uses the default port. Tests can allow HTTP on a temporary port.
pub fn allowed(url: &str, policy: &Policy) -> bool {
    approved_uri(url, policy).is_some()
}

fn approved_uri(url: &str, policy: &Policy) -> Option<Uri> {
    let uri = url.parse::<Uri>().ok()?;
    let authority = uri.authority()?;
    let port = authority.port().filter(|port| port.as_str().bytes().all(|byte| byte.is_ascii_digit()));
    if authority.as_str().contains('@') || (authority.as_str() != authority.host() && port.is_none()) {
        return None;
    }
    let allowed_scheme = match uri.scheme_str()? {
        "https" => port.is_none_or(|port| port.as_u16() == 443),
        "http" => policy.allow_plain_http,
        _ => false,
    };
    (allowed_scheme && policy.allowed_hosts.contains(&normalize_host(authority.host()))).then_some(uri)
}

fn refused_url(url: &str) -> NetError {
    let host = host_of(url).map_or_else(|| "invalid URL".to_string(), |(_, host)| host);
    NetError::NotAllowed(host)
}

/// Resolves a redirect's `Location` against the URL that produced it.
fn resolve(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    if let Some(rest) = location.strip_prefix("//") {
        let scheme = base.split_once("://").map_or("https", |(s, _)| s);
        return format!("{scheme}://{rest}");
    }
    let origin_end = base.split_once("://").and_then(|(_, r)| r.find('/').map(|i| base.len() - r.len() + i));
    let origin = match origin_end {
        Some(i) => &base[..i],
        None => base,
    };
    if location.starts_with('/') {
        format!("{origin}{location}")
    } else {
        let dir_end = base.rfind('/').filter(|i| *i > origin.len()).unwrap_or(origin.len());
        format!("{}/{location}", &base[..dir_end])
    }
}

/// The result of a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub bytes: u64,
    /// The last URL reached (after the redirects).
    pub final_url: String,
}

fn agent(policy: &Policy) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(policy.timeout))
        .max_redirects(0)
        .http_status_as_error(false)
        .user_agent("primitive-net/0.1")
        .build();
    ureq::Agent::new_with_config(config)
}

/// Opens `url`, following redirects by hand so each target is checked. Returns the final response's status, its URL and
/// its body reader source.
fn open(url: &str, policy: &Policy) -> Result<(String, ureq::http::Response<ureq::Body>), NetError> {
    successful(request(url, policy, None)?)
}

fn successful(
    (final_url, resp): (String, ureq::http::Response<ureq::Body>),
) -> Result<(String, ureq::http::Response<ureq::Body>), NetError> {
    if resp.status().as_u16() != 200 {
        return Err(NetError::Status(resp.status().as_u16()));
    }
    Ok((final_url, resp))
}

fn request(url: &str, policy: &Policy, verified_offset: Option<u64>) -> Result<(String, ureq::http::Response<ureq::Body>), NetError> {
    request_with_agent(url, policy, verified_offset, &agent(policy), None)
}

fn request_with_agent(
    url: &str,
    policy: &Policy,
    verified_offset: Option<u64>,
    agent: &ureq::Agent,
    started: Option<Instant>,
) -> Result<(String, ureq::http::Response<ureq::Body>), NetError> {
    let mut current = url.to_string();
    for _ in 0..=policy.max_redirects {
        let uri = approved_uri(&current, policy).ok_or_else(|| refused_url(&current))?;
        let mut request = agent.get(uri);
        if let Some(offset) = verified_offset {
            request = request.header("accept-encoding", "identity");
            if offset > 0 {
                request = request.header("range", format!("bytes={offset}-"));
            }
        }
        if let Some(started) = started {
            let remaining = policy.timeout.checked_sub(started.elapsed()).ok_or(NetError::Timeout)?;
            if remaining.is_zero() {
                return Err(NetError::Timeout);
            }
            request = request.config().timeout_global(Some(remaining)).build();
        }
        let resp = request.call().map_err(transport_error)?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .ok_or(NetError::Status(status))?;
            current = resolve(&current, &location);
            continue;
        }
        return Ok((current, resp));
    }
    Err(NetError::TooManyRedirects)
}

/// Downloads `url` to `dest`, through a `.part` file that is renamed when complete. `progress` gets the bytes so far and
/// the announced total when there is one.
pub fn download(
    url: &str,
    dest: &Path,
    policy: &Policy,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<Download, NetError> {
    let (final_url, resp) = open(url, policy)?;
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if total.is_some_and(|t| t > policy.max_bytes) {
        return Err(NetError::TooLarge { limit: policy.max_bytes });
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension("part");
    let result = (|| {
        let mut out = File::create(&part)?;
        let mut body = resp.into_body();
        let limit = policy.max_bytes;
        let mut reader = body.with_config().limit(limit).reader();
        let mut buf = [0u8; 32 * 1024];
        let mut bytes = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|error| body_error(error, limit))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            bytes += n as u64;
            progress(bytes, total);
        }
        out.sync_all()?;
        drop(out);
        fs::rename(&part, dest)?;
        Ok(Download { bytes, final_url })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn body_error(error: std::io::Error, limit: u64) -> NetError {
    match ureq::Error::from(error) {
        ureq::Error::BodyExceedsLimit(_) => NetError::TooLarge { limit },
        error => transport_error(error),
    }
}

fn transport_error(error: ureq::Error) -> NetError {
    // Client and response errors may include the request URL or remote text. Log only known details.
    match error {
        ureq::Error::Timeout(_) => NetError::Timeout,
        ureq::Error::Io(error) if error.kind() == std::io::ErrorKind::TimedOut => NetError::Timeout,
        ureq::Error::Io(error) => NetError::Transport(error.kind().to_string()),
        ureq::Error::HostNotFound => NetError::Transport("host not found".into()),
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) | ureq::Error::TlsRequired => {
            NetError::Transport("could not establish a secure connection".into())
        }
        _ => NetError::Transport("could not read a response from the server".into()),
    }
}

/// Reads a small text document (a catalog index, a listing) under the same policy, with the same size limit.
pub fn fetch_text(url: &str, policy: &Policy) -> Result<String, NetError> {
    let (_, resp) = open(url, policy)?;
    read_text(resp, policy.max_bytes)
}

/// Reads a file (an icon, a screenshot) into memory under the same policy and size limit.
pub fn fetch_bytes(url: &str, policy: &Policy) -> Result<Vec<u8>, NetError> {
    let (_, resp) = open(url, policy)?;
    read_bytes(resp, policy.max_bytes)
}

/// Reuses HTTPS connections for small responses with one timeout for each fetch, including redirects and the body.
pub struct FetchClient {
    policy: Policy,
    agent: ureq::Agent,
}

impl FetchClient {
    pub fn new(policy: Policy) -> Self {
        let agent = agent(&policy);
        Self { policy, agent }
    }

    /// Reads UTF-8 text within both the requested limit and the policy ceiling.
    pub fn fetch_text(&self, url: &str, max_bytes: u64) -> Result<String, NetError> {
        read_text(self.open(url)?, self.policy.max_bytes.min(max_bytes))
    }

    /// Reads bytes within both the requested limit and the policy ceiling.
    pub fn fetch_bytes(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, NetError> {
        read_bytes(self.open(url)?, self.policy.max_bytes.min(max_bytes))
    }

    fn open(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, NetError> {
        let (_, resp) = successful(request_with_agent(url, &self.policy, None, &self.agent, Some(Instant::now()))?)?;
        Ok(resp)
    }
}

fn read_text(resp: ureq::http::Response<ureq::Body>, limit: u64) -> Result<String, NetError> {
    let mut text = String::new();
    let mut body = resp.into_body();
    let mut reader = body.with_config().limit(limit).reader();
    reader.read_to_string(&mut text).map_err(|error| body_error(error, limit))?;
    Ok(text)
}

fn read_bytes(resp: ureq::http::Response<ureq::Body>, limit: u64) -> Result<Vec<u8>, NetError> {
    let mut body = resp.into_body();
    let mut reader = body.with_config().limit(limit).reader();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|error| body_error(error, limit))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn reads_the_host_of_a_url() {
        assert_eq!(host_of("https://GitHub.com/a/b").unwrap(), ("https".into(), "github.com".into()));
        assert_eq!(host_of("https://user@github.com:443/x").unwrap().1, "github.com");
        assert_eq!(host_of("https://github.com.evil.example/x").unwrap().1, "github.com.evil.example");
        assert_eq!(host_of("https://[::1]:8443/x").unwrap().1, "::1");
        assert!(host_of("github.com/x").is_none());
        assert!(host_of("https:///x").is_none());
    }

    #[test]
    fn allows_only_the_list_over_https() {
        let p = Policy::store(1000);
        assert!(allowed("https://github.com/x/releases/download/v1/a.zip", &p));
        assert!(allowed("https://objects.githubusercontent.com/a", &p));
        assert!(allowed("https://release-assets.githubusercontent.com/github-production-release-asset/1/x?sig=y", &p));
        assert!(allowed("https://primitiveos.github.io/kindle-primitive-catalog/v1/index.json", &p));
        assert!(!allowed("https://primitiveos.github.io.evil.example/index.json", &p));
        assert!(!allowed("https://github.com.evil.example/a", &p), "a look-alike host is refused");
        assert!(!allowed("https://evil.example/github.com", &p));
        assert!(!allowed("https://sub.github.com/a", &p), "a subdomain is not on the list");
        assert!(!allowed("https://github.com@evil.example/a", &p), "user information does not change the host");
        assert!(!allowed("http://github.com/a", &p), "plain HTTP is refused");
    }

    #[test]
    fn production_urls_require_the_default_https_port_and_no_userinfo() {
        let policy = Policy::store(1000);
        assert!(allowed("https://GitHub.com:443/files", &policy));
        for url in [
            "https://github.com:8443/files",
            "https://github.com:missing/files",
            "https://github.com:65536/files",
            "https://github.com:+443/files",
            "https://[github.com]/files",
            "https://reviewer@github.com/files",
            "https://reviewer:fixture-password@github.com/files",
        ] {
            assert!(!allowed(url, &policy));
        }
        assert!(allowed("http://127.0.0.1:12345/file", &local_policy()));
        assert!(!allowed("http://reviewer@127.0.0.1:12345/file", &local_policy()));
    }

    #[test]
    fn resolves_redirect_locations() {
        assert_eq!(resolve("https://github.com/a/b", "https://objects.githubusercontent.com/x"), "https://objects.githubusercontent.com/x");
        assert_eq!(resolve("https://github.com/a/b", "/c/d"), "https://github.com/c/d");
        assert_eq!(resolve("https://github.com/a/b", "c"), "https://github.com/a/c");
        assert_eq!(resolve("https://github.com/a/b", "//raw.githubusercontent.com/y"), "https://raw.githubusercontent.com/y");
    }

    /// A tiny HTTP server on localhost for the download tests: serves `/file`, `/big` (announced too large) and
    /// `/redirect-out` (a redirect to `localhost`, which is not on the test list).
    fn server() -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(8).flatten() {
                let mut stream = stream;
                let mut line = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let _ = reader.read_line(&mut line);
                let mut header = String::new();
                loop {
                    header.clear();
                    if reader.read_line(&mut header).unwrap_or(0) <= 2 {
                        break;
                    }
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = b"hello from the test server, twelve bytes or more".to_vec();
                let response = match path.as_str() {
                    "/file" => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes()
                        .into_iter().chain(body.clone()).collect::<Vec<u8>>(),
                    "/big" => b"HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\nConnection: close\r\n\r\n".to_vec(),
                    "/redirect-out" => b"HTTP/1.1 302 Found\r\nLocation: http://localhost:1/elsewhere?sig=fixture-signature\r\nConnection: close\r\n\r\n".to_vec(),
                    _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                };
                let _ = stream.write_all(&response);
            }
        });
        (format!("http://{addr}"), handle)
    }

    fn local_policy() -> Policy {
        Policy { allowed_hosts: vec!["127.0.0.1".into()], allow_plain_http: true, ..Policy::store(1000) }
    }

    #[test]
    fn downloads_a_file_with_progress_and_renames_it_into_place() {
        let (base, handle) = server();
        let dir = std::env::temp_dir().join(format!("pn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let dest = dir.join("file.zip");
        let mut seen = 0u64;
        let got = download(&format!("{base}/file"), &dest, &local_policy(), |n, _| seen = n).unwrap();
        let expected = b"hello from the test server, twelve bytes or more".len() as u64;
        assert_eq!(got.bytes, expected);
        assert_eq!(seen, expected);
        assert_eq!(fs::read(&dest).unwrap().len() as u64, expected);
        assert!(!dir.join("file.part").exists());
        let _ = fs::remove_dir_all(dir);
        drop(handle);
    }

    #[test]
    fn refuses_a_large_announced_size_before_reading() {
        let (base, _h) = server();
        let dir = std::env::temp_dir().join(format!("pn-big-{}", std::process::id()));
        let r = download(&format!("{base}/big"), &dir.join("x"), &local_policy(), |_, _| {});
        assert!(matches!(r, Err(NetError::TooLarge { .. })), "{r:?}");
        assert!(!dir.join("x").exists());
    }

    #[test]
    fn refuses_a_redirect_to_a_host_not_on_the_list() {
        let (base, _h) = server();
        let dir = std::env::temp_dir().join(format!("pn-redir-{}", std::process::id()));
        let error = download(&format!("{base}/redirect-out"), &dir.join("x"), &local_policy(), |_, _| {}).unwrap_err();
        assert!(matches!(&error, NetError::NotAllowed(host) if host == "localhost"));
        assert!(!format!("{error} {error:?}").contains("fixture-signature"));
    }

    #[test]
    fn fetches_bytes_from_the_local_server() {
        let (base, _h) = server();
        let got = fetch_bytes(&format!("{base}/file"), &local_policy()).unwrap();
        assert_eq!(got, b"hello from the test server, twelve bytes or more".to_vec());
        let too_big = Policy { max_bytes: 10, ..local_policy() };
        assert!(matches!(fetch_bytes(&format!("{base}/file"), &too_big), Err(NetError::TooLarge { .. })));
    }

    #[test]
    fn small_responses_reuse_a_connection_and_apply_the_per_fetch_limit() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut reader = BufReader::new(stream);
            for _ in 0..2 {
                let mut header = String::new();
                assert!(reader.read_line(&mut header).unwrap() > 2);
                while header != "\r\n" {
                    header.clear();
                    assert_ne!(reader.read_line(&mut header).unwrap(), 0);
                }
                reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ntiny").unwrap();
            }
        });
        let url = format!("http://{addr}/file");
        let client = FetchClient::new(Policy { timeout: Duration::from_secs(3), ..local_policy() });
        assert_eq!(client.fetch_text(&url, 4).unwrap(), "tiny");
        assert!(matches!(client.fetch_bytes(&url, 3), Err(NetError::TooLarge { limit: 3 })));
        server.join().unwrap();
    }

    #[test]
    fn small_responses_keep_the_policy_ceiling_and_redirect_rules() {
        let (base, _h) = server();
        let client = FetchClient::new(Policy { max_bytes: 10, ..local_policy() });
        assert!(matches!(client.fetch_bytes(&format!("{base}/file"), 100), Err(NetError::TooLarge { limit: 10 })));
        assert!(matches!(client.fetch_bytes(&format!("{base}/redirect-out"), 100), Err(NetError::NotAllowed(host)) if host == "localhost"));

        let client = FetchClient::new(Policy { max_redirects: 0, ..local_policy() });
        assert!(matches!(client.fetch_bytes(&format!("{base}/redirect-out"), 100), Err(NetError::TooManyRedirects)));
    }

    #[test]
    fn refuses_a_url_outside_the_list_without_connecting() {
        let url = "https://fixture-user:fixture-password@github.com/file?token=fixture-token";
        let policy = Policy::store(100);
        let fetch_error = fetch_text(url, &policy).unwrap_err();
        let dest = std::env::temp_dir().join("unused-store-network-fixture.zip");
        let resume_error = download_resumable(url, &dest, &policy, 5, &"a".repeat(64), |_, _| {}).unwrap_err();
        for error in [fetch_error, resume_error] {
            assert!(matches!(&error, NetError::NotAllowed(host) if host == "github.com"));
            assert!(!format!("{error} {error:?}").contains("fixture-"));
        }
    }

    #[test]
    fn transport_errors_omit_details_from_the_client_and_response() {
        let uri = "https://release-assets.githubusercontent.com/file?sig=fixture-signature";
        let request_error = transport_error(ureq::Error::BadUri(uri.into()));
        let response_error = body_error(std::io::Error::new(std::io::ErrorKind::InvalidData, uri), 100);
        for error in [request_error, response_error] {
            assert!(matches!(error, NetError::Transport(_)));
            assert!(!format!("{error} {error:?}").contains("fixture-signature"));
        }
    }
}
