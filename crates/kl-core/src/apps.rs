//! The app list: built-ins, scriptlets found in `documents/`, and the entries and overrides of
//! `apps.toml`, merged in that order.

use std::{collections::BTreeSet, ffi::OsStr, fs, path::Path};

use crate::{config::Config, scriptlet};

pub const STORE_ENTRY_FILE: &str = "Store.sh";
pub const STORE_BINARY: &str = "store/bin/kl-store-app";
const STORE_NAME: &str = "Store";
const STORE_ICON: &[u8] = include_bytes!("../../../assets/icons/apps/store.png");

/// Reserved spellings used by the built-in Store tile and standalone library entries.
pub fn is_store_alias(name: &str) -> bool {
    let name = name.trim();
    let name = match name.rsplit_once('.') {
        Some((stem, extension)) if extension.eq_ignore_ascii_case("sh") => stem.trim(),
        _ => name,
    };
    ["Store", "Primitive Store", "Primitive-Store", "PrimitiveStore"]
        .iter()
        .any(|alias| name.eq_ignore_ascii_case(alias))
}

/// A complete standalone copy has both the regular library entry and its executable.
pub fn store_present(documents: &Path) -> bool {
    if !fs::symlink_metadata(documents.join(STORE_ENTRY_FILE)).is_ok_and(|entry| entry.is_file()) {
        return false;
    }
    let Some(binary) = documents
        .parent()
        .and_then(|root| fs::symlink_metadata(root.join(STORE_BINARY)).ok())
    else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if binary.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    binary.is_file()
}

/// Resolve the Store tile against its canonical script even if storage changed after discovery.
pub fn store_entry(mut app: App, documents: &Path) -> Option<App> {
    if (app.launch != Launch::Store && app.id != STORE_ENTRY_FILE) || !store_present(documents) {
        return None;
    }
    use_store_script(&mut app, documents);
    Some(app)
}

fn use_store_script(app: &mut App, documents: &Path) {
    app.name = STORE_NAME.into();
    if matches!(app.icon, IconSource::None) {
        app.icon = IconSource::Bundled(STORE_ICON);
    }
    app.background = false;
    app.dir = documents.to_string_lossy().into_owned();
    app.launch = Launch::Script {
        path: documents.join(STORE_ENTRY_FILE).to_string_lossy().into_owned(),
        hooks: false,
    };
}

/// How an app is started. The supervisor (`launcher.sh`) runs it; see `next_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// `/bin/sh <path>`, or with `hooks`, source the file and call `on_run`.
    Script { path: String, hooks: bool },
    /// `/bin/sh -c <command>`.
    Shell { command: String },
    /// Built-in: stop the launcher and start the stock Kindle UI.
    KindleMode,
    /// Built-in: the Settings screen.
    Settings,
    /// Built-in: the Extensions list (KUAL menus).
    Extensions,
    /// Built-in: launch the standalone Store, or install it when missing.
    Store,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconSource {
    /// No icon: a monogram is drawn.
    None,
    /// An image file (PNG).
    File(String),
    /// Base64 PNG data from a `data:` URI.
    Data(String),
    /// A PNG built into the launcher (the built-ins' icons).
    Bundled(&'static [u8]),
}

/// The Kindle UI tile's icon: the Kindle app's reader under the arcs, in the panel's grays
/// (`assets/icons/kindle-ui.png`, made from `assets/kindle-icon.png`).
pub const KINDLE_UI_ICON: &[u8] = include_bytes!("../../../assets/icons/kindle-ui.png");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// Stable id: the file name for scriptlets, the `apps.toml` id otherwise, `kindle`/`settings` for built-ins.
    pub id: String,
    pub name: String,
    pub icon: IconSource,
    pub launch: Launch,
    /// Working directory for the app.
    pub dir: String,
    /// Explicit position from `apps.toml`; apps without one follow, by name.
    pub order: Option<i64>,
    pub hidden: bool,
    /// Runs while the launcher stays in front (apps.toml `mode = "background"`, KUAL `exitmenu: false`); its
    /// last output line is shown when it ends.
    pub background: bool,
}

