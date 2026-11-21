//! Wi-Fi signal strength for the status bar. `com.lab126.wifid signalStrength` reads like `4/5` (S3,
//! device-notes.md); the Wi-Fi icon has three arcs above its dot.

/// `signalStrength` ("4/5") as (level, out of).
pub fn parse_signal(text: &str) -> Option<(u32, u32)> {
    let (level, max) = text.trim().split_once('/')?;
    let (level, max) = (level.trim().parse().ok()?, max.trim().parse().ok()?);
    (max > 0 && level <= max).then_some((level, max))
}

/// The Wi-Fi address from `ifconfig wlan0` output: busybox writes `inet addr:192.168.1.28`, other tools
/// `inet 192.168.1.28`. Loopback and empty answers give `None`.
pub fn parse_ip(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let rest = line.strip_prefix("inet addr:").or_else(|| line.strip_prefix("inet "));
        if let Some(addr) = rest.and_then(|r| r.split_whitespace().next()).map(|a| a.split('/').next().unwrap_or("")) {
            if addr.chars().all(|c| c.is_ascii_digit() || c == '.') && !addr.is_empty() && !addr.starts_with("127.") {
                return Some(addr.to_string());
            }
        }
    }
    None
}

/// One network: its name, signal ("4" of "5"), whether it needs a password, and whether the Kindle knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub name: String,
    pub level: u32,
    pub max: u32,
    pub secured: bool,
    pub known: bool,
}

/// What the Wi-Fi page shows: the network the Kindle is on (if any) and the other networks it can see, strongest
/// first. `scanning` is set while a scan runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiInfo {
    pub current: Option<Network>,
    pub networks: Vec<Network>,
}

/// The connected network from `com.lab126.wifid`'s report (property `711`): its `AP:` line, then the `Signal:` line
/// under it, as "4/5". Empty when there is no connection.
/// The message for a join that did not connect. `in_range`: the network was in the last scan, so the password is
/// the likely cause; otherwise it is out of range now. `saved`: the network had a saved profile already.
pub fn join_failure(in_range: bool, saved: bool) -> String {
    match (in_range, saved) {
        (false, _) => "it is not in range now: move closer, and try again".into(),
        (true, false) => "it did not connect: check the password. The saved password was put back.".into(),
        (true, true) => "it did not connect: its saved password may have changed. Forget it, and join again.".into(),
    }
}

/// The message for a connection that got no address from the network (DHCP): nothing works without one.
pub const NO_ADDRESS: &str = "connected, but the network gave no address. Try again, or forget it and join again.";

pub fn parse_current(report: &str) -> Option<Network> {
    let mut name = None;
    let mut signal = None;
    let mut secured = false;
    for line in report.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("2.3 AP:") {
            // "name (bssid)": the name is everything before the last " (" (the bssid is in brackets).
            let rest = rest.trim();
            let text = match rest.rfind(" (") {
                Some(i) if rest.ends_with(')') => &rest[..i],
                _ => rest,
            };
            name = Some(text.to_string());
        } else if let Some(rest) = line.strip_prefix("2.3.1") {
            if let Some(value) = rest.trim().strip_prefix("Signal:") {
                signal = parse_signal(value);
            }
        } else if let Some(rest) = line.strip_prefix("2.3.3") {
            secured = !rest.contains("Open") && !rest.trim().is_empty();
        }
    }
    let (level, max) = signal?;
    let name = name.filter(|n| !n.is_empty())?;
    Some(Network { name, level, max, secured, known: true })
}

