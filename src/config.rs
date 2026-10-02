//! `bar.toml`: the same conventions as the HeroWM compositor's config
//! (kebab-case keys, `[section]` tables, `{ action, arg }` actions), so it
//! can be edited by hand quickly and read the same way by a settings app.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// The commented default config, also printed by `--print-default-config`.
pub const DEFAULT: &str = include_str!("../res/bar.toml");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub bar: Bar,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub modules: BTreeMap<String, Module>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct Bar {
    pub position: Position,
    pub height: i32,
    pub reserve_space: bool,
    pub padding: i32,
    pub spacing: i32,
    pub modules_left: Vec<String>,
    pub modules_center: Vec<String>,
    pub modules_right: Vec<String>,
    /// Each module on its own background, with see-through gaps between.
    pub islands: bool,
    pub island_style: IslandStyle,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            position: Position::Top,
            height: 34,
            reserve_space: true,
            padding: 6,
            spacing: 4,
            modules_left: Vec::new(),
            modules_center: Vec::new(),
            modules_right: Vec::new(),
            islands: false,
            island_style: IslandStyle::Rounded,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    #[default]
    Top,
    Bottom,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IslandStyle {
    Sharp,
    #[default]
    Rounded,
    Pill,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Style {
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub hover: Option<String>,
    pub font: Option<String>,
    pub font_size: Option<i32>,
}

/// One `[modules.<name>]` section. Which keys matter depends on the kind,
/// which comes from the name ("clock", "cpu", "custom/...").
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Module {
    pub format: Option<String>,
    pub format_disconnected: Option<String>,
    pub interval: Option<f64>,
    pub on_click: Option<Action>,
    /// custom: fixed text
    pub text: Option<String>,
    /// custom: command whose first output line is shown
    pub exec: Option<String>,
    /// battery: e.g. "BAT1" (default: the first one found)
    pub name: Option<String>,
    /// Icon before the text: a built-in icon, a theme icon name or an
    /// image path; "" for none. Built-in kinds pick one by default.
    pub icon: Option<String>,
    /// taskbar: "running", "pinned" or "both"
    pub show: Option<TaskShow>,
    /// taskbar: "icons" (one button per app) or "icons-titles" (one per window)
    pub style: Option<TaskStyle>,
    /// taskbar: apps to always show, as .desktop file names ("foot", "firefox-esr")
    pub pinned: Option<Vec<String>>,
    /// taskbar: the most room it takes, in pixels; buttons shrink to fit
    pub max_width: Option<i32>,
    /// taskbar: always take max-width, so other modules never move
    pub fixed_width: Option<bool>,
    /// taskbar, icons-titles: the widest a window button gets
    pub button_width: Option<i32>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskShow {
    Running,
    Pinned,
    #[default]
    Both,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStyle {
    #[default]
    Icons,
    IconsTitles,
}

/// A string is shorthand for `{ action = "run-command", arg = "..." }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Action {
    Command(String),
    Table(ActionTable),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTable {
    pub action: String,
    pub arg: Option<String>,
}

impl Action {
    /// The command line to run, if this is a run-command action.
    pub fn command(&self) -> Option<&str> {
        match self {
            Action::Command(c) => Some(c),
            Action::Table(t) if t.action == "run-command" => t.arg.as_deref(),
            Action::Table(_) => None,
        }
    }
}

/// `$XDG_CONFIG_HOME/hero/bar.toml`, falling back to `~/.config/hero/bar.toml`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("hero").join("bar.toml"))
}

pub fn parse(text: &str) -> Result<Config, String> {
    let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    for name in config.bar.modules_left.iter().chain(&config.bar.modules_center).chain(&config.bar.modules_right) {
        if crate::modules::Kind::from_name(name).is_none() {
            return Err(format!("unknown module kind '{name}' (built-in: clock, cpu, memory, battery, network, volume, taskbar, custom/<name>)"));
        }
    }
    for (name, m) in &config.modules {
        if let Some(a) = &m.on_click {
            if a.command().is_none() {
                return Err(format!("[modules.\"{name}\"] on-click: only the run-command action is supported"));
            }
        }
    }
    Ok(config)
}

/// Loads `path`, or the default config if it doesn't exist. A broken
/// config is reported and replaced by the default, so the bar still runs.
pub fn load(path: Option<PathBuf>) -> Config {
    let explicit = path.is_some();
    let path = path.or_else(default_path);
    if let Some(p) = &path {
        match std::fs::read_to_string(p) {
            Ok(text) => match parse(&text) {
                Ok(c) => return c,
                Err(e) => eprintln!("herobar: {}: {e}\nherobar: using the default config", p.display()),
            },
            Err(e) if explicit || e.kind() != std::io::ErrorKind::NotFound => {
                eprintln!("herobar: {}: {e}\nherobar: using the default config", p.display())
            }
            Err(_) => {}
        }
    }
    parse(DEFAULT).expect("built-in default config is valid")
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa` (alpha ignored).
pub fn parse_color(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.trim().strip_prefix('#')?;
    let v = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).ok();
    match hex.len() {
        3 => Some((v(0, 1)? * 17, v(1, 1)? * 17, v(2, 1)? * 17)),
        6 | 8 => Some((v(0, 2)?, v(2, 2)?, v(4, 2)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let c = parse(DEFAULT).unwrap();
        assert_eq!(c.bar.modules_center, ["clock"]);
        assert_eq!(c.modules["taskbar"].pinned.as_deref().unwrap_or_default().first().map(String::as_str), Some("foot"));
        assert_eq!(c.bar.island_style, IslandStyle::Rounded);
        assert_eq!(c.modules["custom/menu"].on_click.as_ref().unwrap().command(), Some("heroappearance"));
    }

    #[test]
    fn rejects_unknown_keys_and_modules() {
        assert!(parse("[bar]\nhieght = 3").is_err());
        assert!(parse("[bar]\nmodules-left = [\"weather\"]").is_err());
        assert!(parse("[modules.clock]\non-click = { action = \"quit\" }").is_err());
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_color("#14141c"), Some((0x14, 0x14, 0x1c)));
        assert_eq!(parse_color("#14141c80"), Some((0x14, 0x14, 0x1c)));
        assert_eq!(parse_color("red"), None);
    }
}