impl App {
    /// The three lines the supervisor reads from `/var/tmp/kl/next`: working directory, kind, argument.
    /// Built-ins never reach the supervisor and give `None`.
    pub fn next_file(&self) -> Option<String> {
        let (kind, arg) = match &self.launch {
            Launch::Script { path, hooks: false } => ("script", path.as_str()),
            Launch::Script { path, hooks: true } => ("hooks", path.as_str()),
            Launch::Shell { command } => ("shell", command.as_str()),
            Launch::KindleMode | Launch::Settings | Launch::Extensions | Launch::Store => return None,
        };
        // One value per line: a newline inside a value would shift the others.
        if arg.contains('\n') || self.dir.contains('\n') {
            return None;
        }
        Some(format!("{}\n{kind}\n{arg}\n", self.dir))
    }
}

fn icon_from(value: Option<&str>) -> IconSource {
    match value {
        Some(v) if v.starts_with("data:") => match v.split_once(";base64,") {
            Some((_, data)) => IconSource::Data(data.to_string()),
            None => IconSource::None,
        },
        Some(v) if v.starts_with('/') => IconSource::File(v.to_string()),
        _ => IconSource::None,
    }
}

fn native_icon(file: &str) -> IconSource {
    let bytes: &[u8] = match file {
        "Blocks.sh" => include_bytes!("../../../assets/icons/apps/blocks.png"),
        "Calculator.sh" => include_bytes!("../../../assets/icons/apps/calculator.png"),
        "Cards.sh" => include_bytes!("../../../assets/icons/apps/cards.png"),
        "Cascade.sh" => include_bytes!("../../../assets/icons/apps/cascade.png"),
        "Chess.sh" => include_bytes!("../../../assets/icons/apps/chess.png"),
        "Crossword.sh" => include_bytes!("../../../assets/icons/apps/crossword.png"),
        "Focus.sh" => include_bytes!("../../../assets/icons/apps/focus.png"),
        "Hello.sh" => include_bytes!("../../../assets/icons/apps/hello.png"),
        "Minesweeper.sh" => include_bytes!("../../../assets/icons/apps/minesweeper.png"),
        "Notes.sh" => include_bytes!("../../../assets/icons/apps/notes.png"),
        "Ships.sh" => include_bytes!("../../../assets/icons/apps/ships.png"),
        "Solitaire.sh" => include_bytes!("../../../assets/icons/apps/solitaire.png"),
        "Sudoku.sh" => include_bytes!("../../../assets/icons/apps/sudoku.png"),
        "Words.sh" => include_bytes!("../../../assets/icons/apps/words.png"),
        _ => return IconSource::None,
    };
    IconSource::Bundled(bytes)
}

fn builtins() -> Vec<App> {
    vec![
        App {
            id: "settings".into(),
            name: "Settings".into(),
            icon: IconSource::None,
            launch: Launch::Settings,
            dir: "/".into(),
            order: Some(9_000),
            hidden: false,
            background: false,
        },
        App {
            id: "store".into(),
            name: STORE_NAME.into(),
            icon: IconSource::Bundled(STORE_ICON),
            launch: Launch::Store,
            dir: "/".into(),
            order: Some(9_000),
            hidden: false,
            background: false,
        },
    ]
}

fn scriptlet_filename(path: &Path) -> Option<&OsStr> {
    path.file_name().filter(|name| {
        path.extension().is_some_and(|extension| extension == "sh") && !name.to_string_lossy().starts_with("._")
    })
}

/// The scriptlets in `documents` (the Kindle's `/mnt/us/documents`), sorted by file name. macOS
/// metadata files (`._*`) and this launcher's own entry are skipped.
pub fn scriptlets(documents: &Path) -> Vec<App> {
    let Ok(dir) = fs::read_dir(documents) else { return Vec::new() };
    let mut files: Vec<_> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|path| scriptlet_filename(path).is_some())
        .collect();
    files.sort();
    let mut apps = Vec::new();
    for path in files {
        let Ok(bytes) = fs::read(&path) else { continue };
        let header = scriptlet::parse(&String::from_utf8_lossy(&bytes));
        if header.launcher_entry {
            continue;
        }
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let icon = if header.icon.is_none() && header.author.as_deref() == Some("Primitive AI") {
            native_icon(&file)
        } else {
            icon_from(header.icon.as_deref())
        };
        apps.push(App {
            id: file,
            name: header.name.unwrap_or(stem),
            icon,
            launch: Launch::Script { path: path.to_string_lossy().into_owned(), hooks: header.use_hooks },
            dir: documents.to_string_lossy().into_owned(),
            order: None,
            hidden: false,
            background: false,
        });
    }
    apps
}

