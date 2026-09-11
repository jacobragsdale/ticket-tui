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
//! # team = ["Payments", "Platform"]  # the teams whose areas, members and sprints these are
//! workspace = "~/dev"                              # where clones live
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
//! [notify]                   # a desktop notification when a watched thing moves
//! command = "notify-send {title} {body}"   # left out: nothing is ever run
//!
//! [agents]                   # `w` on a work item: which coding CLI, and how
//! default = "cursor"         # copilot · cursor
//! checkout = "worktree"      # worktree · shared — where the agent's checkout is
//! [agents.copilot]
//! args = []                  # native arguments for that CLI
//!
//! [herdr]                    # where a repository's agent tab goes
//! unmapped_workspace = "Other"
//! [herdr.paths]              # a clone discovery cannot find, by repository name
//! payments-api = "~/src/pay"
//! [[herdr.workspaces]]
//! name = "Payments"
//! repos = ["payments-api", "settlement-worker"]
//! ```
//!
//! Every value here is a default: a flag or a `TICKET_TUI_*` variable still
//! wins over the file, and what none of the three say is left for the Azure
//! CLI to answer. Keys this build does not know are ignored, so the file can
//! grow without an older binary refusing it.

use std::collections::BTreeMap;
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
    /// What says a watched thing has moved, when the file says anything.
    #[serde(default)]
    pub notify: Notify,
    /// Which coding CLI `w` launches on a work item, and how.
    #[serde(default)]
    pub agents: Agents,
    /// Where in Herdr a repository's agent goes.
    #[serde(default)]
    pub herdr: Herdr,
}

/// The `[agents]` table: the provider `w` launches unless asked otherwise,
/// whether each launch gets a git worktree of its own, and the native
/// arguments each provider's CLI is started with.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Agents {
    /// `copilot` or `cursor`; left out, `cursor`.
    #[serde(default)]
    pub default: Option<String>,
    /// `worktree` (the default) or `shared`.
    #[serde(default)]
    pub checkout: Option<String>,
    #[serde(default)]
    pub copilot: ProviderSettings,
    #[serde(default)]
    pub cursor: ProviderSettings,
}

/// What one provider's CLI is started with, after Herdr's own `--`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ProviderSettings {
    #[serde(default)]
    pub args: Vec<String>,
}

/// The `[herdr]` table: which Herdr workspace each repository's tab lives in.
/// A repository no workspace names goes to `unmapped_workspace` when there is
/// one and is asked about otherwise. `paths` says where a clone is when the
/// workspace scan cannot find it.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Herdr {
    #[serde(default)]
    pub unmapped_workspace: Option<String>,
    #[serde(default)]
    pub paths: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub workspaces: Vec<HerdrWorkspace>,
}

/// One `[[herdr.workspaces]]` entry: a workspace, and the repositories whose
/// tabs it holds.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct HerdrWorkspace {
    pub name: String,
    #[serde(default)]
    pub repos: Vec<String>,
}

impl Herdr {
    /// The workspace a repository is routed to: the one that names it, else
    /// the unmapped one, else nothing — which is when the TUI asks.
    #[must_use]
    pub fn workspace_for(&self, repo: &str) -> Option<&str> {
        self.workspaces
            .iter()
            .find(|workspace| {
                workspace
                    .repos
                    .iter()
                    .any(|held| held.eq_ignore_ascii_case(repo))
            })
            .map(|workspace| workspace.name.as_str())
            .or(self.unmapped_workspace.as_deref())
    }

    /// Where a repository's clone is, when the file says so.
    #[must_use]
    pub fn path_for(&self, repo: &str) -> Option<&Path> {
        self.paths
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(repo))
            .map(|(_, path)| path.as_path())
    }

    /// Every workspace name the file knows, in the order it names them.
    #[must_use]
    pub fn workspace_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .workspaces
            .iter()
            .map(|workspace| workspace.name.clone())
            .collect();
        if let Some(unmapped) = &self.unmapped_workspace
            && !names.iter().any(|name| name.eq_ignore_ascii_case(unmapped))
        {
            names.push(unmapped.clone());
        }
        names
    }

    /// Refuses a routing that would send one repository two ways, or names
    /// one workspace twice.
    fn validate(&self) -> Result<()> {
        let mut seen_workspaces: Vec<&str> = Vec::new();
        let mut seen_repos: Vec<(&str, &str)> = Vec::new();
        for workspace in &self.workspaces {
            if workspace.name.trim().is_empty() {
                bail!("herdr.workspaces has a workspace with no name");
            }
            if let Some(twice) = seen_workspaces
                .iter()
                .find(|held| held.eq_ignore_ascii_case(&workspace.name))
            {
                bail!("herdr.workspaces names {twice} twice; merge the two entries");
            }
            seen_workspaces.push(&workspace.name);
            for repo in &workspace.repos {
                if repo.trim().is_empty() {
                    bail!(
                        "herdr.workspaces {} lists a blank repository",
                        workspace.name
                    );
                }
                if let Some((_, other)) = seen_repos
                    .iter()
                    .find(|(held, _)| held.eq_ignore_ascii_case(repo))
                {
                    bail!(
                        "{repo} is routed to both {other} and {}; keep it in one workspace",
                        workspace.name
                    );
                }
                seen_repos.push((repo, &workspace.name));
            }
        }
        if self
            .unmapped_workspace
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            bail!("herdr.unmapped_workspace is blank; give it a value or leave it out");
        }
        Ok(())
    }
}

