//! KUAL extensions: `menu.json` files under the extensions folder (`/mnt/us/extensions/<name>/menu.json`,
//! beside the documents folder). The launcher shows them behind one "Extensions" tile, as KUAL does: a list
//! with submenus, ordered by `priority` then name. An item with an `action` runs
//! `sh <extension>/<action> <params>` in the extension folder.
//!
//! The menu file is the one the extension's `config.xml` names (`<menu type="json">…</menu>`), else `menu.json`.
//! `exitmenu: false` runs the item in the background: the launcher stays in front and shows its last output
//! line. An `if` of the form `"Model" -m` shows the item on that model, `"Model" -m!` on every other one (the
//! Kindle's model is KindleOasis3; see `OASIS_3_MODEL`). Any other `if` leaves the item out. Not used: `status` and
//! `checked`: their meaning is KUAL's own, and no installed menu on the Kindle sets them.

use crate::apps::{App, IconSource, Launch};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The model name of the Kindle Oasis 3 (KOReader's name for it, which KUAL's `if` checks use).
pub const OASIS_3_MODEL: &str = "KindleOasis3";

/// One entry of an extension menu: a submenu (`children`) or a command.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub name: String,
    /// The shell command for an item that runs something.
    pub command: Option<String>,
    /// The extension folder: the command's working directory.
    pub dir: String,
    pub priority: i64,
    /// KUAL `exitmenu: false`: run without leaving the launcher.
    pub background: bool,
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    pub fn is_submenu(&self) -> bool {
        !self.children.is_empty()
    }

    /// The app the launcher hands to the supervisor to run this item.
    pub fn app(&self) -> Option<App> {
        let command = self.command.clone()?;
        Some(App {
            id: format!("kual:{}:{}", self.dir, self.name),
            name: self.name.clone(),
            icon: IconSource::None,
            launch: Launch::Shell { command },
            dir: self.dir.clone(),
            order: None,
            hidden: false,
            background: self.background,
        })
    }
}

/// The items of one `menu.json`. `ext_id` (the folder name) only labels problems.
pub fn parse(json: &str, ext_dir: &Path, ext_id: &str, model: &str) -> Result<Vec<MenuItem>, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("{ext_id}/menu.json: {e}"))?;
    let list = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{ext_id}/menu.json: no items list"))?;
    Ok(items(list, ext_dir, model))
}

/// Whether a KUAL `if` lets an item show on `model`: `"Model" -m` (that model) and `"Model" -m!` (any other). `None`
/// for any other expression: the item is then left out.
fn model_allows(expr: &str, model: &str) -> Option<bool> {
    let mut parts = expr.split_whitespace();
    let name = parts.next()?.trim_matches('"');
    let op = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    match op {
        "-m" => Some(model == name),
        "-m!" => Some(model != name),
        _ => None,
    }
}

fn items(list: &[Value], dir: &Path, model: &str) -> Vec<MenuItem> {
    let mut out = Vec::new();
    for item in list {
        let Some(name) = item.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) else { continue };
        // The model check first: an item for another model (or an expression not known here) is left out.
        if let Some(expr) = item.get("if").and_then(Value::as_str) {
            if model_allows(expr, model) != Some(true) {
                continue;
            }
        }
        let priority = item.get("priority").and_then(Value::as_i64).unwrap_or(0);
        let children = item.get("items").and_then(Value::as_array).map(|c| items(c, dir, model)).unwrap_or_default();
        let command = if children.is_empty() {
            item.get("action").and_then(Value::as_str).filter(|a| !a.is_empty()).map(|action| {
                let mut command = format!("sh {}", shell_quote(&dir.join(action).to_string_lossy()));
                let params = item.get("params").and_then(Value::as_str).unwrap_or("").trim();
                if !params.is_empty() {
                    command.push(' ');
                    command.push_str(params);
                }
                command
            })
        } else {
            None
        };
        // An entry with neither a submenu nor a command does nothing: leave it out.
        if children.is_empty() && command.is_none() {
            continue;
        }
        let background = item.get("exitmenu").and_then(Value::as_bool) == Some(false);
        out.push(MenuItem { name: name.trim().to_string(), command, dir: dir.to_string_lossy().into_owned(), priority, background, children });
    }
    sort(&mut out);
    out
}