/// The scan list from `lipc-hash-prop -n com.lab126.wifid scanList`: one `Hash Index` block per network, with
/// `essid`, `secured`, `known`, `signal` and `signal_max` lines. Hidden networks (no name) are left out, and a
/// network seen more than once keeps its strongest reading. Strongest first.
pub fn parse_scan(text: &str) -> Vec<Network> {
    let mut found: Vec<Network> = Vec::new();
    let mut cur: Option<Network> = None;
    let flush = |cur: &mut Option<Network>, found: &mut Vec<Network>| {
        if let Some(n) = cur.take() {
            if !n.name.is_empty() {
                match found.iter_mut().find(|f| f.name == n.name) {
                    Some(f) if (n.level * f.max) > (f.level * n.max) => *f = n,
                    Some(_) => {}
                    None => found.push(n),
                }
            }
        }
    };
    let quoted = |v: &str| v.trim().trim_matches('"').to_string();
    let number = |v: &str| v.trim().trim_start_matches('(').trim_end_matches(')').trim().parse::<u32>().ok();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Hash Index") {
            flush(&mut cur, &mut found);
            cur = Some(Network { name: String::new(), level: 0, max: 5, secured: false, known: false });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let Some(n) = cur.as_mut() else { continue };
        match key.trim() {
            "essid" => n.name = quoted(value),
            "secured" => n.secured = quoted(value) == "yes",
            "known" => n.known = quoted(value) == "yes",
            "signal" => n.level = number(value).unwrap_or(0),
            "signal_max" => n.max = number(value).filter(|m| *m > 0).unwrap_or(5),
            _ => {}
        }
    }
    flush(&mut cur, &mut found);
    found.sort_by(|a, b| (b.level * a.max).cmp(&(a.level * b.max)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    found
}

/// The input `lipc-hash-prop com.lab126.wifid createProfile` reads: one hash with the network's name and security, and
/// for WPA networks the password (`psk`). Quotes and backslashes in values are escaped for the tool's grammar.
/// `password` is `None` for an open network. The result holds a password: never log it.
pub fn profile_input(name: &str, password: Option<&str>) -> String {
    let quote = |v: &str| {
        let mut out = String::from("\"");
        for c in v.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    };
    // The Kindle's profile parser rejects a `store_nw_user_pref` field (a parse error, nothing saved): the tested
    // one-line form without it saves. Keep the input to these fields.
    match password {
        Some(pw) => format!("[{{ essid = {}, secured = \"yes\", psk = {} }}]\n", quote(name), quote(pw)),
        None => format!("[{{ essid = {}, secured = \"no\" }}]\n", quote(name)),
    }
}

/// The saved profiles from `lipc-hash-prop -n com.lab126.wifid profileData`: each network ID with its name. The
/// passwords in the answer are ignored here and are never kept.
pub fn parse_profiles(text: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut netid: Option<u32> = None;
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.trim() {
            "netid" => netid = value.trim().trim_start_matches('(').trim_end_matches(')').parse().ok(),
            "essid" => {
                if let Some(id) = netid.take() {
                    out.push((id, value.trim().trim_matches('"').to_string()));
                }
            }
            _ => {}
        }
    }
    out
}

/// A saved profile with what a join needs to put it back: its network ID, name, security and password (`psk`).
/// The password is kept in memory only, for a rollback; it is never logged or shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedProfile {
    pub id: u32,
    pub name: String,
    pub secured: bool,
    pub psk: Option<String>,
}

/// The saved profiles, with their passwords, from `lipc-hash-prop -n com.lab126.wifid profileData`.
pub fn parse_saved_profiles(text: &str) -> Vec<SavedProfile> {
    let mut out: Vec<SavedProfile> = Vec::new();
    let mut cur: Option<SavedProfile> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Hash Index") {
            if let Some(p) = cur.take() {
                out.push(p);
            }
            cur = Some(SavedProfile { id: 0, name: String::new(), secured: false, psk: None });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let Some(p) = cur.as_mut() else { continue };
        let value = value.trim();
        match key.trim() {
            "netid" => p.id = value.trim_start_matches('(').trim_end_matches(')').parse().unwrap_or(0),
            "essid" => p.name = value.trim_matches('"').to_string(),
            "secured" => p.secured = value.trim_matches('"') == "yes",
            "psk" => p.psk = Some(value.trim_matches('"').to_string()),
            _ => {}
        }
    }
    if let Some(p) = cur {
        out.push(p);
    }
    out.retain(|p| p.id != 0 && !p.name.is_empty());
    out
}

