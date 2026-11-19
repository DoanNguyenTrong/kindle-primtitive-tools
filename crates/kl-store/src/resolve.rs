//! Choosing what to install for a device: the newest version that has the device's architecture, runs on its firmware,
//! is not withdrawn, and whose requirements are installed.

use crate::catalog::{Listing, Source, VersionEntry};
use crate::version::Version;
use std::collections::BTreeMap;
use std::fmt;

/// What the store knows about the Kindle it runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// `armhf` (hard-float) or `armel` (soft-float).
    pub arch: String,
    pub firmware: Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// A link listing: the store shows it, but installs nothing.
    NotInstallable,
    /// No version has this device's architecture.
    NoArch(String),
    /// Versions exist for the architecture, but none runs on this firmware; the lowest minimum is given.
    NoFirmware { need: String },
    /// Every version with this architecture is withdrawn.
    AllWithdrawn,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotInstallable => write!(f, "this app is listed by link and is not installed by the store"),
            ResolveError::NoArch(arch) => write!(f, "no version is built for this Kindle ({arch})"),
            ResolveError::NoFirmware { need } => write!(f, "needs Kindle firmware {need} or later"),
            ResolveError::AllWithdrawn => write!(f, "every version for this Kindle has been withdrawn"),
        }
    }
}

/// The newest installable version for `device`, with its position in the listing.
pub fn pick_version<'a>(listing: &'a Listing, device: &Device) -> Result<&'a VersionEntry, ResolveError> {
    if listing.source == Source::Link {
        return Err(ResolveError::NotInstallable);
    }
    let mut candidates: Vec<(Version, &VersionEntry)> = listing
        .versions
        .iter()
        .filter_map(|v| Version::parse(&v.version).map(|parsed| (parsed, v)))
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));

    let with_arch: Vec<&(Version, &VersionEntry)> =
        candidates.iter().filter(|(_, v)| v.assets.contains_key(&device.arch)).collect();
    if with_arch.is_empty() {
        return Err(ResolveError::NoArch(device.arch.clone()));
    }
    let live: Vec<&(Version, &VersionEntry)> = with_arch.iter().copied().filter(|(_, v)| v.withdrawn.is_none()).collect();
    if live.is_empty() {
        return Err(ResolveError::AllWithdrawn);
    }
    let mut lowest_need: Option<String> = None;
    for (_, v) in &live {
        let min = Version::parse(&v.firmware.min);
        let max = v.firmware.max.as_deref().and_then(Version::parse);
        let fits_min = min.as_ref().is_some_and(|m| device.firmware >= *m);
        let fits_max = max.as_ref().is_none_or(|m| device.firmware <= *m);
        if fits_min && fits_max {
            return Ok(v);
        }
        if !fits_min && lowest_need.as_ref().is_none_or(|n| min.as_ref() < Version::parse(n).as_ref()) {
            lowest_need = Some(v.firmware.min.clone());
        }
    }
    Err(ResolveError::NoFirmware { need: lowest_need.unwrap_or_else(|| "a newer one".into()) })
}

/// The requirements that are not met, as `(app id, needed range, installed version or none)`.
///
/// `installed` gives the installed version of an app id, if any. A range is `>=X.Y.Z`.
pub fn missing_requirements(
    requires: &BTreeMap<String, String>,
    installed: &dyn Fn(&str) -> Option<Version>,
) -> Vec<(String, String, Option<Version>)> {
    requires
        .iter()
        .filter_map(|(id, range)| {
            let need = Version::parse(range.trim_start_matches(">="))?;
            match installed(id) {
                Some(have) if have >= need => None,
                have => Some((id.clone(), range.clone(), have)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{sample_listing, Firmware};

    fn dev(arch: &str, fw: &str) -> Device {
        Device { arch: arch.into(), firmware: Version::parse(fw).unwrap() }
    }

    #[test]
    fn picks_the_newest_version_for_the_device() {
        let l = sample_listing();
        assert_eq!(pick_version(&l, &dev("armhf", "5.18.2")).unwrap().version, "0.2.18");
    }

    #[test]
    fn skips_withdrawn_versions() {
        let mut l = sample_listing();
        l.versions[0].withdrawn = Some("crashes on start".into());
        assert_eq!(pick_version(&l, &dev("armhf", "5.18.2")).unwrap().version, "0.2.17");
        l.versions[1].withdrawn = Some("also broken".into());
        assert_eq!(pick_version(&l, &dev("armhf", "5.18.2")), Err(ResolveError::AllWithdrawn));
    }

    #[test]
    fn refuses_a_missing_architecture_and_says_which() {
        let l = sample_listing();
        assert_eq!(pick_version(&l, &dev("armel", "5.18.2")), Err(ResolveError::NoArch("armel".into())));
    }

    #[test]
    fn respects_the_firmware_range_on_both_ends() {
        let mut l = sample_listing();
        l.versions[0].firmware = Firmware { min: "5.19.0".into(), max: None };
        l.versions[1].firmware = Firmware { min: "5.16.3".into(), max: Some("5.18.9".into()) };
        // 5.18.2 is below the newest version's minimum, so the older one that fits its range is chosen.
        assert_eq!(pick_version(&l, &dev("armhf", "5.18.2")).unwrap().version, "0.2.17");
        // 5.20.0 is above the newest version's minimum and below no maximum: the newest version.
        assert_eq!(pick_version(&l, &dev("armhf", "5.20.0")).unwrap().version, "0.2.18");
        // Below every minimum: the lowest minimum is reported.
        assert_eq!(pick_version(&l, &dev("armhf", "5.15.0")), Err(ResolveError::NoFirmware { need: "5.16.3".into() }));
        // Above the older version's maximum and below the newest's minimum: nothing fits.
        assert_eq!(pick_version(&l, &dev("armhf", "5.19.0")).unwrap().version, "0.2.18");
        assert_eq!(pick_version(&l, &dev("armhf", "5.16.3")).unwrap().version, "0.2.17");
    }

    #[test]
    fn link_listings_are_never_installed() {
        let mut l = sample_listing();
        l.source = Source::Link;
        assert_eq!(pick_version(&l, &dev("armhf", "5.18.2")), Err(ResolveError::NotInstallable));
    }

    #[test]
    fn reports_unmet_requirements() {
        let mut req = BTreeMap::new();
        req.insert("primitive-launcher".to_string(), ">=0.3.21".to_string());
        req.insert("kterm".to_string(), ">=2.6".to_string());
        let installed = |id: &str| match id {
            "primitive-launcher" => Version::parse("0.3.20"),
            _ => None,
        };
        let missing = missing_requirements(&req, &installed);
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].0, "kterm");
        assert_eq!(missing[1].2, Version::parse("0.3.20"));
        let met = |_: &str| Version::parse("9.0");
        assert!(missing_requirements(&req, &met).is_empty());
    }
}
