/// Launcher surfaces which can each use a different picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperTarget {
    Home,
    Sleep,
    Startup,
}

impl WallpaperTarget {
    pub fn title(self) -> &'static str {
        match self {
            Self::Home => "Home Screen",
            Self::Sleep => "Sleep Screen",
            Self::Startup => "Startup Screen",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WallpaperChoice {
    #[default]
    Default,
    Rotate,
    File(String),
}

impl WallpaperChoice {
    pub fn label(&self, target: WallpaperTarget) -> &str {
        match self {
            Self::Default => match target {
                WallpaperTarget::Home => "Plain background",
                WallpaperTarget::Sleep => "Kindle screensaver",
                WallpaperTarget::Startup => "Standard startup",
            },
            Self::Rotate => "Rotate sleep pictures",
            Self::File(name) => name,
        }
    }
}

/// Temporary preview values. Changing them does not change the applied wallpaper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallpaperDraft {
    pub target: WallpaperTarget,
    pub choice: WallpaperChoice,
    pub opacity: u8,
    pub error: Option<String>,
}

impl WallpaperDraft {
    pub fn has_opacity(&self) -> bool {
        self.choice != WallpaperChoice::Default
    }

    pub fn can_apply(&self) -> bool {
        !matches!(self.choice, WallpaperChoice::File(_)) || self.error.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallpaperSettings {
    pub images: Vec<String>,
    pub home: WallpaperChoice,
    pub sleep: WallpaperChoice,
    pub startup: WallpaperChoice,
    pub home_opacity: u8,
    pub sleep_opacity: u8,
    pub startup_opacity: u8,
}

impl Default for WallpaperSettings {
    fn default() -> Self {
        Self {
            images: Vec::new(),
            home: WallpaperChoice::Default,
            sleep: WallpaperChoice::Default,
            startup: WallpaperChoice::Default,
            home_opacity: 100,
            sleep_opacity: 100,
            startup_opacity: 100,
        }
    }
}

impl WallpaperSettings {
    pub fn selection(&self, target: WallpaperTarget) -> &WallpaperChoice {
        match target {
            WallpaperTarget::Home => &self.home,
            WallpaperTarget::Sleep => &self.sleep,
            WallpaperTarget::Startup => &self.startup,
        }
    }

    pub fn set_selection(&mut self, target: WallpaperTarget, choice: WallpaperChoice) {
        match target {
            WallpaperTarget::Home => self.home = choice,
            WallpaperTarget::Sleep => self.sleep = choice,
            WallpaperTarget::Startup => self.startup = choice,
        }
    }

    pub fn opacity(&self, target: WallpaperTarget) -> u8 {
        let opacity = match target {
            WallpaperTarget::Home => self.home_opacity,
            WallpaperTarget::Sleep => self.sleep_opacity,
            WallpaperTarget::Startup => self.startup_opacity,
        };
        opacity.min(100)
    }

    pub fn set_opacity(&mut self, target: WallpaperTarget, opacity: u8) {
        let slot = match target {
            WallpaperTarget::Home => &mut self.home_opacity,
            WallpaperTarget::Sleep => &mut self.sleep_opacity,
            WallpaperTarget::Startup => &mut self.startup_opacity,
        };
        *slot = opacity.min(100);
    }

    pub fn choices(&self, target: WallpaperTarget) -> impl Iterator<Item = WallpaperChoice> + '_ {
        std::iter::once(WallpaperChoice::Default)
            .chain((target == WallpaperTarget::Sleep).then_some(WallpaperChoice::Rotate))
            .chain(self.images.iter().cloned().map(WallpaperChoice::File))
    }
}
