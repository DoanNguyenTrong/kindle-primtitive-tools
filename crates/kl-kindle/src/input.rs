//! Touch and page-button readers (evdev), adapted from Primitive Browser 0.2.12
//! (`crates/kb-kindle/src/main.rs`). Each runs on its own thread and sends gestures in screen
//! coordinates, already turned for the launcher's rotation.

use std::{
    fs::{self, File},
    io::Read,
    os::unix::io::AsRawFd,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::Sender,
    },
    time::{Duration, Instant},
};

use kl_core::Input;

use crate::{log, profile, Ev};

/// The turn the launcher draws in: 0 upright, 2 upside down. Touch and buttons follow it.
pub static TURN: AtomicU32 = AtomicU32::new(0);

const NATIVE_W: u32 = profile::ACTIVE.screen_w;
const NATIVE_H: u32 = profile::ACTIVE.screen_h;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const KEY_PAGEUP: u16 = profile::ACTIVE.page_back_key;
const KEY_PAGEDOWN: u16 = profile::ACTIVE.page_forward_key;

/// Turn a point from the panel's native (upright) frame into the screen's frame.
pub fn turn_point(turn: u32, x: f32, y: f32) -> (f32, f32) {
    if turn == 2 {
        (NATIVE_W as f32 - 1.0 - x, NATIVE_H as f32 - 1.0 - y)
    } else {
        (x, y)
    }
}

/// `_IOR('E', 0x40 + code, struct input_absinfo)` for the 24-byte absinfo.
const fn eviocgabs(code: u16) -> libc::c_ulong {
    (0x8000_0000u32 | (24 << 16) | (0x45 << 8) | (0x40 + code as u32)) as libc::c_ulong
}

#[repr(C)]
#[derive(Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// The kernel's `struct input_event` on the 32-bit Kindle kernel.
#[repr(C)]
struct InputEvent {
    tv_sec: i32,
    tv_usec: i32,
    kind: u16,
    code: u16,
    value: i32,
}

#[cfg(target_pointer_width = "32")]
const _: () = assert!(std::mem::size_of::<InputEvent>() == 16);

/// Find an input device by part of its kernel name (e.g. "cyttsp", "gpio-keys").
pub fn find(name_hint: &str) -> Option<String> {
    for entry in fs::read_dir("/sys/class/input").ok()?.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.starts_with("event") {
            continue;
        }
        let name = fs::read_to_string(entry.path().join("device/name")).unwrap_or_default();
        if name.trim().contains(name_hint) {
            return Some(format!("/dev/input/{fname}"));
        }
    }
    None
}

fn abs_range(file: &File, code: u16) -> Option<AbsInfo> {
    let mut info = AbsInfo::default();
    // SAFETY: AbsInfo is plain data of the size encoded in the request.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), eviocgabs(code) as _, &mut info as *mut AbsInfo) };
    (rc == 0).then_some(info)
}

fn scale(raw: i32, info: &AbsInfo, out: u32) -> f32 {
    let span = (info.maximum - info.minimum).max(1) as f32;
    ((raw - info.minimum) as f32 / span * (out as f32 - 1.0)).clamp(0.0, out as f32 - 1.0)
}

fn read_event(file: &mut File) -> Option<InputEvent> {
    let mut buf = [0u8; std::mem::size_of::<InputEvent>()];
    file.read_exact(&mut buf).ok()?;
    // SAFETY: InputEvent is repr(C) plain data and buf has its size.
    Some(unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const InputEvent) })
}

/// The gesture for a finished touch: from its start and end points and how long it was held.
pub fn gesture(start: (f32, f32), end: (f32, f32), held: Duration) -> Input {
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let still = dx.abs() < 40.0 && dy.abs() < 40.0;
    if still && held < Duration::from_millis(800) {
        Input::Tap(start.0, start.1)
    } else if still {
        Input::LongTap(start.0, start.1)
    } else {
        Input::Swipe { dx, dy, start_y: start.1 }
    }
}

pub fn touch_loop(path: String, tx: Sender<Ev>) {
    let mut file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => return log(&format!("cannot open {path}: {e}")),
    };
    let (Some(xi), Some(yi)) = (abs_range(&file, ABS_MT_POSITION_X), abs_range(&file, ABS_MT_POSITION_Y)) else {
        return log("touch ranges unavailable");
    };
    log(&format!("touch {path}: x {}..{} y {}..{}", xi.minimum, xi.maximum, yi.minimum, yi.maximum));
    let (mut raw_x, mut raw_y) = (0i32, 0i32);
    let mut active = false;
    let mut start: Option<(f32, f32)> = None;
    let mut last = (0.0f32, 0.0f32);
    let mut down_at = Instant::now();
    loop {
        let Some(ev) = read_event(&mut file) else {
            return log("touch read failed");
        };
        match (ev.kind, ev.code) {
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                if ev.value >= 0 {
                    active = true;
                    start = None;
                    down_at = Instant::now();
                } else if active {
                    active = false;
                    if let Some(s) = start {
                        let _ = tx.send(Ev::Input(gesture(s, last, down_at.elapsed())));
                    }
                }
            }
            (EV_ABS, ABS_MT_POSITION_X) => raw_x = ev.value,
            (EV_ABS, ABS_MT_POSITION_Y) => raw_y = ev.value,
            (EV_SYN, SYN_REPORT) if active => {
                let turn = TURN.load(Ordering::Relaxed);
                last = turn_point(turn, scale(raw_x, &xi, NATIVE_W), scale(raw_y, &yi, NATIVE_H));
                if start.is_none() {
                    start = Some(last);
                }
            }
            _ => {}
        }
    }
}

/// Page buttons: presses only. Upside down, forward and back trade places.
pub fn button_loop(path: String, tx: Sender<Ev>) {
    let mut file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => return log(&format!("cannot open {path}: {e}")),
    };
    log(&format!("buttons {path}"));
    loop {
        let Some(ev) = read_event(&mut file) else {
            return log("button read failed");
        };
        if ev.kind != EV_KEY || ev.value != 1 {
            continue;
        }
        let swapped = TURN.load(Ordering::Relaxed) == 2;
        let input = match (ev.code, swapped) {
            (KEY_PAGEDOWN, false) | (KEY_PAGEUP, true) => Input::PageForward,
            (KEY_PAGEUP, false) | (KEY_PAGEDOWN, true) => Input::PageBack,
            _ => continue,
        };
        let _ = tx.send(Ev::Input(input));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upside_down_turns_touches() {
        assert_eq!(turn_point(0, 10.0, 20.0), (10.0, 20.0));
        assert_eq!(turn_point(2, 0.0, 0.0), (1263.0, 1679.0));
    }

    #[test]
    fn gestures_by_distance_and_time() {
        let short = Duration::from_millis(100);
        assert_eq!(gesture((10.0, 10.0), (20.0, 15.0), short), Input::Tap(10.0, 10.0));
        assert_eq!(gesture((10.0, 10.0), (20.0, 15.0), Duration::from_secs(1)), Input::LongTap(10.0, 10.0));
        assert_eq!(gesture((500.0, 10.0), (100.0, 30.0), short), Input::Swipe { dx: -400.0, dy: 20.0, start_y: 10.0 });
    }
}
