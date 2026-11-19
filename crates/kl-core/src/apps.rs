//! The app list: built-ins, scriptlets found in `documents/`, and the entries and overrides of
//! `apps.toml`, merged in that order.

use std::{fs, path::Path};

use crate::{config::Config, scriptlet};

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
    /// Built-in: Primitive Store (the catalog's apps, install and update).
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
            name: "Store".into(),
            icon: IconSource::None,
            launch: Launch::Store,
            dir: "/".into(),
            order: Some(9_000),
            hidden: false,
            background: false,
        },
        App {
            id: "kindle".into(),
            name: "Kindle UI".into(),
            icon: IconSource::Bundled(KINDLE_UI_ICON),
            launch: Launch::KindleMode,
            dir: "/".into(),
            order: Some(9_001),
            hidden: false,
            background: false,
        },
    ]
}

/// The scriptlets in `documents` (the Kindle's `/mnt/us/documents`), sorted by file name. macOS
/// metadata files (`._*`) and this launcher's own entry are skipped.
pub fn scriptlets(documents: &Path) -> Vec<App> {
    let Ok(dir) = fs::read_dir(documents) else { return Vec::new() };
    let mut files: Vec<_> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sh"))
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("._")))
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
        apps.push(App {
            id: file,
            name: header.name.unwrap_or(stem),
            icon: icon_from(header.icon.as_deref()),
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
/// the problems found.
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
        } else {
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
    merge(scriptlets(documents), config)
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
        assert_eq!(ids, ["koreader", "b.sh", "a.sh", "z.sh", "settings", "store", "kindle"]);
        assert!(apps[3].hidden);
        assert_eq!(apps[0].launch, Launch::Shell { command: "/mnt/us/koreader/koreader.sh".into() });
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn builtins_keep_their_launch() {
        let (config, _) = Config::parse("[[app]]\nid = \"kindle\"\ncommand = \"rm -rf /\"\nname = \"Stock\"\n");
        let (apps, _) = merge(vec![], &config);
        let kindle = apps.iter().find(|a| a.id == "kindle").unwrap();
        assert_eq!(kindle.launch, Launch::KindleMode);
        assert_eq!(kindle.name, "Stock");
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
