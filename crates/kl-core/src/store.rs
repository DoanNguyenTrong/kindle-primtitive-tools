//! The store's entries as the screens show them: one per listing in the catalog cache, with what is installed and
//! what the store would do for it. Built from `kl-store`'s listing and the installed record; no device code here.

use kl_store::catalog::{Changes, Listing, Source, VersionEntry};
use kl_store::record::Installed;
use kl_store::resolve::{self, Device};
use kl_store::Version;

/// Whether an entry is installed, and whether a newer version is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    Installed(String),
    Update { installed: String, latest: String },
}

/// The store's tabs in their display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreTab {
    Today,
    Apps,
    Updates,
    Search,
}

impl StoreTab {
    pub const ALL: [Self; 4] = [Self::Today, Self::Apps, Self::Updates, Self::Search];
}

pub const STORE_ID: &str = "primitive-store";
pub use kl_store::install::LAUNCHER_ID;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionNotes {
    pub version: String,
    pub date: String,
    pub notes: String,
}

/// What the action button does: install from the store (hosted or upstream), or show the page to read (link).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Install,
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreEntry {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub developer: String,
    pub description: String,
    pub categories: Vec<String>,
    pub licence: String,
    pub repo: String,
    pub kind: Kind,
    pub latest: Option<String>,
    pub status: Status,
    /// What the app does outside its own folder, in the words the install sheet uses.
    pub changes: Vec<String>,
    /// Whether the confirmation needs a system and startup warning.
    pub system_changes: bool,
    /// The download's size for the latest version (its armhf asset), when the listing has one.
    pub size: Option<u64>,
    /// The lowest Kindle firmware the latest version runs on.
    pub firmware_min: String,
    /// The Kindles the latest version was tested on.
    pub tested_on: Vec<String>,
    /// How many screenshots the listing has (at most two are shown on the product page).
    pub screenshots: usize,
    pub versions: Vec<VersionNotes>,
    pub availability: Option<String>,
    /// A known launcher entry can be opened through the supervisor.
    pub launchable: bool,
}

/// A size as the product page shows it: "4.5 MB", "640 KB", "900 bytes".
pub fn format_size(bytes: u64) -> String {
    if bytes == 1 {
        "1 byte".to_string()
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} bytes")
    }
}

/// The newest version entry that is not withdrawn, by version order.
pub fn newest_live_entry(listing: &Listing) -> Option<&kl_store::catalog::VersionEntry> {
    listing
        .versions
        .iter()
        .filter(|v| v.withdrawn.is_none())
        .filter_map(|v| Version::parse(&v.version).map(|p| (p, v)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, v)| v)
}

/// The newest version that is not withdrawn, by version order.
pub fn newest_live(listing: &Listing) -> Option<String> {
    newest_live_entry(listing).map(|v| v.version.clone())
}

