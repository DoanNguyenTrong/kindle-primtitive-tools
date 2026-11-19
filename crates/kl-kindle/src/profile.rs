//! Copied from Primitive Browser 0.2.12 (`crates/kb-kindle/src/profile.rs`).
//!
//! Per-model device values. Only the Kindle Oasis 3 (firmware 5.18.2) is built in, and only that model has
//! been tested. Another model is added as one more `Profile` once its values are read from the device (see
//! docs/DEVICES.md); the rest of the code takes the values from the active profile.

/// The values that differ between Kindle models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub name: &'static str,
    /// Screen size in device pixels, upright.
    pub screen_w: u32,
    pub screen_h: u32,
    /// Part of the touch controller's device name (found in /proc/bus/input/devices).
    pub touch_name: &'static str,
    /// Keycodes of the page-turn buttons: back, then forward.
    pub page_back_key: u16,
    pub page_forward_key: u16,
    /// Battery and charger nodes in sysfs.
    pub battery_capacity: &'static str,
    pub charger_charging: &'static str,
    pub charger_status: &'static str,
    pub charger_online: &'static str,
}

/// The Kindle Oasis 3 on firmware 5.18.2.
pub const OASIS_3: Profile = Profile {
    name: "Kindle Oasis 3 (5.18.2)",
    screen_w: 1264,
    screen_h: 1680,
    touch_name: "cyttsp",
    page_back_key: 104,
    page_forward_key: 109,
    battery_capacity: "/sys/class/power_supply/max77796-battery/capacity",
    charger_charging: "/sys/class/power_supply/max77796-charger/charging",
    charger_status: "/sys/class/power_supply/max77796-charger/status",
    charger_online: "/sys/class/power_supply/max77796-charger/online",
};

/// The profile this build runs with.
pub const ACTIVE: Profile = OASIS_3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oasis_3_values_are_unchanged() {
        assert_eq!((ACTIVE.screen_w, ACTIVE.screen_h), (1264, 1680));
        assert_eq!(ACTIVE.touch_name, "cyttsp");
        assert_eq!((ACTIVE.page_back_key, ACTIVE.page_forward_key), (104, 109));
        assert!(ACTIVE.battery_capacity.ends_with("max77796-battery/capacity"));
        assert!(ACTIVE.charger_online.ends_with("max77796-charger/online"));
    }
}
