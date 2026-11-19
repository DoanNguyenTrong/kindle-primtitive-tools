//! A native app's event loop, screen output, and local persistence.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use kl_core::Settings;
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
    let mut settings =
        Settings::parse(&fs::read_to_string(launcher.join("settings.txt")).unwrap_or_default());
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

    let mut asleep = false;
    let persist = |app: &mut dyn App| {
        if let Err(error) = app.save() {
            log(&format!("Could not save: {error}"));
        }
    };
    loop {
        let deadline = if asleep { None } else { app.deadline() };
        let event = match deadline {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let change = match event {
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => app.tick(Instant::now()),
            Ok(Ev::Input(Input::Sleep)) if !asleep => {
                asleep = true;
                app.input(Input::Sleep);
                persist(&mut app);
                Change::None
            }
            Ok(Ev::Input(Input::Wake)) if asleep => {
                asleep = false;
                app.input(Input::Wake);
                Change::Full
            }
            Ok(Ev::Input(Input::Sleep | Input::Wake)) => Change::None,
            Ok(Ev::Input(input)) if !asleep => app.input(input),
            Ok(Ev::Turn(rotation)) if settings.auto_rotation && rotation != settings.rotation => {
                settings.rotation = rotation;
                input::TURN.store(rotation.turn(), Ordering::Relaxed);
                let _ = save(&run.join("orientation"), rotation.letter().as_bytes());
                if asleep {
                    Change::None
                } else {
                    Change::Full
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
    persist(&mut app);
    log("Closed");
    Ok(())
}
