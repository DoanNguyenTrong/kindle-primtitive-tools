//! Launcher and Store adapters for the shared device SDK.

use std::{path::Path, sync::mpsc::Sender};

use kl_core::Rotation;

use crate::Ev;

pub use primitive_device::{
    accelerometer_turn, available_storage_bytes, check_ssh, forget_network, free_storage,
    join_network, launcher_dir, local_time, power_event, refresh_screen_timeout, restart_launcher,
    set_autostart, set_light, set_public_dns, set_ssh, set_ssh_password, set_warmth, set_wifi,
    status, system_command, unload_stock_screensaver, usb_state, wifi_info, wlan_ip, LEVEL_MAX,
};

pub fn orientation_loop(tx: Sender<Ev>, start: Rotation) {
    primitive_device::orientation_loop(start, |turn| tx.send(Ev::Turn(turn)).is_ok());
}

pub fn power_loop(tx: Sender<Ev>) {
    primitive_device::power_loop(|input| {
        let _ = tx.send(Ev::Input(input));
    });
}

/// Loading a trusted key opts the device into signed catalog and package verification.
pub fn store_catalog_key() -> std::io::Result<Option<kl_store::signatures::TrustedKey>> {
    #[cfg(target_os = "linux")]
    {
        use std::io::{Error, ErrorKind};
        let Some(bytes) = crate::package_hooks::read_store_policy_file(crate::package_hooks::SIGNING_KEY_FILE, 128)? else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&bytes).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        kl_store::signatures::TrustedKey::from_hex(text.trim())
            .map(Some)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(None)
    }
}

/// Resolve the local Kindle's architecture and firmware version for the Store.
pub fn store_device() -> Option<kl_store::resolve::Device> {
    let text = |path: &str| std::fs::read_to_string(path).ok();
    let firmware = text("/etc/prettyversion.txt")
        .and_then(|t| kl_store::version::firmware_from_text(&t))
        .or_else(|| {
            text("/etc/version.txt").and_then(|t| kl_store::version::firmware_from_text(&t))
        })?;
    let arch = if Path::new("/lib/ld-linux-armhf.so.3").exists() {
        "armhf"
    } else {
        "armel"
    };
    Some(kl_store::resolve::Device {
        arch: arch.to_string(),
        firmware,
    })
}