/// "reading" → "Reading", "developer" → "Developer".
fn capitalise(word: &str) -> String {
    let mut c = word.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The "What this app changes" lines, from the listing's flags (store-install-apps §4).
pub fn change_lines(c: &Changes) -> Vec<String> {
    let mut lines = Vec::new();
    if c.network {
        lines.push("Uses the internet.".to_string());
    }
    if c.stops_kindle_ui {
        lines.push("Stops the Kindle's own screen while it runs.".to_string());
    }
    if c.starts_at_boot {
        lines.push("Starts when the Kindle starts.".to_string());
    }
    if c.root_filesystem {
        lines.push("Changes the Kindle's system files.".to_string());
    }
    if c.hooks {
        lines.push("Runs its own install steps.".to_string());
    }
    if lines.is_empty() {
        lines.push("Changes nothing outside its own folder.".to_string());
    }
    if !c.notes.is_empty() {
        lines.push(c.notes.clone());
    }
    lines
}

impl StoreEntry {
    /// The entry for a listing, given what is installed under its id.
    pub fn from_listing(listing: &Listing, installed: Option<&Installed>) -> StoreEntry {
        Self::from_selection(listing, installed, newest_live_entry(listing), None, None)
    }

    pub fn for_device(listing: &Listing, installed: Option<&Installed>, device: &Device) -> StoreEntry {
        if listing.source == Source::Link {
            return Self::from_listing(listing, installed);
        }
        match resolve::pick_version(listing, device) {
            Ok(version) => Self::from_selection(listing, installed, Some(version), Some(&device.arch), None),
            Err(error) => Self::from_selection(listing, installed, None, None, Some(error.to_string())),
        }
    }

    fn from_selection(listing: &Listing, installed: Option<&Installed>, newest: Option<&VersionEntry>, arch: Option<&str>, availability: Option<String>) -> StoreEntry {
        let latest = newest.map(|v| v.version.clone());
        let status = match (installed, &latest) {
            (None, _) => Status::NotInstalled,
            (Some(i), Some(l)) if Version::parse(l) > Version::parse(&i.version) => {
                Status::Update { installed: i.version.clone(), latest: l.clone() }
            }
            (Some(i), _) => Status::Installed(i.version.clone()),
        };
        let mut version_entries: Vec<_> = listing.versions.iter()
            .filter(|v| v.withdrawn.is_none())
            .filter_map(|v| Version::parse(&v.version).map(|number| (number, v)))
            .collect();
        version_entries.sort_by(|(a, _), (b, _)| b.cmp(a));
        if let Some(selected) = newest {
            if let Some(index) = version_entries.iter().position(|(_, version)| version.version == selected.version) {
                let primary = version_entries.remove(index);
                version_entries.insert(0, primary);
            }
        }
        let versions = version_entries.iter().map(|(_, version)| VersionNotes {
            version: version.version.clone(), date: version.date.clone(), notes: version.notes.clone(),
        }).collect();
        StoreEntry {
            id: listing.id.clone(),
            name: listing.name.clone(),
            subtitle: listing.subtitle.clone(),
            developer: listing.developer.name.clone(),
            description: listing.description.clone(),
            categories: listing.categories.clone(),
            licence: listing.licence.clone(),
            repo: listing.repo.clone(),
            kind: if listing.source == Source::Link { Kind::Link } else { Kind::Install },
            latest,
            status,
            changes: change_lines(&listing.changes),
            system_changes: listing.categories.iter().any(|category| category == "system")
                || listing.changes.starts_at_boot
                || listing.changes.root_filesystem,
            size: newest.and_then(|v| match arch {
                Some(arch) => v.assets.get(arch),
                None => v.assets.get("armhf").or_else(|| v.assets.values().next()),
            }).map(|a| a.size),
            firmware_min: newest.map(|v| v.firmware.min.clone()).unwrap_or_default(),
            tested_on: newest.map(|v| v.tested_on.clone()).unwrap_or_default(),
            screenshots: listing.screenshots.len().min(2),
            versions,
            availability,
            launchable: installed.and_then(crate::store_open::entry_file).is_some(),
        }
    }

    /// An installed app remains manageable if its catalog listing has gone away.
    pub fn from_installed(app: &Installed) -> StoreEntry {
        Self {
            id: app.id.clone(),
            name: app.id.split('-').map(capitalise).collect::<Vec<_>>().join(" "),
            subtitle: "Installed from the store".to_string(),
            developer: String::new(),
            description: "The store has no listing for this app.".to_string(),
            categories: Vec::new(),
            licence: String::new(),
            repo: String::new(),
            kind: Kind::Install,
            latest: None,
            status: Status::Installed(app.version.clone()),
            changes: Vec::new(),
            system_changes: false,
            size: None,
            firmware_min: String::new(),
            tested_on: Vec::new(),
            screenshots: 0,
            versions: Vec::new(),
            availability: None,
            launchable: crate::store_open::entry_file(app).is_some(),
        }
    }

    /// The label of the product page's last row when the app can be removed from the Kindle.
    pub const REMOVE_LABEL: &'static str = "Remove from this Kindle";

    /// Whether the store installed this app (so the page offers Remove).
    pub fn can_remove(&self) -> bool {
        self.kind == Kind::Install
            && !matches!(self.id.as_str(), STORE_ID | LAUNCHER_ID)
            && matches!(self.status, Status::Installed(_) | Status::Update { .. })
    }

    pub fn can_open(&self) -> bool {
        self.kind == Kind::Install && self.launchable && matches!(self.status, Status::Installed(_))
            && !matches!(self.id.as_str(), STORE_ID | LAUNCHER_ID)
    }

    pub fn can_install(&self) -> bool {
        self.kind == Kind::Install && self.latest.is_some() && !matches!(self.status, Status::Installed(_))
    }

    /// The rows the product page shows: the information rows, and Remove when the app is installed.
    pub fn page_rows(&self) -> Vec<(String, String)> {
        let mut rows = self.info_rows();
        if self.can_remove() {
            rows.push((Self::REMOVE_LABEL.to_string(), String::new()));
        }
        rows
    }

    /// The information rows of the product page: label and value, in the order the page shows them.
    pub fn info_rows(&self) -> Vec<(String, String)> {
        let mut rows = vec![
            ("Developer".to_string(), self.developer.clone()),
            ("Licence".to_string(), self.licence.clone()),
            ("Category".to_string(), self.categories.iter().map(|c| capitalise(c)).collect::<Vec<_>>().join(", ")),
            ("Kindle firmware".to_string(), if self.firmware_min.is_empty() { String::new() } else { format!("{} or later", self.firmware_min) }),
            ("Tested on".to_string(), if self.tested_on.is_empty() { "Not yet".to_string() } else { self.tested_on.join(", ") }),
            ("Repository".to_string(), self.repo.trim_start_matches("https://").to_string()),
        ];
        for line in &self.changes {
            rows.push(("Change".to_string(), line.clone()));
        }
        rows
    }

    /// The status line on the app's page, in the store's words.
    pub fn status_line(&self) -> String {
        if self.kind == Kind::Link {
            return "Listed by link: the store does not install it.".to_string();
        }
        match (&self.status, &self.latest) {
            (Status::NotInstalled, Some(v)) => format!("Version {v}"),
            (Status::NotInstalled, None) => self.availability.clone().unwrap_or_else(|| "No version for this Kindle.".to_string()),
            (Status::Installed(v), _) => format!("Installed: version {v}"),
            (Status::Update { installed, latest }, _) => format!("Update ready: {installed} to {latest}"),
        }
    }

    /// The action button's word, for the row and the page.
    pub fn action_label(&self) -> &'static str {
        match (self.kind, &self.status) {
            (Kind::Link, _) => "View",
            (Kind::Install, Status::NotInstalled) if self.latest.is_none() => "Unavailable",
            (Kind::Install, Status::NotInstalled) => "Get",
            (Kind::Install, Status::Update { .. }) => "Update",
            (Kind::Install, Status::Installed(_)) if self.can_open() => "Open",
            (Kind::Install, Status::Installed(_)) => "Installed",
        }
    }
}

