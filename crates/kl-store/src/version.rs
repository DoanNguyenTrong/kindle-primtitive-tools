//! Versions as the store writes them: `X.Y.Z`, a date-style `2026.10`, or either with a packaging suffix `-pN`.
//! Ordering compares the numbers first, then the packaging number, so `2.6` < `2.6-p1` < `2.7`.

use std::cmp::Ordering;

#[derive(Debug, Clone)]
pub struct Version {
    pub nums: Vec<u64>,
    pub packaging: u64,
}

impl Version {
    /// Parses `1.2.3`, `2026.10`, `2.6-p1`. `None` for anything else.
    pub fn parse(text: &str) -> Option<Version> {
        let (main, packaging) = match text.rsplit_once("-p") {
            Some((m, p)) => (m, p.parse::<u64>().ok()?),
            None => (text, 0),
        };
        let nums = main.split('.').map(|p| p.parse::<u64>().ok()).collect::<Option<Vec<_>>>()?;
        if nums.is_empty() {
            return None;
        }
        Some(Version { nums, packaging })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.nums.len().max(other.nums.len());
        for i in 0..n {
            let a = self.nums.get(i).copied().unwrap_or(0);
            let b = other.nums.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        self.packaging.cmp(&other.packaging)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Equality follows ordering, so `1.0` and `1.0.0` are the same version.
impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

/// The Kindle's firmware version from the text of `/etc/prettyversion.txt` (`Kindle 5.18.2 (…)`) or
/// `/etc/version.txt` (`System Software Version: …`): the first word with at least two dots-separated numbers.
pub fn firmware_from_text(text: &str) -> Option<Version> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ':')
        .filter(|w| w.contains('.'))
        .find_map(|w| {
            let w = w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
            let v = Version::parse(w)?;
            (v.nums.len() >= 2).then_some(v)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn orders_semver_dates_and_packaging() {
        assert!(v("0.3.21") > v("0.3.9"));
        assert!(v("2.6-p1") > v("2.6"));
        assert!(v("2.7") > v("2.6-p9"));
        assert!(v("2026.10") > v("2026.9"));
        assert_eq!(v("1.0"), v("1.0.0"));
    }

    #[test]
    fn rejects_what_is_not_a_version() {
        assert!(Version::parse("").is_none());
        assert!(Version::parse("1.x").is_none());
        assert!(Version::parse("1.2-pbeta").is_none());
    }

    #[test]
    fn reads_the_kindle_firmware_from_its_files() {
        assert_eq!(firmware_from_text("Kindle 5.18.2 (~~otaVersion~~)"), Some(v("5.18.2")));
        assert_eq!(
            firmware_from_text("System Software Version: 025-juno_1802_stinger_zelda-443455"),
            None,
            "a build number without dots is not the firmware version"
        );
        assert_eq!(firmware_from_text("System Software Version: 5.16.3.1"), Some(v("5.16.3.1")));
    }
}
