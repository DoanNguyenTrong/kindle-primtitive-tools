//! `apps.toml`: the user's own file for the grid size and for app entries and overrides. The user writes it
//! (over USB or SSH). The launcher rewrites it only to hide or show apps (edit mode, Settings > Hidden apps);
//! that rewrite keeps the known fields and drops comments and unknown keys. Unknown keys are ignored when
//! reading; a bad file gives a notice and the defaults, and the launcher never overwrites a file it cannot read.
//!
//! ```toml
//! columns = 4                            # optional: the grid is 4 × 5 by default
//! rows = 5
//!
//! [[app]]
//! id = "koreader"                       # new entry: needs a command
//! name = "KOReader"
//! command = "/mnt/us/koreader/koreader.sh"
//! icon = "/mnt/us/launcher/icons/koreader.png"
//! dir = "/mnt/us/koreader"
//! order = 1
//!
//! [[app]]
//! id = "Primitive Browser.sh"           # a discovered scriptlet: override only what differs
//! order = 2
//!
//! [[app]]
//! id = "kindle"                         # built-in
//! hidden = true
//! ```
//!
//! A commandless scriptlet override takes effect when its file is present. It can stay in the file after uninstall.

use toml::{Table, Value};

/// One `[[app]]` entry. Every field but `id` is optional.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AppEntry {
    pub id: String,
    pub name: Option<String>,
    pub command: Option<String>,
    pub icon: Option<String>,
    pub dir: Option<String>,
    pub order: Option<i64>,
    pub hidden: Option<bool>,
    /// `mode = "background"` (true) or `"screen"` (false).
    pub background: Option<bool>,
}

/// The grid: 4 columns × 5 rows, as on a phone home screen.
pub const DEFAULT_COLUMNS: u32 = 4;
pub const DEFAULT_ROWS: u32 = 5;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub columns: u32,
    pub rows: u32,
    pub apps: Vec<AppEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Config { columns: DEFAULT_COLUMNS, rows: DEFAULT_ROWS, apps: Vec::new() }
    }
}

impl Config {
    /// Like `parse`, but a file that is not valid TOML is an error: the caller must not write over it.
    pub fn load(text: &str) -> Result<Config, String> {
        text.parse::<Table>().map_err(|e| e.to_string())?;
        Ok(Config::parse(text).0)
    }

    /// The entry for `id`, added when there is none.
    fn entry(&mut self, id: &str) -> &mut AppEntry {
        let index = match self.apps.iter().position(|a| a.id == id) {
            Some(i) => i,
            None => {
                self.apps.push(AppEntry { id: id.into(), ..Default::default() });
                self.apps.len() - 1
            }
        };
        &mut self.apps[index]
    }

    /// Hide or show an app by id.
    pub fn set_hidden(&mut self, id: &str, hidden: bool) {
        self.entry(id).hidden = Some(hidden);
    }

    /// Set an app's position in the grid (edit mode: Move earlier / later).
    pub fn set_order(&mut self, id: &str, order: i64) {
        self.entry(id).order = Some(order);
    }

    /// Set an app's icon file, or clear it (`None`) to go back to the app's own icon.
    pub fn set_icon(&mut self, id: &str, icon: Option<String>) {
        self.entry(id).icon = icon;
    }

    /// The file text for this config: the grid size when it is not the default, then each entry with the
    /// fields that are set.
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        if (self.columns, self.rows) != (DEFAULT_COLUMNS, DEFAULT_ROWS) {
            out = format!("columns = {}\nrows = {}\n", self.columns, self.rows);
        }
        for a in &self.apps {
            out.push_str("\n[[app]]\n");
            out.push_str(&format!("id = {}\n", quote(&a.id)));
            if let Some(v) = &a.name {
                out.push_str(&format!("name = {}\n", quote(v)));
            }
            if let Some(v) = &a.command {
                out.push_str(&format!("command = {}\n", quote(v)));
            }
            if let Some(v) = &a.icon {
                out.push_str(&format!("icon = {}\n", quote(v)));
            }
            if let Some(v) = &a.dir {
                out.push_str(&format!("dir = {}\n", quote(v)));
            }
            if let Some(v) = a.order {
                out.push_str(&format!("order = {v}\n"));
            }
            if let Some(v) = a.hidden {
                out.push_str(&format!("hidden = {v}\n"));
            }
            if let Some(v) = a.background {
                out.push_str(if v { "mode = \"background\"\n" } else { "mode = \"screen\"\n" });
            }
        }
        out
    }

    /// Parse `apps.toml`. Returns the config and the problems found (shown to the user as a notice).
    /// A file that is not valid TOML gives the defaults and one problem.
    pub fn parse(text: &str) -> (Config, Vec<String>) {
        let mut problems = Vec::new();
        let table: Table = match text.parse() {
            Ok(t) => t,
            Err(e) => {
                let first = e.to_string().lines().next().unwrap_or("").to_string();
                return (Config::default(), vec![format!("apps.toml is not valid: {first}")]);
            }
        };
        let mut config = Config::default();
        if let Some(v) = table.get("columns").and_then(Value::as_integer) {
            config.columns = v.clamp(3, 5) as u32;
        }
        if let Some(v) = table.get("rows").and_then(Value::as_integer) {
            config.rows = v.clamp(3, 6) as u32;
        }
        if let Some(list) = table.get("app").and_then(Value::as_array) {
            for (i, item) in list.iter().enumerate() {
                let Some(t) = item.as_table() else {
                    problems.push(format!("apps.toml: app #{} is not a table", i + 1));
                    continue;
                };
                let Some(id) = t.get("id").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else {
                    problems.push(format!("apps.toml: app #{} has no id", i + 1));
                    continue;
                };
                let text = |key: &str| t.get(key).and_then(Value::as_str).map(str::to_string);
                let background = match t.get("mode").and_then(Value::as_str) {
                    Some("background") => Some(true),
                    Some("screen") => Some(false),
                    Some(other) => {
                        problems.push(format!("apps.toml: {}: mode \"{other}\" is not \"screen\" or \"background\"", id.trim()));
                        None
                    }
                    None => None,
                };
                config.apps.push(AppEntry {
                    id: id.trim().to_string(),
                    name: text("name"),
                    command: text("command"),
                    icon: text("icon"),
                    dir: text("dir"),
                    order: t.get("order").and_then(Value::as_integer),
                    hidden: t.get("hidden").and_then(Value::as_bool),
                    background,
                });
            }
        }
        (config, problems)
    }
}

