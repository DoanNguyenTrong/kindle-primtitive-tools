//! A native app's event loop, screen output, and local persistence.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use kl_core::{Rotation, Settings};
use kl_kindle::{device, display::Display, input, profile, Ev};
use primitive_ui::{Canvas, Input};

pub use kl_kindle::{display, eink, input as kindle_input, profile as kindle_profile};

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

    fn deadline(&self) -> Option<Instant> {
        None
    }

    fn tick(&mut self, _now: Instant) -> Change {
        Change::None
    }

    fn save(&mut self) -> io::Result<()> {
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
}

impl Lifecycle {
    pub fn is_awake(&self) -> bool {
        self.state == State::Awake
    }

    pub fn deadline(&self, app: &impl App) -> Option<Instant> {
        if self.is_awake() {
            app.deadline()
        } else {
            None
        }
    }

    pub fn input(
        &mut self,
        app: &mut impl App,
        input: Input,
        on_save_error: impl FnOnce(io::Error),
    ) -> Change {
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
                if change == Change::Exit {
                    self.close(app, on_save_error);
                }
                change
            }
        }
    }

    /// Persist once when an app exits or its event channel closes.
    pub fn close(&mut self, app: &mut impl App, on_save_error: impl FnOnce(io::Error)) {
        if self.state != State::Closed {
            self.state = State::Closed;
            persist(app, on_save_error);
        }
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
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
            let _ = writeln!(file, "{message}");
        }
    };
    let mut settings = display_settings(
        &fs::read_to_string(launcher.join("settings.txt")).unwrap_or_default(),
        &fs::read_to_string(app_dir.join("settings.txt")).unwrap_or_default(),
    );
    let fonts = run.join("fonts");
    let fonts = if fonts.is_dir() {
        fonts
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
        thread::spawn(move || device::orientation_loop(tx, orientation));
    }
    log("Opened");
    display.show(
        frame(&app, &mut canvas, settings.dark_mode),
        &settings,
        true,
    );

    let mut lifecycle = Lifecycle::default();
    let on_save_error = |error| log(&format!("Could not save: {error}"));
    loop {
        let event = match lifecycle.deadline(&app) {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let change = match event {
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => app.tick(Instant::now()),
            Ok(Ev::Input(input)) => lifecycle.input(&mut app, input, on_save_error),
            Ok(Ev::Turn(rotation)) if settings.auto_rotation && rotation != settings.rotation => {
                settings.rotation = rotation;
                input::TURN.store(rotation.turn(), Ordering::Relaxed);
                let _ = save(&run.join("orientation"), rotation.letter().as_bytes());
                if lifecycle.is_awake() {
                    Change::Full
                } else {
                    Change::None
                }
            }
            _ => Change::None,
        };
        match change {
            Change::None => {}
            Change::Exit => break,
            Change::Partial | Change::Full => {
                let image = frame(&app, &mut canvas, settings.dark_mode);
                display.show(image, &settings, change == Change::Full);
            }
        }
    }
    lifecycle.close(&mut app, on_save_error);
    log("Closed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Demo {
        saves: usize,
        inputs: Vec<Input>,
        save_fails: bool,
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
            if self.save_fails {
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
