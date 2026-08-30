//! `config.toml`: the file a user — or the `theme` tool — writes to shape the
//! TUI. It lives in `$XDG_CONFIG_HOME/ticket-tui/` (`~/.config` by default, on
//! macOS too, because that is where every other terminal program keeps its
//! own), and it is optional: a missing file is the default configuration.
//!
//! ```toml
//! [theme]
//! preset = "custom"          # terminal · terminal-light · mono · custom
//!
//! [theme.custom]             # what `theme apply` writes, in its own words
//! name = "neon-void"
//! appearance = "dark"
//! bg = "#05060a"
//! bg_deep = "#000000"
//! surface = "#0b0d14"
//! overlay = "#171b28"
//! fg = "#dfe6ff"
//! subtle = "#aab4dd"
//! muted = "#626c9c"
//! accent = "#c07cff"
//! red = "#ff5f87"
//! green = "#4ef5a4"
//! yellow = "#ffd75f"
//! blue = "#61a8ff"
//! cyan = "#4fe8ff"
//! orange = "#ff9e5e"
//! teal = "#29e0c8"
//! ```
//!
//! Keys this build does not know are ignored, so the table can grow without
//! an older binary refusing the file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ratatui::style::Color;
use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub theme: ThemeSection,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ThemeSection {
    /// Which theme to paint with, when the file says. Left out, the custom
    /// palette is used if there is one, and the terminal's own colours if not.
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub custom: Option<Palette>,
}

/// Whether a palette is meant for a dark or a light terminal.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

/// One palette in the `theme` tool's vocabulary: the grounds from the window
/// back, three weights of text, one accent, and seven hues.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Palette {
    /// The palette's slug, for the footer to name when it changes.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub appearance: Appearance,
    pub bg: Rgb,
    pub bg_deep: Rgb,
    pub surface: Rgb,
    pub overlay: Rgb,
    pub fg: Rgb,
    pub subtle: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub cyan: Rgb,
    pub orange: Rgb,
    pub teal: Rgb,
}

impl Palette {
    /// What the footer calls this palette.
    #[must_use]
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("custom")
    }
}

/// A `#rrggbb` colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `#rrggbb`, case-insensitively.
    pub fn parse(raw: &str) -> Result<Self> {
        let digits = raw
            .strip_prefix('#')
            .filter(|digits| digits.len() == 6)
            .with_context(|| format!("expected #rrggbb, got {raw:?}"))?;
        let value = u32::from_str_radix(digits, 16)
            .with_context(|| format!("expected #rrggbb, got {raw:?}"))?;
        // Six hex digits fit in three bytes, so every shift below truncates
        // nothing.
        #[allow(clippy::cast_possible_truncation)]
        Ok(Self(
            (value >> 16) as u8,
            (value >> 8 & 0xff) as u8,
            (value & 0xff) as u8,
        ))
    }

    /// The colour `t` of the way from this one to `other`, for the tints a
    /// palette does not name — a hover a shade lighter than the ground.
    #[must_use]
    pub fn mix(self, other: Self, t: f32) -> Self {
        let channel = |from: u8, to: u8| {
            let mixed = f32::from(from) + (f32::from(to) - f32::from(from)) * t;
            // Clamped to a byte before the cast, so nothing is truncated.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let byte = mixed.round().clamp(0.0, 255.0) as u8;
            byte
        };
        Self(
            channel(self.0, other.0),
            channel(self.1, other.1),
            channel(self.2, other.2),
        )
    }
}

impl From<Rgb> for Color {
    fn from(Rgb(r, g, b): Rgb) -> Self {
        Self::Rgb(r, g, b)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// `$XDG_CONFIG_HOME/ticket-tui/config.toml`, or `~/.config/ticket-tui/config.toml`.
#[must_use]
pub fn default_path() -> PathBuf {
    config_home(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .join("ticket-tui")
    .join("config.toml")
}

fn config_home(xdg: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    xdg.filter(|path| path.is_absolute())
        .or_else(|| home.map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

/// Reads the file, or the default configuration when there is none.
pub fn load(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(source) => parse(&source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn parse(source: &str) -> Result<Config> {
    toml::from_str(source).map_err(|error| anyhow::anyhow!("{}", error.message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEON_VOID: &str = r##"
[theme]
preset = "custom"

[theme.custom]
name = "neon-void"
appearance = "dark"
bg = "#05060a"
bg_deep = "#000000"
surface = "#0b0d14"
overlay = "#171b28"
fg = "#dfe6ff"
subtle = "#aab4dd"
muted = "#626c9c"
accent = "#c07cff"
red = "#ff5f87"
green = "#4ef5a4"
yellow = "#ffd75f"
blue = "#61a8ff"
cyan = "#4fe8ff"
orange = "#ff9e5e"
teal = "#29e0c8"
ansi = ["#0b0d14"]
"##;

    #[test]
    fn a_palette_in_the_theme_tools_words_parses() {
        let config = parse(NEON_VOID).unwrap();
        assert_eq!(config.theme.preset.as_deref(), Some("custom"));
        let palette = config.theme.custom.unwrap();
        assert_eq!(palette.label(), "neon-void");
        assert_eq!(palette.appearance, Appearance::Dark);
        assert_eq!(palette.accent, Rgb(0xc0, 0x7c, 0xff));
        assert_eq!(Color::from(palette.bg), Color::Rgb(0x05, 0x06, 0x0a));
    }

    #[test]
    fn an_empty_file_is_the_default_configuration() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(parse("[theme]\n").unwrap(), Config::default());
    }

    #[test]
    fn a_bad_colour_names_itself() {
        let error = parse("[theme.custom]\nbg = \"blue\"\n").unwrap_err();
        assert!(
            format!("{error:#}").contains("expected #rrggbb, got \"blue\""),
            "{error:#}"
        );
        assert!(Rgb::parse("#12345").is_err());
        assert!(Rgb::parse("123456").is_err());
        assert_eq!(Rgb::parse("#ABCDEF").unwrap(), Rgb(0xab, 0xcd, 0xef));
    }

    #[test]
    fn a_missing_file_is_the_default_configuration() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&dir.path().join("config.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn mixing_moves_each_channel_part_of_the_way() {
        assert_eq!(Rgb(0, 0, 0).mix(Rgb(100, 200, 50), 0.5), Rgb(50, 100, 25));
        assert_eq!(Rgb(10, 10, 10).mix(Rgb(20, 20, 20), 0.0), Rgb(10, 10, 10));
        assert_eq!(Rgb(10, 10, 10).mix(Rgb(20, 20, 20), 1.0), Rgb(20, 20, 20));
    }

    #[test]
    fn the_config_home_follows_xdg_then_home() {
        assert_eq!(
            config_home(Some("/x/cfg".into()), Some("/home/j".into())),
            PathBuf::from("/x/cfg")
        );
        assert_eq!(
            config_home(Some("relative".into()), Some("/home/j".into())),
            PathBuf::from("/home/j/.config")
        );
        assert_eq!(config_home(None, None), PathBuf::from(".config"));
    }
}