/// A TOML basic string: quotes, backslashes and control characters escaped.
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_grid_and_entries() {
        let (c, p) = Config::parse(
            "columns = 4\nrows = 9\n[[app]]\nid = \"koreader\"\nname = \"KOReader\"\ncommand = \"/k.sh\"\norder = 1\n[[app]]\nid = \"kindle\"\nhidden = true\n",
        );
        assert!(p.is_empty());
        assert_eq!((c.columns, c.rows), (4, 6));
        assert_eq!(c.apps.len(), 2);
        assert_eq!(c.apps[0].command.as_deref(), Some("/k.sh"));
        assert_eq!(c.apps[0].order, Some(1));
        assert_eq!(c.apps[1].hidden, Some(true));
    }

    #[test]
    fn a_bad_file_gives_defaults_and_a_problem() {
        let (c, p) = Config::parse("columns = \n");
        assert_eq!(c, Config::default());
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn entries_without_id_are_reported() {
        let (c, p) = Config::parse("[[app]]\nname = \"x\"\n");
        assert!(c.apps.is_empty());
        assert_eq!(p, vec!["apps.toml: app #1 has no id".to_string()]);
    }

    #[test]
    fn the_writer_round_trips_through_the_parser() {
        let mut config = Config { columns: 3, rows: 4, apps: Vec::new() };
        config.apps.push(AppEntry {
            id: "koreader".into(),
            name: Some("KO \"reader\"\\".into()),
            command: Some("/mnt/us/koreader/koreader.sh --asap".into()),
            order: Some(1),
            ..Default::default()
        });
        config.set_hidden("Primitive Browser.sh", true);
        config.set_hidden("koreader", false);
        let text = config.to_toml();
        let (back, problems) = Config::parse(&text);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(back.columns, 3);
        assert_eq!(back.rows, 4);
        assert_eq!(back.apps.len(), 2);
        assert_eq!(back.apps[0].name.as_deref(), Some("KO \"reader\"\\"));
        assert_eq!(back.apps[0].hidden, Some(false));
        assert_eq!(back.apps[1].id, "Primitive Browser.sh");
        assert_eq!(back.apps[1].hidden, Some(true));
    }

    #[test]
    fn the_default_grid_is_not_written() {
        let mut config = Config::default();
        config.set_hidden("a.sh", true);
        let text = config.to_toml();
        assert!(!text.contains("columns"), "{text}");
        let (back, problems) = Config::parse(&text);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!((back.columns, back.rows), (DEFAULT_COLUMNS, DEFAULT_ROWS));
        assert_eq!(back.apps[0].hidden, Some(true));
    }

    #[test]
    fn set_hidden_changes_an_existing_entry_in_place() {
        let (mut config, _) = Config::parse("[[app]]\nid = \"a.sh\"\norder = 3\n");
        config.set_hidden("a.sh", true);
        assert_eq!(config.apps.len(), 1);
        assert_eq!(config.apps[0].order, Some(3));
        assert_eq!(config.apps[0].hidden, Some(true));
    }

    #[test]
    fn load_refuses_text_that_is_not_toml() {
        assert!(Config::load("columns = [").is_err());
        assert!(Config::load("").is_ok());
        assert!(Config::load("columns = 3\n").is_ok());
    }

    #[test]
    fn order_and_icon_are_saved_and_an_icon_can_be_cleared() {
        let (mut config, _) = Config::parse("[[app]]\nid = \"a.sh\"\nicon = \"/old.png\"\n");
        config.set_order("a.sh", 2);
        config.set_order("b.sh", 1);
        config.set_icon("b.sh", Some("/mnt/us/launcher/icons/b.png".into()));
        let (back, problems) = Config::parse(&config.to_toml());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(back.apps[0].order, Some(2));
        assert_eq!(back.apps[0].icon.as_deref(), Some("/old.png"));
        assert_eq!(back.apps[1].icon.as_deref(), Some("/mnt/us/launcher/icons/b.png"));
        config.set_icon("a.sh", None);
        let (back, _) = Config::parse(&config.to_toml());
        assert_eq!(back.apps[0].icon, None);
    }

    #[test]
    fn mode_is_read_written_and_checked() {
        let (config, problems) = Config::parse("[[app]]\nid = \"a\"\nmode = \"background\"\n[[app]]\nid = \"b\"\nmode = \"later\"\n");
        assert_eq!(config.apps[0].background, Some(true));
        assert_eq!(config.apps[1].background, None);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("mode"), "{problems:?}");
        let (back, _) = Config::parse(&config.to_toml());
        assert_eq!(back.apps[0].background, Some(true));
    }
}