/// An entry for the screens' tests, without a listing.
#[cfg(test)]
pub(crate) fn test_entry(id: &str, name: &str, status: Status) -> StoreEntry {
    StoreEntry {
        id: id.into(),
        name: name.into(),
        subtitle: format!("{name} subtitle"),
        developer: "Primitive AI".into(),
        description: "A test app.".into(),
        categories: vec!["productivity".into()],
        licence: "PolyForm-Noncommercial-1.0.0".into(),
        repo: "https://github.com/example/app".into(),
        kind: Kind::Install,
        latest: Some("1.0.0".into()),
        status,
        changes: vec!["Changes nothing outside its own folder.".into()],
        system_changes: false,
        size: Some(1_000_000),
        firmware_min: "5.16.3".into(),
        tested_on: vec!["KindleOasis3".into()],
        screenshots: 0,
        versions: Vec::new(),
        availability: None,
        launchable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample_listing() -> Listing {
        serde_json::from_str(
            r#"{
            "schema": 1, "id": "primitive-browser", "dir": "browser",
            "name": "Primitive Browser", "subtitle": "A browser for e-ink",
            "developer": {"name": "Primitive AI"}, "categories": ["internet"],
            "description": "A web browser made for e-ink, with night mode and image switch.",
            "repo": "https://github.com/DoanNguyenTrong/kindle-primitive-browser", "licence": "PolyForm-Noncommercial-1.0.0",
            "source": "hosted", "icon": "icon.png",
            "changes": {"network": true, "stops_kindle_ui": false, "starts_at_boot": false, "root_filesystem": false, "hooks": false},
            "versions": [
              {"version": "0.2.18", "date": "2026-11-20", "notes": "Newest.", "firmware": {"min": "5.16.3"},
               "tested_on": ["KindleOasis3"], "assets": {"armhf": {"url": "https://example.com/a.zip", "sha256": "aa", "size": 10}}},
              {"version": "0.2.17", "date": "2026-11-19", "notes": "Older.", "firmware": {"min": "5.16.3"},
               "tested_on": ["KindleOasis3"], "assets": {"armhf": {"url": "https://example.com/b.zip", "sha256": "bb", "size": 10}}}
            ]
        }"#,
        )
        .expect("the sample listing is valid")
    }

    fn installed(version: &str) -> Installed {
        Installed {
            id: "primitive-browser".into(),
            dir: "browser".into(),
            version: version.into(),
            arch: "armhf".into(),
            installed_at: "2026-11-19".into(),
            source_url: String::new(),
            sha256: String::new(),
            files: vec![],
            requires: Default::default(),
        }
    }

    #[test]
    fn a_missing_install_is_get_and_the_newest_live_version_is_shown() {
        let e = StoreEntry::from_listing(&sample_listing(), None);
        assert_eq!(e.status, Status::NotInstalled);
        assert_eq!(e.latest.as_deref(), Some("0.2.18"));
        assert_eq!(e.action_label(), "Get");
        assert_eq!(e.changes, vec!["Uses the internet."]);
    }

    #[test]
    fn an_older_install_offers_an_update_and_the_same_version_is_installed() {
        let l = sample_listing();
        let old = StoreEntry::from_listing(&l, Some(&installed("0.2.17")));
        assert_eq!(old.status, Status::Update { installed: "0.2.17".into(), latest: "0.2.18".into() });
        assert_eq!(old.action_label(), "Update");
        let same = StoreEntry::from_listing(&l, Some(&installed("0.2.18")));
        assert_eq!(same.status, Status::Installed("0.2.18".into()));
        assert_eq!(same.action_label(), "Installed");
    }

    #[test]
    fn a_withdrawn_version_is_not_offered_and_a_link_listing_says_view() {
        let mut l = sample_listing();
        l.versions[0].withdrawn = Some("broken".into());
        assert_eq!(newest_live(&l).as_deref(), Some("0.2.17"));
        l.source = Source::Link;
        let e = StoreEntry::from_listing(&l, None);
        assert_eq!(e.kind, Kind::Link);
        assert_eq!(e.action_label(), "View");
    }

    #[test]
    fn the_status_line_says_what_is_installed_and_what_is_ready() {
        let l = sample_listing();
        assert_eq!(StoreEntry::from_listing(&l, None).status_line(), "Version 0.2.18");
        assert_eq!(
            StoreEntry::from_listing(&l, Some(&installed("0.2.17"))).status_line(),
            "Update ready: 0.2.17 to 0.2.18"
        );
        assert_eq!(StoreEntry::from_listing(&l, Some(&installed("0.2.18"))).status_line(), "Installed: version 0.2.18");
        let mut link = l.clone();
        link.source = Source::Link;
        assert!(StoreEntry::from_listing(&link, None).status_line().starts_with("Listed by link"));
    }

    #[test]
    fn the_product_page_facts_come_from_the_newest_version() {
        let e = StoreEntry::from_listing(&sample_listing(), None);
        assert_eq!(e.firmware_min, "5.16.3");
        assert_eq!(e.tested_on, vec!["KindleOasis3"]);
        assert_eq!(e.size, Some(10));
        let rows = e.info_rows();
        assert_eq!(rows[0], ("Developer".to_string(), "Primitive AI".to_string()));
        assert_eq!(rows[2].1, "Internet");
        assert_eq!(rows[3].1, "5.16.3 or later");
        assert_eq!(rows.last().unwrap().1, "Uses the internet.");
        assert_eq!(format_size(4_500_000), "4.5 MB");
        assert_eq!(format_size(640_000), "640 KB");
    }

    #[test]
    fn a_change_with_no_flags_says_it_changes_nothing_outside_its_folder() {
        let c = Changes { network: false, stops_kindle_ui: false, starts_at_boot: false, root_filesystem: false, hooks: false, notes: String::new() };
        assert_eq!(change_lines(&c), vec!["Changes nothing outside its own folder."]);
    }
}