/// The providers `[agents]` may name, as Herdr spells their kinds.
pub const PROVIDERS: [&str; 2] = ["copilot", "cursor"];

/// The checkout policies `[agents]` may name.
pub const CHECKOUT_POLICIES: [&str; 2] = ["worktree", "shared"];

/// Adds `repo` to the `[[herdr.workspaces]]` entry called `workspace` in the
/// file's own text, or appends such an entry, leaving every other line —
/// comments included — exactly as it was. The result is parsed before it is
/// handed back, so a file this cannot edit is refused rather than broken.
pub fn remember_workspace(source: &str, workspace: &str, repo: &str) -> Result<String> {
    let mut lines: Vec<String> = source.lines().map(str::to_owned).collect();
    let block_start = lines.iter().enumerate().position(|(index, line)| {
        line.trim() == "[[herdr.workspaces]]"
            && lines[index + 1..]
                .iter()
                .take_while(|line| !line.trim().starts_with('['))
                .any(|line| key_value(line, "name").is_some_and(|value| value == workspace))
    });
    let quoted = format!("{repo:?}");
    match block_start {
        Some(start) => {
            let end = lines[start + 1..]
                .iter()
                .position(|line| line.trim().starts_with('['))
                .map_or(lines.len(), |offset| start + 1 + offset);
            let repos_at = (start + 1..end).find(|index| lines[*index].trim().starts_with("repos"));
            match repos_at {
                Some(index) if lines[index].contains(']') => {
                    let line = &lines[index];
                    let open = line.find('[').context("repos is not a list")?;
                    let close = line.rfind(']').context("repos is not a list")?;
                    let inside = line[open + 1..close].trim().trim_end_matches(',');
                    let joined = if inside.is_empty() {
                        quoted
                    } else {
                        format!("{inside}, {quoted}")
                    };
                    lines[index] = format!("{}[{joined}]{}", &line[..open], &line[close + 1..]);
                }
                // ponytail: a multi-line list gets the new name as its first
                // element, right after the opening bracket, which is valid
                // whether or not the last element carries a comma.
                Some(index) => lines.insert(index + 1, format!("    {quoted},")),
                None => {
                    let name_at = (start + 1..end)
                        .find(|index| key_value(&lines[*index], "name").is_some())
                        .unwrap_or(start);
                    lines.insert(name_at + 1, format!("repos = [{quoted}]"));
                }
            }
        }
        None => {
            if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("[[herdr.workspaces]]".to_owned());
            lines.push(format!("name = {workspace:?}"));
            lines.push(format!("repos = [{quoted}]"));
        }
    }
    let mut edited = lines.join("\n");
    edited.push('\n');
    let config = parse(&edited).context("the edited config.toml would not parse")?;
    if config.herdr.workspace_for(repo) != Some(workspace) {
        bail!("the edited config.toml does not route {repo} to {workspace}");
    }
    Ok(edited)
}

/// Writes [`remember_workspace`]'s result over the file, which is made when
/// there is none yet.
pub fn remember_workspace_in(path: &Path, workspace: &str, repo: &str) -> Result<()> {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let edited = remember_workspace(&source, workspace, repo)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(path, edited).with_context(|| format!("writing {}", path.display()))
}

/// The bare string value of a `key = "value"` line, or nothing for any other
/// line.
fn key_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (name, value) = line.split_once('=')?;
    if name.trim() != key {
        return None;
    }
    let value = value.trim();
    let value = value
        .split_once(" #")
        .map_or(value, |(value, _)| value.trim());
    value.strip_prefix('"')?.strip_suffix('"')
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
    /// The teams whose slice of the project this is — one name or a list:
    /// their area paths narrow every pull, their members fill the assignee
    /// picker, and their sprints are what `@current` means.
    #[serde(default, deserialize_with = "one_or_many")]
    pub team: Vec<String>,
    /// Where the Repos tab looks for clones and makes new ones. A leading
    /// `~/` is the home directory.
    #[serde(default)]
    pub workspace: Option<PathBuf>,
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

