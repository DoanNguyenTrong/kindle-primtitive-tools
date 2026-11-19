//! Scriptlet headers: the comment lines at the top of a `documents/*.sh` file that the Kindle's scriptlet
//! support (SH_Integration) reads. Format: https://kindlemodding.org/kindle-dev/scriptlets.html
//!
//! ```text
//! #!/bin/sh
//! # Name: Primitive Browser
//! # Author: Primitive AI
//! # Icon: data:image/png;base64,iVBOR...
//! # UseHooks
//! # DontUseFBInk
//! ```

/// The header of a scriptlet.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: Option<String>,
    pub author: Option<String>,
    /// The `# Icon:` value as written: an absolute path or a `data:` URI.
    pub icon: Option<String>,
    /// `# UseHooks`: the script defines `on_run` (and `on_install`, `on_remove`) instead of running whole.
    pub use_hooks: bool,
    /// This launcher's own library entry carries this marker, so it is not listed as an app.
    pub launcher_entry: bool,
}

/// The marker line in this launcher's own `Launcher.sh`.
pub const LAUNCHER_MARKER: &str = "PrimitiveLauncherEntry";

/// Read the header from a script's text. Only the leading block of comment and blank lines is read;
/// the first command line ends it.
pub fn parse(text: &str) -> Header {
    let mut header = Header::default();
    for raw in text.lines().take(200) {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with("#!") {
            continue;
        }
        let Some(comment) = line.strip_prefix('#') else { break };
        let comment = comment.trim();
        if let Some((key, value)) = comment.split_once(':') {
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "name" if !value.is_empty() => header.name = Some(value.to_string()),
                "author" if !value.is_empty() => header.author = Some(value.to_string()),
                "icon" if !value.is_empty() => header.icon = Some(value.to_string()),
                _ => {}
            }
        } else if comment.eq_ignore_ascii_case("UseHooks") {
            header.use_hooks = true;
        } else if comment == LAUNCHER_MARKER {
            header.launcher_entry = true;
        }
    }
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_header_fields() {
        let text = "#!/bin/sh\n# Name: Primitive Browser\n# Author: Primitive AI\n# Icon: data:image/png;base64,AAAA\n# DontUseFBInk\n#\n# A comment.\n\nDIR=/mnt/us/browser\n# Name: not header\n";
        let h = parse(text);
        assert_eq!(h.name.as_deref(), Some("Primitive Browser"));
        assert_eq!(h.author.as_deref(), Some("Primitive AI"));
        assert_eq!(h.icon.as_deref(), Some("data:image/png;base64,AAAA"));
        assert!(!h.use_hooks);
        assert!(!h.launcher_entry);
    }

    #[test]
    fn handles_crlf_hooks_and_case() {
        let h = parse("#!/bin/sh\r\n# name: KOReader\r\n# usehooks\r\non_run() { :; }\r\n");
        assert_eq!(h.name.as_deref(), Some("KOReader"));
        assert!(h.use_hooks);
    }

    #[test]
    fn a_missing_header_gives_nothing() {
        assert_eq!(parse("echo hi\n"), Header::default());
        assert_eq!(parse("#!/bin/sh\n# Name:\n").name, None);
    }

    #[test]
    fn recognises_the_launcher_entry() {
        assert!(parse("#!/bin/sh\n# Name: Launcher\n# PrimitiveLauncherEntry\n").launcher_entry);
    }
}
