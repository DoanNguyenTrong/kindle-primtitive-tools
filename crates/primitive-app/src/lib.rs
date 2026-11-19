//! A native app's event loop, screen output, and local persistence.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use kl_core::{storage_lease::user_storage_is_mounted, Rotation, Settings, Usb};
use kl_kindle::{device, display::Display, input, profile, Ev};
use primitive_ui::{full_screen_message, Canvas, Input};

mod storage;

pub use kl_kindle::{display, eink, input as kindle_input, profile as kindle_profile};
pub use storage::with_storage;

pub const WIDTH: u32 = profile::ACTIVE.screen_w;
pub const HEIGHT: u32 = profile::ACTIVE.screen_h;

/// The redraw needed after an event. Partial updates use the device driver's pixel comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    None,
    Partial,
    Full,
    Exit,
}

pub trait App {
    fn draw(&self, canvas: &mut Canvas);
    fn input(&mut self, input: Input) -> Change;

    fn press_point(&self, input: Input) -> Option<(f32, f32)> {
        match input {
            Input::Tap(x, y) => Some((x, y)),
            _ => None,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn tick(&mut self, _now: Instant) -> Change {
        Change::None
    }

    fn save(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn storage_returned(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default, PartialEq, Eq)]
enum State {
    #[default]
    Awake,
    Asleep,
    Closed,
}

/// Shared sleep, wake and exit transitions for device and host app drivers.
#[derive(Default)]
pub struct Lifecycle {
    state: State,
    usb_drive: bool,
}

impl Lifecycle {
    pub fn is_awake(&self) -> bool {
        self.state == State::Awake
    }

    pub fn deadline(&self, app: &impl App) -> Option<Instant> {
        if self.is_awake() && !self.usb_drive {
            app.deadline()
        } else {
            None
        }
    }

    fn set_usb_drive(&mut self, active: bool) -> bool {
        let changed = self.usb_drive != active;
        self.usb_drive = active;
        changed
    }

    /// Draw the pressed style before the app receives the action. Normal frames clear it.
    pub fn press_feedback(
        &self,
        app: &impl App,
        canvas: &mut Canvas,
        input: Input,
        dark: bool,
    ) -> Option<Vec<u8>> {
        if !self.is_awake() || self.usb_drive {
            return None;
        }
        let (x, y) = app.press_point(input)?;
        if !canvas.press_at(x, y) {
            return None;
        }
        let pressed_frame = frame(app, canvas, dark);
        canvas.clear_press();
        Some(pressed_frame)
    }

    pub fn input(
        &mut self,
        app: &mut impl App,
        input: Input,
        on_save_error: impl FnOnce(io::Error),
    ) -> Change {
        if self.usb_drive && !matches!(input, Input::Sleep | Input::Wake) {
            return Change::None;
        }
        match (&self.state, input) {
            (State::Awake, Input::Sleep) => {
                self.state = State::Asleep;
                app.input(Input::Sleep);
                persist(app, on_save_error);
                Change::None
            }
            (State::Asleep, Input::Wake) => {
                self.state = State::Awake;
                app.input(Input::Wake);
                Change::Full
            }
            (State::Awake, Input::Wake) | (State::Asleep | State::Closed, _) => Change::None,
            (State::Awake, input) => {
                let change = app.input(input);
                if change == Change::Exit && !self.finish(app, on_save_error) {
                    return Change::None;
                }
                change
            }
        }
    }

    /// Persist once when an app exits or its event channel closes.
    pub fn close(&mut self, app: &mut impl App, on_save_error: impl FnOnce(io::Error)) {
        self.finish(app, on_save_error);
    }

    fn finish(&mut self, app: &mut impl App, on_save_error: impl FnOnce(io::Error)) -> bool {
        if self.state == State::Closed {
            return true;
        }
        if let Err(error) = app.save() {
            let suspended = error.kind() == io::ErrorKind::WouldBlock;
            on_save_error(error);
            // Stay in the app if USB takes the volume before the exit save finishes.
            if suspended {
                return false;
            }
        }
        self.state = State::Closed;
        true
    }
}

fn persist(app: &mut impl App, on_save_error: impl FnOnce(io::Error)) {
    if let Err(error) = app.save() {
        on_save_error(error);
    }
}

pub fn frame(app: &impl App, canvas: &mut Canvas, dark: bool) -> Vec<u8> {
    canvas.begin();
    app.draw(canvas);
    canvas.finish(dark)
}

pub fn font_paths(directory: &Path) -> Vec<String> {
    ["NotoSans-Regular.ttf", "NotoSans-Bold.ttf"]
        .into_iter()
        .map(|font| directory.join(font).to_string_lossy().into_owned())
        .collect()
}

pub fn write_png(
    path: &Path,
    width: u32,
    height: u32,
    gray: &[u8],
) -> Result<(), png::EncodingError> {
    let mut png = png::Encoder::new(io::BufWriter::new(File::create(path)?), width, height);
    png.set_color(png::ColorType::Grayscale);
    png.set_depth(png::BitDepth::Eight);
    png.write_header()?.write_image_data(gray)
}

/// Atomically replace a text or data file without leaving a trailing open handle on user storage.
pub fn save(path: &Path, contents: &[u8]) -> io::Result<()> {
    with_storage(|| write_atomic(path, contents))
}

fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
    let mut temporary_name = name.to_owned();
    temporary_name.push(".tmp");
    let temporary = path.with_file_name(temporary_name);
    let mut file = File::create(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)
}

fn display_settings(launcher: &str, app: &str) -> Settings {
    let mut settings = Settings::parse(launcher);
    for (key, value) in app.lines().filter_map(|line| line.split_once('=')) {
        match (key.trim(), value.trim()) {
            ("rotation", "upright") => settings.rotation = Rotation::Upright,
            ("rotation", "upside-down") => settings.rotation = Rotation::UpsideDown,
            ("auto_rotation", "on") => settings.auto_rotation = true,
            ("auto_rotation", "off") => settings.auto_rotation = false,
            ("dark_mode", "on") => settings.dark_mode = true,
            ("dark_mode", "off") => settings.dark_mode = false,
            _ => {}
        }
    }
    settings
}

/// Use the shared launcher settings and device driver while foregrounded by the launcher.
pub fn run(mut app: impl App, app_dir: &Path) -> Result<(), String> {
    if !cfg!(target_os = "linux") || !Path::new("/dev/fb0").exists() {
        return Err("Use this app's desktop target to simulate it on a computer.".into());
    }
    let launcher =
        std::env::var_os("KL_DIR").map_or_else(|| PathBuf::from("/mnt/us/launcher"), PathBuf::from);
    let run =
        std::env::var_os("KL_RUN").map_or_else(|| PathBuf::from("/var/tmp/kl"), PathBuf::from);
    let log_path = app_dir.join("log-app.txt");
    let log = |message: &str| {
        kl_kindle::log(message);
        let _ = with_storage(|| {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)?;
            writeln!(file, "{message}")
        });
    };
    let mut settings = display_settings(
        &fs::read_to_string(launcher.join("settings.txt")).unwrap_or_default(),
        &fs::read_to_string(app_dir.join("settings.txt")).unwrap_or_default(),
    );
    let ram_fonts = run.join("fonts");
    let fonts_in_ram = ram_fonts.is_dir();
    let fonts = if fonts_in_ram {
        ram_fonts
    } else {
        launcher.join("fonts")
    };
    let mut canvas = Canvas::new(WIDTH, HEIGHT, &font_paths(&fonts))?;
    let mut display = Display::open();

    if settings.auto_rotation {
        settings.rotation = device::accelerometer_turn().unwrap_or(settings.rotation);
    }
    input::TURN.store(settings.rotation.turn(), Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    if let Some(path) = input::find(profile::ACTIVE.touch_name) {
        let events = tx.clone();
        thread::spawn(move || input::touch_loop(path, events));
    }
    if let Some(path) = input::find("gpio-keys") {
        let events = tx.clone();
        thread::spawn(move || input::button_loop(path, events));
    }
    {
        let events = tx.clone();
        thread::spawn(move || device::power_loop(events));
    }
    if settings.auto_rotation {
        let orientation = settings.rotation;
        let events = tx.clone();
        thread::spawn(move || device::orientation_loop(events, orientation));
    }
    log("Opened");
    display.show(
        frame(&app, &mut canvas, settings.dark_mode),
        &settings,
        true,
    );

    // A fallback font source may map a file from user storage, so keep the startup pause in that case.
    let managed_storage = fonts_in_ram
        && storage::enable(&run).unwrap_or_else(|error| {
            kl_kindle::log(&format!("Could not enable idle USB: {error}"));
            false
        });
    if managed_storage {
        let events = tx.clone();
        thread::spawn(move || watch_usb(events));
    }
    drop(tx);

    let mut lifecycle = Lifecycle::default();
    let on_save_error = |error| kl_kindle::log(&format!("Could not save: {error}"));
    loop {
        let event = match lifecycle.deadline(&app) {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let usb_drive = managed_storage && !user_storage_is_mounted();
        let usb_changed = lifecycle.set_usb_drive(usb_drive);
        let usb_redraw = matches!(event, Ok(Ev::UsbRedraw));
        let mut pressed = false;
        let change = match event {
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) if !usb_drive => app.tick(Instant::now()),
            Ok(Ev::Input(input)) => {
                if !usb_changed {
                    if let Some(image) =
                        lifecycle.press_feedback(&app, &mut canvas, input, settings.dark_mode)
                    {
                        display.show(image, &settings, false);
                        pressed = true;
                    }
                }
                lifecycle.input(&mut app, input, on_save_error)
            }
            Ok(Ev::Turn(rotation)) if settings.auto_rotation && rotation != settings.rotation => {
                settings.rotation = rotation;
                input::TURN.store(rotation.turn(), Ordering::Relaxed);
                // The supervisor's orientation file is in RAM and needs no flash lease.
                let _ = write_atomic(&run.join("orientation"), rotation.letter().as_bytes());
                if lifecycle.is_awake() {
                    Change::Full
                } else {
                    Change::None
                }
            }
            _ => Change::None,
        };
        if usb_changed && !usb_drive {
            match app.storage_returned() {
                Ok(()) if storage::pending_save() => persist(&mut app, on_save_error),
                Ok(()) => {}
                Err(error) => kl_kindle::log(&format!("Could not reload Kindle storage: {error}")),
            }
        }
        if usb_drive {
            if lifecycle.is_awake() && (usb_changed || usb_redraw || change == Change::Full) {
                canvas.begin();
                full_screen_message(
                    &mut canvas,
                    "USB drive",
                    "Eject the Kindle from your computer to return to the app.",
                );
                display.show(canvas.finish(settings.dark_mode), &settings, true);
            }
            continue;
        }
        let change = if lifecycle.is_awake() && usb_changed && change != Change::Exit {
            Change::Full
        } else {
            change
        };
        match change {
            Change::None if !pressed => {}
            Change::Exit => break,
            Change::None | Change::Partial | Change::Full => {
                let image = frame(&app, &mut canvas, settings.dark_mode);
                display.show(image, &settings, change == Change::Full);
            }
        }
    }
    lifecycle.close(&mut app, on_save_error);
    log("Closed");
    Ok(())
}

fn watch_usb(events: mpsc::Sender<Ev>) {
    let mut attached = false;
    let mut redraw = false;
    loop {
        let current = !user_storage_is_mounted();
        let event = if current != attached {
            attached = current;
            redraw = current;
            Some(Ev::Usb(if current { Usb::Drive } else { Usb::Unplugged }))
        } else if redraw {
            // The stock USB screen can appear just after the first redraw.
            redraw = false;
            Some(Ev::UsbRedraw)
        } else {
            None
        };
        if event.is_some_and(|event| events.send(event).is_err()) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Demo {
        saves: usize,
        inputs: Vec<Input>,
        save_fails: bool,
        storage_unavailable: bool,
    }

    impl App for Demo {
        fn draw(&self, _canvas: &mut Canvas) {}

        fn input(&mut self, input: Input) -> Change {
            self.inputs.push(input);
            if input == Input::PageBack {
                Change::Exit
            } else {
                Change::Partial
            }
        }

        fn deadline(&self) -> Option<Instant> {
            Some(Instant::now())
        }

        fn save(&mut self) -> io::Result<()> {
            self.saves += 1;
            if self.storage_unavailable {
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "storage exported",
                ))
            } else if self.save_fails {
                Err(io::Error::other("disk full"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn lifecycle_saves_at_sleep_and_exit_and_suppresses_sleeping_input() {
        let mut demo = Demo::default();
        let mut lifecycle = Lifecycle::default();
        let on_error = |error| panic!("unexpected save failure: {error}");

        assert!(lifecycle.deadline(&demo).is_some());
        assert_eq!(
            lifecycle.input(&mut demo, Input::Sleep, on_error),
            Change::None
        );
        assert!(lifecycle.deadline(&demo).is_none());
        assert_eq!(demo.saves, 1);
        assert_eq!(
            lifecycle.input(&mut demo, Input::Sleep, on_error),
            Change::None
        );
        assert_eq!(
            lifecycle.input(&mut demo, Input::Tap(12.0, 12.0), on_error),
            Change::None
        );
        assert_eq!(
            lifecycle.input(&mut demo, Input::Wake, on_error),
            Change::Full
        );
        assert!(lifecycle.deadline(&demo).is_some());
        assert_eq!(
            lifecycle.input(&mut demo, Input::Wake, on_error),
            Change::None
        );
        assert_eq!(
            lifecycle.input(&mut demo, Input::PageBack, on_error),
            Change::Exit
        );
        lifecycle.close(&mut demo, on_error);
        assert_eq!(demo.saves, 2);
        assert_eq!(demo.inputs, [Input::Sleep, Input::Wake, Input::PageBack]);
    }

    #[test]
    fn lifecycle_reports_save_failure_and_completes_transition() {
        let mut demo = Demo {
            save_fails: true,
            ..Demo::default()
        };
        let mut lifecycle = Lifecycle::default();
        let mut errors = Vec::new();
        assert_eq!(
            lifecycle.input(&mut demo, Input::Sleep, |e| errors.push(e)),
            Change::None
        );
        assert!(!lifecycle.is_awake());
        assert_eq!(errors[0].to_string(), "disk full");
    }

    #[test]
    fn drive_mode_suspends_input_and_deadlines_but_preserves_power_events() {
        let mut demo = Demo::default();
        let mut lifecycle = Lifecycle::default();
        let on_error = |error| panic!("unexpected save failure: {error}");
        assert!(lifecycle.set_usb_drive(true));
        assert!(lifecycle.deadline(&demo).is_none());
        assert_eq!(
            lifecycle.input(&mut demo, Input::PageBack, on_error),
            Change::None
        );
        assert_eq!(
            lifecycle.input(&mut demo, Input::Sleep, on_error),
            Change::None
        );
        assert!(!lifecycle.is_awake());
        assert_eq!(
            lifecycle.input(&mut demo, Input::Wake, on_error),
            Change::Full
        );
        assert!(lifecycle.is_awake());
        assert!(lifecycle.deadline(&demo).is_none());
        assert_eq!(demo.inputs, [Input::Sleep, Input::Wake]);
        assert!(lifecycle.set_usb_drive(false));
        assert!(lifecycle.deadline(&demo).is_some());
    }

    #[test]
    fn an_exit_save_losing_storage_keeps_the_session_alive() {
        let mut demo = Demo {
            storage_unavailable: true,
            ..Demo::default()
        };
        let mut lifecycle = Lifecycle::default();
        let on_error = |error: io::Error| assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            lifecycle.input(&mut demo, Input::PageBack, on_error),
            Change::None
        );
        assert!(lifecycle.is_awake());
        demo.storage_unavailable = false;
        assert_eq!(
            lifecycle.input(&mut demo, Input::PageBack, on_error),
            Change::Exit
        );
        assert!(!lifecycle.is_awake());
        assert_eq!(demo.saves, 2);
    }

    #[test]
    fn per_app_settings_override_only_documented_display_preferences() {
        let launcher = "rotation=upside-down\nauto_rotation=off\ndark_mode=on\nstore_daily_check=0";
        let inherited = display_settings(launcher, "");
        assert_eq!(inherited.rotation, Rotation::UpsideDown);
        assert!(inherited.dark_mode);
        assert!(!inherited.auto_rotation);
        let overridden = display_settings(
            launcher,
            "rotation=upright\nauto_rotation=on\ndark_mode=invalid\nstore_daily_check=1",
        );
        assert_eq!(overridden.rotation, Rotation::Upright);
        assert!(overridden.auto_rotation);
        assert!(overridden.dark_mode);
        assert!(!overridden.store_daily_check);
        assert!(!display_settings(launcher, "dark_mode=off").dark_mode);
    }
}