/// `team = "Payments"` and `team = ["Payments", "Platform"]` both read: one
/// team is the common case and a list is the same thing said twice.
fn one_or_many<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(one) => vec![one],
        OneOrMany::Many(many) => many,
    })
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
    ]
    .into_iter()
    .chain(
        config
            .devops
            .team
            .iter()
            .map(|team| ("devops.team", Some(team.as_str()))),
    ) {
        if value.is_some_and(|value| value.trim().is_empty()) {
            bail!("{key} is blank; give it a value or leave it out");
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    config.devops.workspace = config
        .devops
        .workspace
        .take()
        .map(|path| expand_home(&path, home.clone()));
    if let Some(provider) = config.agents.default.as_deref()
        && !PROVIDERS.contains(&provider)
    {
        bail!(
            "agents.default is {provider:?}; it is one of {}",
            PROVIDERS.join(", ")
        );
    }
    if let Some(policy) = config.agents.checkout.as_deref()
        && !CHECKOUT_POLICIES.contains(&policy)
    {
        bail!(
            "agents.checkout is {policy:?}; it is one of {}",
            CHECKOUT_POLICIES.join(", ")
        );
    }
    config.herdr.validate()?;
    config.herdr.paths = config
        .herdr
        .paths
        .iter()
        .map(|(name, path)| (name.clone(), expand_home(path, home.clone())))
        .collect();
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
    fn the_devops_table_parses_and_a_blank_value_is_refused() {
        let config = parse(
            "[devops]\norg = \"myorg\"\nproject = \"ISTO\"\ncode_project = \"Fiquants\"\nquery = \"[System.Id] > 1\"\n",
        )
        .unwrap();
        assert_eq!(config.devops.org.as_deref(), Some("myorg"));
        assert_eq!(config.devops.project.as_deref(), Some("ISTO"));
        assert_eq!(config.devops.code_project.as_deref(), Some("Fiquants"));
        assert_eq!(config.devops.query.as_deref(), Some("[System.Id] > 1"));

        // One team or a list of them: the same key either way.
        let one = parse("[devops]\nteam = \"Payments\"\n").unwrap();
        assert_eq!(one.devops.team, vec!["Payments".to_owned()]);
        let two = parse("[devops]\nteam = [\"Payments\", \"Platform\"]\n").unwrap();
        assert_eq!(
            two.devops.team,
            vec!["Payments".to_owned(), "Platform".to_owned()]
        );
        assert_eq!(
            format!(
                "{:#}",
                parse("[devops]\nteam = [\"Payments\", \" \"]\n").unwrap_err()
            ),
            "devops.team is blank; give it a value or leave it out"
        );

        // Left out is the whole point: an older file, or one that only paints,
        // says nothing about it.
        let empty = parse("[theme]\n").unwrap();
        assert_eq!(empty.devops, DevOps::default());

        assert_eq!(
            format!("{:#}", parse("[devops]\nproject = \"  \"\n").unwrap_err()),
            "devops.project is blank; give it a value or leave it out"
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

    const ROUTED: &str = r#"
[agents]
default = "cursor"
checkout = "shared"
[agents.cursor]
args = ["--model", "auto"]

[herdr]
unmapped_workspace = "Other"
[herdr.paths]
payments-api = "~/src/pay"

# The money side.
[[herdr.workspaces]]
name = "Payments"
repos = ["payments-api", "settlement-worker"]

[[herdr.workspaces]]
name = "Reporting"
repos = ["reporting-api"]
"#;

    #[test]
    fn repositories_are_routed_to_their_workspace_or_the_unmapped_one() {
        let config = parse(ROUTED).unwrap();
        assert_eq!(config.agents.default.as_deref(), Some("cursor"));
        assert_eq!(config.agents.checkout.as_deref(), Some("shared"));
        assert_eq!(config.agents.cursor.args, ["--model", "auto"]);
        assert!(config.agents.copilot.args.is_empty());
        let herdr = &config.herdr;
        assert_eq!(herdr.workspace_for("payments-api"), Some("Payments"));
        assert_eq!(
            herdr.workspace_for("Settlement-Worker"),
            Some("Payments"),
            "names match without regard to case"
        );
        assert_eq!(herdr.workspace_for("reporting-api"), Some("Reporting"));
        assert_eq!(herdr.workspace_for("chef"), Some("Other"));
        assert_eq!(
            herdr.workspace_names(),
            ["Payments", "Reporting", "Other"],
            "the unmapped workspace is offered last"
        );
        assert!(
            herdr
                .path_for("payments-api")
                .is_some_and(|path| !path.starts_with("~")),
            "a tilde in a path override is the home directory"
        );
        assert_eq!(herdr.path_for("reporting-api"), None);

        let unmapped =
            parse("[[herdr.workspaces]]\nname = \"Payments\"\nrepos = [\"a\"]\n").unwrap();
        assert_eq!(
            unmapped.herdr.workspace_for("b"),
            None,
            "with no unmapped workspace the TUI asks"
        );
        assert_eq!(parse("").unwrap().herdr, Herdr::default());
        assert_eq!(parse("").unwrap().agents, Agents::default());
    }

    #[test]
    fn an_ambiguous_routing_or_an_unknown_provider_is_refused() {
        let twice = "[[herdr.workspaces]]\nname = \"Payments\"\nrepos = [\"pay\"]\n\
                     [[herdr.workspaces]]\nname = \"Reporting\"\nrepos = [\"Pay\"]\n";
        assert_eq!(
            format!("{:#}", parse(twice).unwrap_err()),
            "Pay is routed to both Payments and Reporting; keep it in one workspace"
        );
        let same_name = "[[herdr.workspaces]]\nname = \"Payments\"\n\
                         [[herdr.workspaces]]\nname = \"payments\"\n";
        assert!(format!("{:#}", parse(same_name).unwrap_err()).contains("names Payments twice"),);
        assert!(
            format!(
                "{:#}",
                parse("[agents]\ndefault = \"claude\"\n").unwrap_err()
            )
            .contains("agents.default is \"claude\"; it is one of copilot, cursor")
        );
        assert!(
            format!(
                "{:#}",
                parse("[agents]\ncheckout = \"clone\"\n").unwrap_err()
            )
            .contains("agents.checkout is \"clone\"")
        );
        assert!(
            format!(
                "{:#}",
                parse("[herdr]\nunmapped_workspace = \" \"\n").unwrap_err()
            )
            .contains("herdr.unmapped_workspace is blank")
        );
    }

    #[test]
    fn remembering_a_routing_edits_only_the_workspace_it_names() {
        // An existing single-line list grows by one name; the comment above
        // the block and every other line are untouched.
        let edited = remember_workspace(ROUTED, "Reporting", "report-exporter").unwrap();
        assert!(edited.contains("# The money side."), "{edited}");
        assert!(
            edited.contains("repos = [\"reporting-api\", \"report-exporter\"]"),
            "{edited}"
        );
        assert!(
            edited.contains("repos = [\"payments-api\", \"settlement-worker\"]"),
            "the other workspace is as it was: {edited}"
        );
        assert_eq!(
            parse(&edited)
                .unwrap()
                .herdr
                .workspace_for("report-exporter"),
            Some("Reporting")
        );

        // A workspace the file does not know is appended as a block of its own.
        let appended = remember_workspace(ROUTED, "Platform", "chef").unwrap();
        assert!(
            appended.ends_with("[[herdr.workspaces]]\nname = \"Platform\"\nrepos = [\"chef\"]\n"),
            "{appended}"
        );
        assert_eq!(
            parse(&appended).unwrap().herdr.workspace_for("chef"),
            Some("Platform")
        );

        // A multi-line list takes the name as its first element.
        let multi =
            "[[herdr.workspaces]]\nname = \"Payments\"\nrepos = [\n  \"pay\",\n  \"settle\"\n]\n";
        let edited = remember_workspace(multi, "Payments", "contracts").unwrap();
        assert_eq!(
            parse(&edited).unwrap().herdr.workspaces[0].repos,
            ["contracts", "pay", "settle"]
        );

        // A block with a name and no list yet gets one under the name.
        let bare = "[[herdr.workspaces]]\nname = \"Payments\" # money\n";
        let edited = remember_workspace(bare, "Payments", "pay").unwrap();
        assert_eq!(
            edited,
            "[[herdr.workspaces]]\nname = \"Payments\" # money\nrepos = [\"pay\"]\n"
        );

        // An empty file becomes one block; a routing that would be ambiguous
        // is refused by the parse of the result rather than written.
        let fresh = remember_workspace("", "Payments", "pay").unwrap();
        assert_eq!(
            fresh,
            "[[herdr.workspaces]]\nname = \"Payments\"\nrepos = [\"pay\"]\n"
        );
        let refused = remember_workspace(ROUTED, "Reporting", "payments-api").unwrap_err();
        assert!(
            format!("{refused:#}").contains("routed to both"),
            "{refused:#}"
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        remember_workspace_in(&path, "Payments", "pay").unwrap();
        remember_workspace_in(&path, "Payments", "settle").unwrap();
        assert_eq!(
            load(&path).unwrap().herdr.workspaces[0].repos,
            ["pay", "settle"]
        );
    }
}
