//! `settings.txt`: what the user changes in the launcher's Settings screen. Written by the launcher,
//! one `key=value` per line. Unknown keys and bad values are ignored.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    Upright,
    UpsideDown,
}

impl Rotation {
    /// The turn in quarter turns the browser uses: 0 upright, 2 upside down.
    pub fn turn(self) -> u32 {
        match self {
            Rotation::Upright => 0,
            Rotation::UpsideDown => 2,
        }
    }

    /// The letter the framework uses for its accelerometer property, published for apps in
    /// `/var/tmp/kl/orientation`.
    pub fn letter(self) -> &'static str {
        match self {
            Rotation::Upright => "U",
            Rotation::UpsideDown => "D",
        }
    }

    pub fn flipped(self) -> Rotation {
        match self {
            Rotation::Upright => Rotation::UpsideDown,
            Rotation::UpsideDown => Rotation::Upright,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub rotation: Rotation,
    /// True: the screen follows the device's turn (framework accelerometer). False: `rotation` is fixed.
    pub auto_rotation: bool,
    /// True: the launcher's own sleep screen (the stock screensaver is unloaded). False: the Kindle's.
    pub sleep_screen_launcher: bool,
    /// White on black: the launcher's screens are drawn inverted (picture icons keep their look).
    pub dark_mode: bool,
    /// The opening zoom when an app starts. On by default.
    pub animation: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { rotation: Rotation::Upright, auto_rotation: true, sleep_screen_launcher: false, dark_mode: false, animation: true }
    }
}

impl Settings {
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match (key.trim(), value.trim()) {
                ("rotation", "upright") => s.rotation = Rotation::Upright,
                ("rotation", "upside-down") => s.rotation = Rotation::UpsideDown,
                ("sleep_screen", "launcher") => s.sleep_screen_launcher = true,
                ("auto_rotation", "on") => s.auto_rotation = true,
                ("auto_rotation", "off") => s.auto_rotation = false,
                ("sleep_screen", "kindle") => s.sleep_screen_launcher = false,
                ("dark_mode", value) => s.dark_mode = value == "on",
                ("animation", value) => s.animation = value != "off",
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        let rotation = match self.rotation {
            Rotation::Upright => "upright",
            Rotation::UpsideDown => "upside-down",
        };
        let sleep = if self.sleep_screen_launcher { "launcher" } else { "kindle" };
        let on = |b: bool| if b { "on" } else { "off" };
        format!(
            "rotation={rotation}\nauto_rotation={}\nsleep_screen={sleep}\ndark_mode={}\nanimation={}\n",
            on(self.auto_rotation),
            on(self.dark_mode),
            on(self.animation)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let s = Settings { rotation: Rotation::UpsideDown, auto_rotation: false, sleep_screen_launcher: true, dark_mode: true, animation: false };
        assert_eq!(Settings::parse(&s.to_text()), s);
        assert!(Settings::default().animation && !Settings::default().dark_mode);
        assert!(Settings::parse("animation=yes\n").animation, "only off turns it off");
        assert_eq!(Settings::parse("junk\nrotation=sideways\n"), Settings::default());
    }

    #[test]
    fn auto_rotation_is_on_unless_the_file_says_off() {
        assert!(Settings::default().auto_rotation);
        assert!(Settings::parse("rotation=upright\n").auto_rotation);
        assert!(!Settings::parse("auto_rotation=off\n").auto_rotation);
        assert!(Settings::parse("auto_rotation=on\n").auto_rotation);
    }

    #[test]
    fn turns_and_letters() {
        assert_eq!(Rotation::UpsideDown.turn(), 2);
        assert_eq!(Rotation::Upright.letter(), "U");
        assert_eq!(Rotation::Upright.flipped(), Rotation::UpsideDown);
    }
}
