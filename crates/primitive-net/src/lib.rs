//! Downloads for the Primitive apps. A download goes only to hosts on an allow list, over HTTPS; every redirect is
//! checked against the same list before it is followed (the client's own redirects are off); the size is limited, both
//! by the announced length and while reading; and progress is reported as the bytes arrive.

use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

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
    /// The URL or a redirect points at a host not on the list, or at plain HTTP.
    NotAllowed(String),
    TooLarge { limit: u64 },
    Status(u16),
    TooManyRedirects,
    Transport(String),
    Io(std::io::Error),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::NotAllowed(u) => write!(f, "the download is not from an allowed host: {u}"),
            NetError::TooLarge { limit } => write!(f, "the download is larger than {limit} bytes"),
            NetError::Status(s) => write!(f, "the server answered with status {s}"),
            NetError::TooManyRedirects => write!(f, "too many redirects"),
            NetError::Transport(e) => write!(f, "the download failed: {e}"),
            NetError::Io(e) => write!(f, "cannot write the download: {e}"),
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
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next()?
    } else {
        host_port.split(':').next()?
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme, host.to_ascii_lowercase()))
}

/// Whether `url` may be fetched under `policy`: the host is on the list, and the scheme is HTTPS (or plain HTTP when the
/// policy allows it).
pub fn allowed(url: &str, policy: &Policy) -> bool {
    match host_of(url) {
        Some((scheme, host)) => {
            let scheme_ok = scheme == "https" || (scheme == "http" && policy.allow_plain_http);
            scheme_ok && policy.allowed_hosts.contains(&host)
        }
        None => false,
    }
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
    let agent = agent(policy);
    let mut current = url.to_string();
    for _ in 0..=policy.max_redirects {
        if !allowed(&current, policy) {
            return Err(NetError::NotAllowed(current));
        }
        let resp = agent.get(&current).call().map_err(|e| NetError::Transport(e.to_string()))?;
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
        if status != 200 {
            return Err(NetError::Status(status));
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
    let mut out = File::create(&part)?;
    let mut body = resp.into_body();
    let mut reader = body.with_config().limit(policy.max_bytes).reader();
    let mut buf = [0u8; 32 * 1024];
    let mut bytes = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = fs::remove_file(&part);
                let msg = e.to_string();
                return Err(if msg.contains("limit") { NetError::TooLarge { limit: policy.max_bytes } } else { NetError::Transport(msg) });
            }
        };
        out.write_all(&buf[..n])?;
        bytes += n as u64;
        progress(bytes, total);
    }
    out.sync_all()?;
    drop(out);
    fs::rename(&part, dest)?;
    Ok(Download { bytes, final_url })
}

/// Reads a small text document (a catalog index, a listing) under the same policy, with the same size limit.
pub fn fetch_text(url: &str, policy: &Policy) -> Result<String, NetError> {
    let (_, resp) = open(url, policy)?;
    let mut text = String::new();
    let mut body = resp.into_body();
    let mut reader = body.with_config().limit(policy.max_bytes).reader();
    reader.read_to_string(&mut text).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("limit") {
            NetError::TooLarge { limit: policy.max_bytes }
        } else {
            NetError::Transport(msg)
        }
    })?;
    Ok(text)
}

/// Reads a file (an icon, a screenshot) into memory under the same policy and size limit.
pub fn fetch_bytes(url: &str, policy: &Policy) -> Result<Vec<u8>, NetError> {
    let (_, resp) = open(url, policy)?;
    let mut body = resp.into_body();
    let mut reader = body.with_config().limit(policy.max_bytes).reader();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("limit") {
            NetError::TooLarge { limit: policy.max_bytes }
        } else {
            NetError::Transport(msg)
        }
    })?;
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
        assert!(!allowed("https://github.com.evil.example/a", &p), "a look-alike host is refused");
        assert!(!allowed("https://evil.example/github.com", &p));
        assert!(!allowed("https://sub.github.com/a", &p), "a subdomain is not on the list");
        assert!(!allowed("https://github.com@evil.example/a", &p), "user information does not change the host");
        assert!(!allowed("http://github.com/a", &p), "plain HTTP is refused");
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
                    "/redirect-out" => b"HTTP/1.1 302 Found\r\nLocation: http://localhost:1/elsewhere\r\nConnection: close\r\n\r\n".to_vec(),
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
        let r = download(&format!("{base}/redirect-out"), &dir.join("x"), &local_policy(), |_, _| {});
        assert!(matches!(r, Err(NetError::NotAllowed(_))), "{r:?}");
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
    fn refuses_a_url_outside_the_list_without_connecting() {
        let r = fetch_text("https://evil.example/x", &Policy::store(100));
        assert!(matches!(r, Err(NetError::NotAllowed(_))));
    }
}