fn sort(items: &mut [MenuItem]) {
    items.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

/// Single-quote a path for `sh -c`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The JSON menu an extension declares in its `config.xml` (`<menu type="json">name</menu>`), else `menu.json`.
fn menu_file(dir: &Path) -> PathBuf {
    if let Ok(xml) = fs::read_to_string(dir.join("config.xml")) {
        let mut rest = xml.as_str();
        while let Some(start) = rest.find("<menu") {
            let after = &rest[start + 5..];
            let Some(close) = after.find('>') else { break };
            let (tag, body) = (&after[..close], &after[close + 1..]);
            // `<menus>` is the list around the entries: skip it.
            if !tag.starts_with('s') && tag.contains("json") {
                if let Some(end) = body.find("</menu>") {
                    let name = body[..end].trim();
                    if !name.is_empty() && !name.contains("..") {
                        return dir.join(name);
                    }
                }
            }
            rest = body;
        }
    }
    dir.join("menu.json")
}

/// The top level of the Extensions list: the items of every extension under `root` with a `menu.json`, in
/// one list. Problems (unreadable or invalid files) are returned as text; the other extensions still load.
pub fn discover_root(root: &Path, model: &str) -> (Vec<MenuItem>, Vec<String>) {
    let mut menu = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = fs::read_dir(root) else { return (menu, problems) };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        let file = menu_file(&dir);
        if !file.is_file() {
            continue;
        }
        let ext_id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match fs::read_to_string(&file) {
            Ok(text) => match parse(&text, &dir, &ext_id, model) {
                Ok(found) => menu.extend(found),
                Err(e) => problems.push(e),
            },
            Err(e) => problems.push(format!("{ext_id}/menu.json: {e}")),
        }
    }
    sort(&mut menu);
    (menu, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MENU: &str = r#"{
        "items": [
            { "name": "Wi-Fi tools", "priority": 5, "items": [
                { "name": "Say hi", "action": "bin/hi.sh", "params": "--loud" },
                { "name": "Reset", "action": "bin/reset.sh", "priority": -1 }
            ]},
            { "name": "Just a menu", "items": [] },
            { "name": "Quoted", "action": "it's.sh", "priority": 1 },
            { "name": "", "action": "x.sh" },
            { "name": "No action" }
        ]
    }"#;

    #[test]
    fn submenus_keep_their_items_in_priority_order() {
        let menu = parse(MENU, Path::new("/mnt/us/extensions/tools"), "tools", OASIS_3_MODEL).unwrap();
        let names: Vec<_> = menu.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Quoted", "Wi-Fi tools"]);
        let tools = &menu[1];
        assert!(tools.is_submenu());
        assert_eq!(tools.command, None);
        let inner: Vec<_> = tools.children.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(inner, ["Reset", "Say hi"]);
    }

    #[test]
    fn commands_quote_paths_and_pass_params() {
        let menu = parse(MENU, Path::new("/e"), "e", OASIS_3_MODEL).unwrap();
        assert_eq!(menu[0].command.as_deref(), Some("sh '/e/it'\\''s.sh'"));
        assert_eq!(menu[1].children[1].command.as_deref(), Some("sh '/e/bin/hi.sh' --loud"));
        let app = menu[1].children[0].app().unwrap();
        assert_eq!(app.launch, Launch::Shell { command: "sh '/e/bin/reset.sh'".into() });
        assert_eq!(app.dir, "/e");
        assert_eq!(menu[1].app(), None);
    }

    #[test]
    fn a_menu_without_items_is_a_problem() {
        let err = parse("{}", Path::new("/e"), "e", OASIS_3_MODEL).unwrap_err();
        assert!(err.contains("no items list"), "{err}");
        assert!(parse("{ not json", Path::new("/e"), "e", OASIS_3_MODEL).is_err());
    }

    #[test]
    fn discover_root_merges_extensions_and_reports_bad_ones() {
        let root = std::env::temp_dir().join(format!("kl-kual-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for d in ["good", "other", "bad", "no-menu"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(root.join("good/menu.json"), MENU).unwrap();
        fs::write(root.join("other/menu.json"), r#"{"items":[{"name":"Alpha","action":"a.sh","priority":1}]}"#).unwrap();
        fs::write(root.join("bad/menu.json"), "[]").unwrap();
        let (menu, problems) = discover_root(&root, OASIS_3_MODEL);
        let names: Vec<_> = menu.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Quoted", "Wi-Fi tools"]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("bad/menu.json"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn config_xml_names_the_menu_file_and_exitmenu_false_runs_in_the_background() {
        let root = std::env::temp_dir().join(format!("kl-kual-xml-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ext")).unwrap();
        fs::write(
            root.join("ext/config.xml"),
            r#"<?xml version="1.0"?><extension><menus><menu type="json" dynamic="true">custom.json</menu></menus></extension>"#,
        )
        .unwrap();
        fs::write(root.join("ext/custom.json"), r#"{"items":[{"name":"Quiet","action":"q.sh","exitmenu":false},{"name":"Loud","action":"l.sh"}]}"#).unwrap();
        let (menu, problems) = discover_root(&root, OASIS_3_MODEL);
        assert!(problems.is_empty(), "{problems:?}");
        let names: Vec<_> = menu.iter().map(|i| (i.name.as_str(), i.background)).collect();
        assert_eq!(names, [("Loud", false), ("Quiet", true)]);
        assert!(menu[1].app().unwrap().background);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_root_is_not_a_problem() {
        let (menu, problems) = discover_root(Path::new("/definitely/not/here"), OASIS_3_MODEL);
        assert!(menu.is_empty() && problems.is_empty());
    }

    #[test]
    fn a_model_check_shows_an_item_on_that_model_only() {
        assert_eq!(model_allows("\"KindleOasis3\" -m", OASIS_3_MODEL), Some(true));
        assert_eq!(model_allows("\"KindleOasis3\" -m!", OASIS_3_MODEL), Some(false));
        assert_eq!(model_allows("\"KindleVoyage\" -m", OASIS_3_MODEL), Some(false));
        assert_eq!(model_allows("\"KindleVoyage\" -m!", OASIS_3_MODEL), Some(true));
        assert_eq!(model_allows("\"KindleOasis3\" -x", OASIS_3_MODEL), None, "not known: left out");
        assert_eq!(model_allows("", OASIS_3_MODEL), None);
    }

    #[test]
    fn items_with_an_if_follow_the_model() {
        let json = r#"{ "items": [
            { "name": "Start KOReader", "if": "\"KindleVoyage\" -m!", "action": "a.sh" },
            { "name": "Oasis only", "if": "\"KindleOasis3\" -m", "action": "b.sh" },
            { "name": "Other models", "if": "\"KindleOasis3\" -m!", "action": "c.sh" },
            { "name": "No check", "action": "d.sh" },
            { "name": "Menu for Voyage", "if": "\"KindleVoyage\" -m", "items": [ { "name": "x", "action": "e.sh" } ] }
        ] }"#;
        let menu = parse(json, Path::new("/e"), "e", OASIS_3_MODEL).unwrap();
        let names: Vec<_> = menu.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["No check", "Oasis only", "Start KOReader"]);
    }

}
