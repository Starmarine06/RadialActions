//! User settings, kept in `%APPDATA%\RadialActions\settings.toml`.
//!
//! The file is read each time the wheel opens, so the settings window (a
//! separate process) never has to talk to the running app.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    /// "click" or "flick" (release the trigger key over a slice).
    pub mode: String,
    /// "shift", "ctrl", "alt" or "ctrl+shift".
    pub key: String,
    /// How long the cursor must rest before the wheel opens.
    pub delay_ms: u32,
    /// "system", "light", "dark" or "neon".
    pub theme: String,
    pub animate: bool,
    /// Stay out of the way while a game or presentation is full screen.
    pub pause_fullscreen: bool,
    /// Show the move / copy / rename slice.
    pub send: bool,
    pub welcomed: bool,
    /// Pinned destinations for Move to / Copy to.
    pub folders: Vec<String>,
    /// Destinations picked lately, newest first.
    pub recent_folders: Vec<String>,
    /// Per file category: slice order and hidden slices.
    pub wheels: BTreeMap<String, Layout>,
    /// The user's own actions.
    pub custom: Vec<Custom>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Layout {
    pub order: Vec<String>,
    pub hidden: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Custom {
    pub name: String,
    /// Program to start.
    pub program: String,
    /// Arguments, with {file} {files} {dir} {name} {stem} placeholders.
    pub args: String,
    /// "*", "folder", or extensions such as "png, jpg".
    pub types: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: "click".into(),
            key: "shift".into(),
            delay_ms: 150,
            theme: "system".into(),
            animate: true,
            pause_fullscreen: true,
            send: true,
            welcomed: false,
            folders: Vec::new(),
            recent_folders: Vec::new(),
            wheels: BTreeMap::new(),
            custom: Vec::new(),
        }
    }
}

pub fn data_dir() -> Option<PathBuf> {
    // The override keeps tests and screenshots away from the real settings.
    if let Some(d) = std::env::var_os("RADIALACTIONS_DATA") {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("RadialActions"))
}

fn parse(text: &str) -> Option<Settings> {
    toml::from_str::<Settings>(text).ok().map(Settings::sanitised)
}

impl Settings {
    /// Repairs hand-edited values so the app can't be configured into a corner.
    pub fn sanitised(mut self) -> Self {
        if !matches!(self.mode.as_str(), "click" | "flick") {
            self.mode = "click".into();
        }
        if !matches!(self.key.as_str(), "shift" | "ctrl" | "alt" | "ctrl+shift") {
            self.key = "shift".into();
        }
        if !matches!(self.theme.as_str(), "system" | "light" | "dark" | "neon") {
            self.theme = "system".into();
        }
        self.delay_ms = self.delay_ms.clamp(50, 1000);
        self
    }

    pub fn flick(&self) -> bool {
        self.mode == "flick"
    }
}

pub fn load() -> Settings {
    let Some(dir) = data_dir() else { return Settings::default() };
    if let Ok(text) = std::fs::read_to_string(dir.join("settings.toml")) {
        return parse(&text).unwrap_or_default();
    }
    // 1.0 stored only the pick mode.
    let mut s = Settings::default();
    if let Ok(old) = std::fs::read_to_string(dir.join("config.txt")) {
        if old.lines().any(|l| l.trim() == "mode=flick") {
            s.mode = "flick".into();
        }
    }
    s
}

/// A file that no longer parses (a hand edit gone wrong) is set aside before
/// being replaced, so custom actions in it aren't lost.
fn keep_unreadable(file: &std::path::Path) {
    if std::fs::read_to_string(file).is_ok_and(|t| parse(&t).is_none()) {
        let _ = std::fs::copy(file, file.with_extension("broken.toml"));
    }
}

/// Writes through a temporary file so a reader never sees half a file.
pub fn save(s: &Settings) -> Result<(), String> {
    let dir = data_dir().ok_or("No settings folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    keep_unreadable(&dir.join("settings.toml"));
    let text = toml::to_string_pretty(s).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("settings.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("settings.toml")).map_err(|e| e.to_string())
}

/// Loads, changes and saves in one step.
pub fn update(f: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let mut s = load();
    f(&mut s);
    save(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_bad_values_fall_back() {
        let s = parse("theme = \"neon\"\nkey = \"banana\"\ndelay_ms = 5\n").unwrap();
        assert_eq!((s.theme.as_str(), s.key.as_str(), s.delay_ms), ("neon", "shift", 50));
        assert!(s.send && s.animate);
    }

    #[test]
    fn a_broken_file_is_set_aside_not_lost() {
        let d = std::env::temp_dir().join(format!("radialactions_config_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join("settings.toml");
        std::fs::write(&file, "folders = [broken").unwrap();
        keep_unreadable(&file);
        assert!(d.join("settings.broken.toml").exists());
    }

    #[test]
    fn settings_round_trip_through_toml() {
        let mut s = Settings::default();
        s.folders.push("C:\\Photos".into());
        s.wheels.insert("image".into(), Layout { order: vec!["PDF".into()], hidden: vec!["Gray".into()] });
        s.custom.push(Custom { name: "Open".into(), program: "notepad.exe".into(), args: "\"{file}\"".into(), types: "txt".into() });
        assert_eq!(parse(&toml::to_string_pretty(&s).unwrap()).unwrap(), s);
    }
}
