//! `config.toml`: the one file a user — or the `theme` tool — writes to say
//! where the work is and how the TUI should look. It lives in
//! `$XDG_CONFIG_HOME/ticket-tui/` (`~/.config` by default, on macOS too,
//! because that is where every other terminal program keeps its own), and it
//! is optional: a missing file is the default configuration.
//!
//! ```toml
//! [devops]                   # tabs 1 to 4, and every subcommand
//! org = "myorg"              # slug or https://dev.azure.com/myorg
//! project = "ISTO"           # where the work items live
//! code_project = "Fiquants"  # repos, pull requests and pipelines; left out = project
//! query = "[System.AreaPath] UNDER 'ISTO\\Team'"   # optional WIQL scope on every pull
//! workspace = "~/Development"                      # where clones live
//!
//! [azure]                    # tabs 6 ACR and 7 Key Vault
//! subscriptions = ["dev-guid", "qa-guid"]   # left out: whatever `az account show` says
//! registries = ["acrdev", "acrqa"]          # optional: only these, in this order
//! vaults = ["kv-dev", "kv-qa"]              # optional: only these, in this order
//!
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
//!
//! [[clusters]]               # the AKS tab, one table per cluster
//! name = "qa"                # what the tab calls it
//! context = "aks-qa"         # the kubeconfig context kubectl uses
//! namespaces = ["orders"]    # left out or empty: --all-namespaces
//!
//! [notify]                   # a desktop notification when a watched thing moves
//! command = "notify-send {title} {body}"   # left out: nothing is ever run
//! ```
//!
//! Every value here is a default: a flag or a `TICKET_TUI_*` variable still
//! wins over the file, and what none of the three say is left for the Azure
//! CLI to answer. Keys this build does not know are ignored, so the file can
//! grow without an older binary refusing it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ratatui::style::Color;
use serde::{Deserialize, Deserializer};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub theme: ThemeSection,
    /// Where the work items and the code live, when the file says.
    #[serde(default)]
    pub devops: DevOps,
    /// What the ACR and Key Vault tabs read, when the file says.
    #[serde(default)]
    pub azure: Azure,
    /// The clusters the AKS tab reads, in the order the file lists them.
    #[serde(default)]
    pub clusters: Vec<Cluster>,
    /// What says a watched thing has moved, when the file says anything.
    #[serde(default)]
    pub notify: Notify,
}

/// The desktop-notification side of the file. `{title}` and `{body}` are
/// substituted into the command, each as one single-quoted shell word, and it
/// is run through `sh -c`. No table, and nothing is ever run.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Notify {
    #[serde(default)]
    pub command: Option<String>,
}

/// The Azure DevOps side of the file. Everything is optional: what is left out
/// falls back to a flag, a variable, or `az devops configure`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct DevOps {
    /// Organization slug, or the `https://dev.azure.com/...` URL it is in.
    #[serde(default)]
    pub org: Option<String>,
    /// The project the work items live in.
    #[serde(default)]
    pub project: Option<String>,
    /// The project the repositories, pull requests and pipelines live in.
    /// Left out, they live in the project above.
    #[serde(default)]
    pub code_project: Option<String>,
    /// One extra WIQL condition ANDed into every pull.
    #[serde(default)]
    pub query: Option<String>,
    /// Where the Repos tab looks for clones and makes new ones. A leading
    /// `~/` is the home directory.
    #[serde(default)]
    pub workspace: Option<PathBuf>,
}

/// The subscription side of the file: which subscriptions the ACR and Key
/// Vault tabs read, and which of the resources in them are worth listing. An
/// empty list is no opinion at all rather than an empty tab.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Azure {
    /// The subscription ids to read. Left out: whichever one the Azure CLI is
    /// set to.
    #[serde(default)]
    pub subscriptions: Vec<String>,
    /// Only these registries, in this order. Left out: every one the
    /// subscriptions hold.
    #[serde(default)]
    pub registries: Vec<String>,
    /// Only these vaults, in this order. Left out: every one the
    /// subscriptions hold.
    #[serde(default)]
    pub vaults: Vec<String>,
}

/// One cluster the AKS tab reads: what to call it, the kubeconfig context
/// `kubectl` reaches it by, and which namespaces to read.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Cluster {
    pub name: String,
    pub context: String,
    /// Left out or empty, every namespace is read at once.
    #[serde(default)]
    pub namespaces: Vec<String>,
}

