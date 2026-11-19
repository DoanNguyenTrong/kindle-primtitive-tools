//! The Kindle device layer, shared by Primitive Launcher and Primitive Store: the e-ink panel, the touch and button
//! input, the device's status, and the events an app's loop reads.

pub mod device;
pub mod display;
pub mod eink;
pub mod input;
pub mod profile;

use kl_core::{Input, Rotation};

pub enum Ev {
    Input(Input),
    Tick,
    /// The USB state changed (the cable, or the drive shown on a computer).
    Usb(kl_core::Usb),
    /// Draw the drive-mode screen again: the stock system draws its own USB screen when the drive goes to the computer.
    UsbRedraw,
    /// The device was turned (auto-rotation, after the debouncer).
    Turn(Rotation),
    /// A background app ended: its name, last output line and whether it succeeded.
    Background { name: String, line: String, ok: bool },
    /// A Wi-Fi scan finished.
    Wifi(kl_core::wifi::WifiInfo),
    /// A Wi-Fi join or forget finished: whether it worked, and what to tell the user.
    WifiResult { ok: bool, message: String },
    /// A background SSH job finished (set the password, check the server): what to tell the user.
    Notice(String),
    /// The store's catalog refresh finished: the store's entries again, and what to tell the user.
    StoreRefreshed { entries: Vec<kl_core::store::StoreEntry>, message: String },
    /// An app from the store was installed (or not): the store's entries, the library's apps again, and the message.
    StoreInstalled { entries: Vec<kl_core::store::StoreEntry>, message: String },
    /// The store's install sheet changed stage or crossed another progress step.
    StoreProgress { message: String, percent: Option<u8> },
    /// The launcher finished installing the standalone store.
    StoreBootstrapped(Result<(), String>),
}

pub fn log(msg: &str) {
    eprintln!("[kl] {msg}");
}