/// The grid order: ordered apps first, then the rest by name, then the built-ins (orders from 9000).
pub fn sort_key(a: &App) -> (u8, i64, String) {
    let group = match a.order {
        Some(o) if o < 9_000 => 0,
        None => 1,
        Some(_) => 2,
    };
    (group, a.order.unwrap_or(0), a.name.to_lowercase())
}

/// Merge built-ins, discovered apps and `apps.toml`, and sort. Returns every app (hidden ones too) and
/// the problems found. A saved scriptlet override stays dormant until that file is present again.
pub fn merge(discovered: Vec<App>, config: &Config) -> (Vec<App>, Vec<String>) {
    let mut apps = builtins();
    apps.extend(discovered);
    let mut problems = Vec::new();
    for entry in &config.apps {
        if let Some(app) = apps.iter_mut().find(|a| a.id == entry.id) {
            if let Some(name) = &entry.name {
                app.name = name.clone();
            }
            if let Some(icon) = &entry.icon {
                app.icon = icon_from(Some(icon));
            }
            if let Some(command) = &entry.command {
                if !matches!(app.launch, Launch::KindleMode | Launch::Settings | Launch::Extensions | Launch::Store) {
                    app.launch = Launch::Shell { command: command.clone() };
                }
            }
            if let Some(dir) = &entry.dir {
                app.dir = dir.clone();
            }
            if entry.order.is_some() {
                app.order = entry.order;
            }
            if let Some(hidden) = entry.hidden {
                app.hidden = hidden;
            }
            if let Some(background) = entry.background {
                app.background = background;
            }
        } else if let Some(command) = &entry.command {
            apps.push(App {
                id: entry.id.clone(),
                name: entry.name.clone().unwrap_or_else(|| entry.id.clone()),
                icon: icon_from(entry.icon.as_deref()),
                launch: Launch::Shell { command: command.clone() },
                dir: entry.dir.clone().unwrap_or_else(|| "/mnt/us".into()),
                order: entry.order,
                hidden: entry.hidden.unwrap_or(false),
                background: entry.background.unwrap_or(false),
            });
        } else if scriptlet_filename(Path::new(&entry.id)).is_none_or(|name| name != entry.id.as_str()) {
            problems.push(format!("apps.toml: no app \"{}\" found and no command given", entry.id));
        }
    }
    // Ordered apps first, then the rest by name, then the built-ins (orders from 9000) last.
    apps.sort_by_cached_key(sort_key);
    (apps, problems)
}

/// Discover scriptlets in `documents` and merge them with `config`.
/// Every app the launcher shows in the grid: scriptlets in the documents folder, then the `apps.toml`
/// overrides. KUAL extensions are listed behind the Extensions tile (`kual::discover_root`).
pub fn discover(documents: &Path, config: &Config) -> (Vec<App>, Vec<String>) {
    let scripts = scriptlets(documents);
    // Remember the original names because apps.toml may rename an old Store script.
    let former_store_ids: BTreeSet<_> = scripts
        .iter()
        .filter(|app| is_store_alias(&app.name))
        .map(|app| app.id.clone())
        .collect();
    let standalone =
        scripts.iter().any(|app| app.id == STORE_ENTRY_FILE) && store_present(documents);
    let (mut apps, problems) = merge(scripts, config);
    apps.retain_mut(|app| {
        if standalone && app.id == STORE_ENTRY_FILE {
            use_store_script(app, documents);
            true
        } else if !standalone && app.launch == Launch::Store {
            app.name = STORE_NAME.into();
            app.background = false;
            true
        } else {
            app.launch != Launch::Store
                && !is_store_alias(&app.id)
                && !is_store_alias(&app.name)
                && !former_store_ids.contains(&app.id)
        }
    });
    apps.sort_by_cached_key(sort_key);
    (apps, problems)
}