impl Cluster {
    /// The namespaces to read, one call each; `None` is all of them in one.
    #[must_use]
    pub fn targets(&self) -> Vec<Option<&str>> {
        if self.namespaces.is_empty() {
            vec![None]
        } else {
            self.namespaces
                .iter()
                .map(|held| Some(held.as_str()))
                .collect()
        }
    }
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

/// A leading `~/` — or a bare `~` — is the home directory; every other path
/// is taken as it was written. Nothing else is expanded: this is one file
/// written by hand, not a shell.
fn expand_home(path: &Path, home: Option<PathBuf>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    }
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
    let mut config: Config =
        toml::from_str(source).map_err(|error| anyhow::anyhow!("{}", error.message()))?;
    // A value written blank is a mistake rather than an opinion: it would
    // otherwise mask the flag, the variable and the CLI default behind it.
    for (key, value) in [
        ("devops.org", config.devops.org.as_deref()),
        ("devops.project", config.devops.project.as_deref()),
        ("devops.code_project", config.devops.code_project.as_deref()),
        ("devops.query", config.devops.query.as_deref()),
        ("notify.command", config.notify.command.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            bail!("{key} is blank; give it a value or leave it out");
        }
    }
    for (key, names) in [
        ("azure.subscriptions", &config.azure.subscriptions),
        ("azure.registries", &config.azure.registries),
        ("azure.vaults", &config.azure.vaults),
    ] {
        if names.iter().any(|name| name.trim().is_empty()) {
            bail!("{key} holds a blank name; give it a value or leave it out");
        }
    }
    config.devops.workspace = config
        .devops
        .workspace
        .take()
        .map(|path| expand_home(&path, std::env::var_os("HOME").map(PathBuf::from)));
    for (index, cluster) in config.clusters.iter().enumerate() {
        if cluster.name.trim().is_empty() || cluster.context.trim().is_empty() {
            bail!("clusters[{index}] needs a name and a context");
        }
        if config.clusters[..index]
            .iter()
            .any(|held| held.name == cluster.name)
        {
            bail!("two clusters are called {:?}", cluster.name);
        }
    }
    Ok(config)
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
    fn clusters_parse_from_the_file_and_no_namespaces_means_all_of_them() {
        let config = parse(
            "[[clusters]]\nname = \"qa\"\ncontext = \"aks-qa\"\nnamespaces = [\"orders\", \"billing\"]\n\n[[clusters]]\nname = \"prod\"\ncontext = \"aks-prod\"\n",
        )
        .unwrap();
        assert_eq!(config.clusters.len(), 2);
        assert_eq!(
            config.clusters[0].targets(),
            vec![Some("orders"), Some("billing")]
        );
        assert_eq!(config.clusters[1].targets(), vec![None]);
        assert_eq!(parse("").unwrap().clusters, Vec::new());
    }

    #[test]
    fn a_cluster_without_a_context_or_a_name_used_twice_names_itself() {
        let error = parse("[[clusters]]\nname = \"qa\"\ncontext = \"\"\n").unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "clusters[0] needs a name and a context"
        );
        let error = parse(
            "[[clusters]]\nname = \"qa\"\ncontext = \"a\"\n[[clusters]]\nname = \"qa\"\ncontext = \"b\"\n",
        )
        .unwrap_err();
        assert_eq!(format!("{error:#}"), "two clusters are called \"qa\"");
    }

    #[test]
    fn the_devops_and_azure_tables_parse_and_a_blank_value_is_refused() {
        let config = parse(
            "[devops]\norg = \"myorg\"\nproject = \"ISTO\"\ncode_project = \"Fiquants\"\nquery = \"[System.Id] > 1\"\n\n[azure]\nsubscriptions = [\"dev\", \"qa\"]\nregistries = [\"acrdev\"]\nvaults = [\"kv-dev\"]\n",
        )
        .unwrap();
        assert_eq!(config.devops.org.as_deref(), Some("myorg"));
        assert_eq!(config.devops.project.as_deref(), Some("ISTO"));
        assert_eq!(config.devops.code_project.as_deref(), Some("Fiquants"));
        assert_eq!(config.devops.query.as_deref(), Some("[System.Id] > 1"));
        assert_eq!(config.azure.subscriptions, ["dev", "qa"]);
        assert_eq!(config.azure.registries, ["acrdev"]);
        assert_eq!(config.azure.vaults, ["kv-dev"]);

        // Left out is the whole point: an older file, or one that only paints,
        // says nothing about either.
        let empty = parse("[theme]\n").unwrap();
        assert_eq!(empty.devops, DevOps::default());
        assert_eq!(empty.azure, Azure::default());

        assert_eq!(
            format!("{:#}", parse("[devops]\nproject = \"  \"\n").unwrap_err()),
            "devops.project is blank; give it a value or leave it out"
        );
        assert_eq!(
            format!(
                "{:#}",
                parse("[azure]\nvaults = [\"kv\", \"\"]\n").unwrap_err()
            ),
            "azure.vaults holds a blank name; give it a value or leave it out"
        );
    }

    #[test]
    fn the_notify_table_parses_and_a_blank_command_is_refused() {
        let config = parse("[notify]\ncommand = \"notify-send {title} {body}\"\n").unwrap();
        assert_eq!(
            config.notify.command.as_deref(),
            Some("notify-send {title} {body}")
        );
        // No table at all is the whole point: nothing is ever run.
        assert_eq!(parse("").unwrap().notify, Notify::default());
        assert_eq!(
            format!("{:#}", parse("[notify]\ncommand = \" \"\n").unwrap_err()),
            "notify.command is blank; give it a value or leave it out"
        );
    }

    /// The file the README tells people to copy is the file this build reads.
    #[test]
    fn the_example_file_parses_and_its_notify_command_is_the_documented_one() {
        let config = parse(include_str!("../config.example.toml")).unwrap();
        let command = config.notify.command.unwrap();
        assert!(
            command.contains("{title}") && command.contains("{body}"),
            "{command}"
        );
    }

    #[test]
    fn a_workspace_written_with_a_tilde_is_the_home_directory() {
        assert_eq!(
            expand_home(Path::new("~/Development"), Some("/home/j".into())),
            PathBuf::from("/home/j/Development")
        );
        assert_eq!(
            expand_home(Path::new("/srv/code"), Some("/home/j".into())),
            PathBuf::from("/srv/code"),
            "an absolute path is taken as written"
        );
        assert_eq!(
            expand_home(Path::new("~/Development"), None),
            PathBuf::from("~/Development"),
            "with no home to expand to, the path stands as written"
        );
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
