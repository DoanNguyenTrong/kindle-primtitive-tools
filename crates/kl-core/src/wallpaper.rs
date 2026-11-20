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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WallpaperSettings {
    pub images: Vec<String>,
    pub home: WallpaperChoice,
    pub sleep: WallpaperChoice,
    pub startup: WallpaperChoice,
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

    pub fn choices(&self, target: WallpaperTarget) -> impl Iterator<Item = WallpaperChoice> + '_ {
        std::iter::once(WallpaperChoice::Default)
            .chain((target == WallpaperTarget::Sleep).then_some(WallpaperChoice::Rotate))
            .chain(self.images.iter().cloned().map(WallpaperChoice::File))
    }
}
