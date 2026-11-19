//! Low and critical battery warnings (FR-10: 10 % and 5 %). Thresholds have hysteresis: a warning starts at the
//! threshold and clears only after the level climbs back above a higher line, so the footer does not flicker.
//! Charging clears every warning.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Warning {
    #[default]
    None,
    Low,
    Critical,
}

/// Warning starts at or below these levels (percent).
pub const LOW_AT: u32 = 10;
pub const CRITICAL_AT: u32 = 5;
/// A warning clears only above these levels.
pub const LOW_CLEAR_AT: u32 = 15;
pub const CRITICAL_CLEAR_AT: u32 = 8;

/// The warning for a new reading, given the previous one.
pub fn assess(level: Option<u32>, charging: bool, previous: Warning) -> Warning {
    let level = match level {
        Some(l) if !charging => l,
        _ => return Warning::None,
    };
    match previous {
        Warning::Critical if level < CRITICAL_CLEAR_AT => Warning::Critical,
        _ if level <= CRITICAL_AT => Warning::Critical,
        Warning::Low | Warning::Critical if level < LOW_CLEAR_AT => Warning::Low,
        _ if level <= LOW_AT => Warning::Low,
        _ => Warning::None,
    }
}

/// The footer text for a warning, or `None` when there is nothing to show.
pub fn message(warning: Warning, level: Option<u32>) -> Option<String> {
    let level = level.map(|l| format!(" {l}%")).unwrap_or_default();
    match warning {
        Warning::None => None,
        Warning::Low => Some(format!("Battery low{level}. Connect the charger soon.")),
        Warning::Critical => Some(format!("Battery critical{level}. Connect the charger now.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warning_starts_at_its_threshold() {
        assert_eq!(assess(Some(11), false, Warning::None), Warning::None);
        assert_eq!(assess(Some(10), false, Warning::None), Warning::Low);
        assert_eq!(assess(Some(5), false, Warning::Low), Warning::Critical);
    }

    #[test]
    fn a_warning_clears_only_above_the_higher_line() {
        // Low: stays until 15%, not 11%.
        assert_eq!(assess(Some(12), false, Warning::Low), Warning::Low);
        assert_eq!(assess(Some(15), false, Warning::Low), Warning::None);
        // Critical: falls back to Low at 8%, clears at 15%.
        assert_eq!(assess(Some(9), false, Warning::Critical), Warning::Low);
        assert_eq!(assess(Some(7), false, Warning::Critical), Warning::Critical);
    }

    #[test]
    fn charging_and_unknown_levels_show_nothing() {
        assert_eq!(assess(Some(3), true, Warning::Critical), Warning::None);
        assert_eq!(assess(None, false, Warning::Low), Warning::None);
    }

    #[test]
    fn messages_name_the_level() {
        assert_eq!(message(Warning::None, Some(50)), None);
        assert_eq!(message(Warning::Low, Some(9)).as_deref(), Some("Battery low 9%. Connect the charger soon."));
        assert_eq!(message(Warning::Critical, None).as_deref(), Some("Battery critical. Connect the charger now."));
    }
}