/// How many of the icon's three arcs are lit for a signal: none for no signal, at least one for any.
pub fn arcs(level: u32, max: u32) -> usize {
    if level == 0 || max == 0 {
        return 0;
    }
    ((level * 3).div_ceil(max) as usize).clamp(1, 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_wifid_value() {
        assert_eq!(parse_signal("4/5\n"), Some((4, 5)));
        assert_eq!(parse_signal("0/5"), Some((0, 5)));
        assert_eq!(parse_signal("6/5"), None);
        assert_eq!(parse_signal(""), None);
        assert_eq!(parse_signal("x/5"), None);
    }

    #[test]
    fn reads_the_address_from_ifconfig() {
        let busybox = "wlan0     Link encap:Ethernet  HWaddr F4:03:2A:00:00:00\n          inet addr:192.168.1.28  Bcast:192.168.1.255  Mask:255.255.255.0\n";
        assert_eq!(parse_ip(busybox).as_deref(), Some("192.168.1.28"));
        assert_eq!(parse_ip("    inet 10.0.0.5/24 brd 10.0.0.255\n").as_deref(), Some("10.0.0.5"));
        assert_eq!(parse_ip("    inet 127.0.0.1  Mask:255.0.0.0"), None);
        assert_eq!(parse_ip("wlan0 down"), None);
    }

    #[test]
    fn reads_the_scan_list_strongest_first_without_hidden_or_repeated_networks() {
        let text = r#"Hash Index: 0
	bssid = "aa:01"
	essid = "Cafe"
	secured = "yes"
	known = "no"
	signal = (2)
	signal_max = (5)
Hash Index: 1
	essid = "Home"
	secured = "yes"
	known = "yes"
	signal = (4)
	signal_max = (5)
Hash Index: 2
	essid = ""
	signal = (5)
	signal_max = (5)
Hash Index: 3
	essid = "Cafe"
	secured = "yes"
	signal = (3)
	signal_max = (5)
Hash Index: 4
	essid = "Guest"
	secured = "no"
	known = "no"
	signal = (4)
	signal_max = (5)
"#;
        let nets = parse_scan(text);
        let names: Vec<_> = nets.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Guest", "Home", "Cafe"], "strongest first, then by name; hidden dropped");
        let cafe = nets.iter().find(|n| n.name == "Cafe").unwrap();
        assert_eq!(cafe.level, 3, "the stronger reading of a repeated network is kept");
        assert!(!nets[0].secured && !nets[0].known, "Guest is open and unknown");
        assert!(nets[1].known && nets[1].secured, "Home is saved and locked");
        assert!(parse_scan("nothing here").is_empty());
    }

    #[test]
    fn reads_the_connected_network_from_the_report() {
        let report = "2.2 Wireless: On(1)\n2.3 AP: Cafe Name (6a:96:00:00:00:14)\n2.3.1   Signal: 4/5\n2.3.2   Captive: No(0)\n2.3.3   Security: WPA2-PSK\n";
        let n = parse_current(report).unwrap();
        assert_eq!(n.name, "Cafe Name");
        assert_eq!((n.level, n.max), (4, 5));
        assert!(n.secured);
        assert_eq!(parse_current("2.2 Wireless: On(1)\n"), None);
    }

    #[test]
    fn the_profile_input_escapes_and_keeps_open_networks_free_of_a_password() {
        let open = profile_input("Guest \"cafe\"", None);
        assert_eq!(open, "[{ essid = \"Guest \\\"cafe\\\"\", secured = \"no\" }]\n");
        assert!(!open.contains("store_nw_user_pref"), "{open}");
        let locked = profile_input("Home", Some("pa\\ss\"w0rd"));
        assert!(locked.contains("psk = \"pa\\\\ss\\\"w0rd\""), "{locked}");
        assert!(locked.starts_with("[{ essid = \"Home\", secured = \"yes\""));
    }

    #[test]
    fn reads_the_profile_names_and_ids_without_keeping_passwords() {
        let text = "Hash Index: 0\n\tnetid = (14)\n\tessid = \"Home\"\n\tsecured = \"yes\"\n\tpsk = \"secret1\"\nHash Index: 1\n\tnetid = (2)\n\tessid = \"Guest\"\n";
        assert_eq!(parse_profiles(text), vec![(14, "Home".to_string()), (2, "Guest".to_string())]);
    }

    #[test]
    fn keeps_the_saved_password_for_a_rollback_and_only_there() {
        let text = "Hash Index: 0\n\tnetid = (14)\n\tessid = \"Home\"\n\tsecured = \"yes\"\n\tpsk = \"hunter22\"\nHash Index: 1\n\tnetid = (2)\n\tessid = \"Guest\"\n\tsecured = \"no\"\n";
        let profiles = parse_saved_profiles(text);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0], SavedProfile { id: 14, name: "Home".into(), secured: true, psk: Some("hunter22".into()) });
        assert_eq!(profiles[1].psk, None);
    }

    #[test]
    fn arcs_follow_the_level() {
        let lit: Vec<usize> = (0..=5).map(|l| arcs(l, 5)).collect();
        assert_eq!(lit, [0, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn a_failed_join_says_why_it_failed() {
        assert!(join_failure(false, false).contains("not in range"));
        assert!(join_failure(true, false).contains("check the password"));
        assert!(join_failure(true, true).contains("saved password may have changed"));
        assert!(NO_ADDRESS.contains("no address"));
    }
}