/// KOReader's own library entry (`KOReader.sh`) would start the stock UI again when KOReader exits. Unless
/// `apps.toml` gives it a command, it runs through the launcher's wrapper (`wrapper`, `run-koreader.sh`), which
/// keeps the stock UI stopped. Nothing changes when the wrapper is missing.
pub fn use_koreader_wrapper(apps: &mut [App], config: &Config, wrapper: &Path) {
    if !wrapper.is_file() {
        return;
    }
    let overridden = |id: &str| config.apps.iter().any(|e| e.id == id && e.command.is_some());
    for app in apps.iter_mut().filter(|a| a.id == "KOReader.sh" && !overridden(&a.id)) {
        if matches!(app.launch, Launch::Script { .. }) {
            app.launch = Launch::Shell { command: format!("sh '{}'", wrapper.display()) };
        }
    }
}

/// The built-in Extensions tile, shown when KUAL extensions are installed. It sorts with the other built-ins.
pub fn extensions_app() -> App {
    App {
        id: "extensions".into(),
        name: "Extensions".into(),
        icon: IconSource::None,
        launch: Launch::Extensions,
        dir: "/".into(),
        order: Some(9_000),
        hidden: false,
        background: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kl-core-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_scriptlets_and_skips_metadata_and_the_launcher() {
        let dir = temp_dir("scan");
        fs::write(dir.join("Browser.sh"), "#!/bin/sh\n# Name: Primitive Browser\n# Icon: data:image/png;base64,QUJD\n").unwrap();
        fs::write(dir.join("Plain.sh"), "echo hi\n").unwrap();
        fs::write(dir.join("Hooks.sh"), "#!/bin/sh\n# UseHooks\n").unwrap();
        fs::write(dir.join("._Browser.sh"), "junk").unwrap();
        fs::write(dir.join("Launcher.sh"), "#!/bin/sh\n# Name: Launcher\n# PrimitiveLauncherEntry\n").unwrap();
        fs::write(dir.join("book.azw3"), "x").unwrap();
        let apps = scriptlets(&dir);
        let names: Vec<_> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Primitive Browser", "Hooks", "Plain"]);
        assert_eq!(apps[0].icon, IconSource::Data("QUJD".into()));
        assert_eq!(apps[0].id, "Browser.sh");
        assert!(matches!(&apps[1].launch, Launch::Script { hooks: true, .. }));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_first_party_scriptlets_and_store_have_distinct_icons() {
        let dir = temp_dir("bundled-app-icons");
        let files = [
            "Blocks.sh", "Calculator.sh", "Cards.sh", "Cascade.sh", "Chess.sh",
            "Crossword.sh", "Focus.sh", "Hello.sh", "Minesweeper.sh", "Notes.sh",
            "Ships.sh", "Solitaire.sh", "Sudoku.sh", "Words.sh",
        ];
        for file in files {
            fs::write(dir.join(file), "#!/bin/sh\n# Author: Primitive AI\n").unwrap();
        }
        let (apps, problems) = discover(&dir, &Config::default());
        assert!(problems.is_empty());
        let mut images = BTreeSet::new();
        for id in files.into_iter().chain(["store"]) {
            let app = apps.iter().find(|app| app.id == id).unwrap();
            let IconSource::Bundled(bytes) = app.icon else { panic!("{id} has no icon") };
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "{id}");
            assert!(images.insert(bytes), "{id} shares another app's icon");
        }
        assert_eq!(images.len(), 15);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_icon_defaults_respect_both_identity_fields_and_overrides() {
        let dir = temp_dir("app-icon-overrides");
        for (file, script) in [
            ("Notes.sh", "# Author: Other Author\n"),
            ("focus.sh", "# Author: Primitive AI\n"),
            ("Chess.sh", "# Author: Primitive AI\n# Icon: /mnt/us/chess/custom.png\n"),
            ("Blocks.sh", "# Author: Primitive AI\n"),
        ] {
            fs::write(dir.join(file), script).unwrap();
        }
        let (config, _) = Config::parse(
            "[[app]]\nid = \"Blocks.sh\"\nicon = \"/mnt/us/launcher/icons/blocks-custom.png\"\n",
        );
        let (apps, problems) = discover(&dir, &config);
        assert!(problems.is_empty());
        let icon = |id| &apps.iter().find(|app| app.id == id).unwrap().icon;
        assert_eq!(icon("Notes.sh"), &IconSource::None);
        assert_eq!(icon("focus.sh"), &IconSource::None);
        assert_eq!(icon("Chess.sh"), &IconSource::File("/mnt/us/chess/custom.png".into()));
        assert_eq!(icon("Blocks.sh"), &IconSource::File("/mnt/us/launcher/icons/blocks-custom.png".into()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn store_uses_the_same_icon_before_and_after_installation() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("store-icon");
        let documents = root.join("documents");
        fs::create_dir(&documents).unwrap();
        fs::write(documents.join("Primitive Store.sh"), "# Name: Primitive Store\n").unwrap();
        let icon = |config: &Config| {
            let (apps, problems) = discover(&documents, config);
            assert!(problems.is_empty());
            let store: Vec<_> = apps.iter().filter(|app| is_store_alias(&app.name)).collect();
            assert_eq!(store.len(), 1);
            store[0].icon.clone()
        };
        assert_eq!(icon(&Config::default()), IconSource::Bundled(STORE_ICON));

        let binary = root.join(STORE_BINARY);
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, "placeholder").unwrap();
        fs::set_permissions(binary, fs::Permissions::from_mode(0o755)).unwrap();
        for name in ["Store", "Primitive Store"] {
            fs::write(documents.join(STORE_ENTRY_FILE), format!("# Name: {name}\n")).unwrap();
            assert_eq!(icon(&Config::default()), IconSource::Bundled(STORE_ICON));
        }
        let (config, _) = Config::parse("[[app]]\nid = \"Store.sh\"\nicon = \"/mnt/us/store/custom.png\"\n");
        assert_eq!(icon(&config), IconSource::File("/mnt/us/store/custom.png".into()));
        fs::remove_dir_all(root).unwrap();
    }

    fn script(id: &str, name: &str) -> App {
        App {
            id: id.into(),
            name: name.into(),
            icon: IconSource::None,
            launch: Launch::Script { path: format!("/mnt/us/documents/{id}"), hooks: false },
            dir: "/mnt/us/documents".into(),
            order: None,
            hidden: false,
            background: false,
        }
    }

    #[test]
    fn koreader_runs_through_the_wrapper_unless_apps_toml_says_otherwise() {
        let dir = temp_dir("wrapper");
        let wrapper = dir.join("run-koreader.sh");
        let mut apps = vec![script("KOReader.sh", "KOReader"), script("b.sh", "B")];
        use_koreader_wrapper(&mut apps, &Config::default(), &wrapper);
        assert!(matches!(apps[0].launch, Launch::Script { .. }), "no wrapper file: unchanged");
        fs::write(&wrapper, "#!/bin/sh\n").unwrap();
        use_koreader_wrapper(&mut apps, &Config::default(), &wrapper);
        assert_eq!(apps[0].launch, Launch::Shell { command: format!("sh '{}'", wrapper.display()) });
        assert!(matches!(apps[1].launch, Launch::Script { .. }));
        let mut apps = vec![script("KOReader.sh", "KOReader")];
        let (config, _) = Config::parse("[[app]]\nid = \"KOReader.sh\"\ncommand = \"/k.sh\"\n");
        use_koreader_wrapper(&mut apps, &config, &wrapper);
        assert!(matches!(apps[0].launch, Launch::Script { .. }), "apps.toml's command wins (merge applies it)");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merges_overrides_new_entries_and_sorts() {
        let (config, _) = Config::parse(
            "[[app]]\nid = \"koreader\"\nname = \"KOReader\"\ncommand = \"/mnt/us/koreader/koreader.sh\"\norder = 1\n\
             [[app]]\nid = \"b.sh\"\norder = 2\n[[app]]\nid = \"z.sh\"\nhidden = true\n[[app]]\nid = \"ghost\"\n",
        );
        let (apps, problems) = merge(vec![script("a.sh", "Alpha"), script("b.sh", "Beta"), script("z.sh", "Zed")], &config);
        let ids: Vec<_> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["koreader", "b.sh", "a.sh", "z.sh", "settings", "store"]);
        assert!(apps[3].hidden);
        assert_eq!(apps[0].launch, Launch::Shell { command: "/mnt/us/koreader/koreader.sh".into() });
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn store_uninstall_keeps_scriptlet_preferences_for_reinstall_without_a_warning() {
        use kl_store::{catalog::{Firmware, Manifest}, install::{self, Paths, Source}};

        let root = temp_dir("store-scriptlet-preferences");
        let paths = Paths::under(&root);
        let manifest = Manifest {
            schema: 1,
            id: "primitive-notes".into(),
            dir: "notes".into(),
            version: "1.0.0".into(),
            arch: "armhf".into(),
            firmware: Firmware { min: "5.16.3".into(), max: None },
            requires: Default::default(),
            files: vec!["documents/Notes.sh".into(), "notes/app".into()],
            hooks: Vec::new(),
        };
        let source = Source {
            manifest: &manifest,
            source_url: "https://example.com/notes.zip",
            sha256: "00",
            installed_at: "2026-11-19",
        };
        let install_notes = || {
            for file in &manifest.files {
                let staged = paths.staging.join(&manifest.id).join(file);
                fs::create_dir_all(staged.parent().unwrap()).unwrap();
                fs::write(staged, "#!/bin/sh\n# Name: Notes\n").unwrap();
            }
            install::install(&paths, &source, None).unwrap();
        };
        let preferences = root.join("launcher/apps.toml");
        let saved = "# Keep this preference\n[[app]]\nid = \"Notes.sh\"\nname = \"My Notes\"\norder = 7\nhidden = true\n\
                     [[app]]\nid = \"Manual.sh\"\ncommand = \"/mnt/us/scripts/manual.sh\"\n";
        install_notes();
        fs::write(&preferences, saved).unwrap();
        let discover_library = || {
            let config = Config::load(&fs::read_to_string(&preferences).unwrap()).unwrap();
            discover(&root.join("documents"), &config)
        };
        assert!(discover_library().0.iter().any(|app| app.id == "Notes.sh"));

        install::remove(&paths, "primitive-notes").unwrap();
        let (apps, problems) = discover_library();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(apps.iter().all(|app| app.id != "Notes.sh"));
        assert!(apps.iter().any(|app| app.id == "Manual.sh" && matches!(app.launch, Launch::Shell { .. })));
        assert_eq!(fs::read_to_string(&preferences).unwrap(), saved);

        install_notes();
        let (apps, problems) = discover_library();
        assert!(problems.is_empty(), "{problems:?}");
        let notes = apps.iter().find(|app| app.id == "Notes.sh").unwrap();
        assert_eq!((&*notes.name, notes.order, notes.hidden), ("My Notes", Some(7), true));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn builtins_keep_their_launch() {
        let (config, _) = Config::parse("[[app]]\nid = \"store\"\ncommand = \"rm -rf /\"\nname = \"Shop\"\n");
        let (apps, _) = merge(vec![], &config);
        let store = apps.iter().find(|a| a.id == "store").unwrap();
        assert_eq!(store.launch, Launch::Store);
        assert_eq!(store.name, "Shop");
    }

    #[test]
    fn the_kindle_ui_is_not_a_tile_on_the_home_screen() {
        let (apps, _) = merge(vec![], &Config::default());
        assert!(apps.iter().all(|a| a.launch != Launch::KindleMode), "it is in the power menu instead");
    }

    #[test]
    fn next_file_has_three_lines() {
        let app = script("a.sh", "A");
        assert_eq!(app.next_file().unwrap(), "/mnt/us/documents\nscript\n/mnt/us/documents/a.sh\n");
        let mut hooks = app.clone();
        hooks.launch = Launch::Script { path: "/x.sh".into(), hooks: true };
        assert!(hooks.next_file().unwrap().contains("\nhooks\n"));
        let mut bad = app.clone();
        bad.launch = Launch::Shell { command: "a\nb".into() };
        assert_eq!(bad.next_file(), None);
        assert_eq!(builtins()[0].next_file(), None);
    }
}
