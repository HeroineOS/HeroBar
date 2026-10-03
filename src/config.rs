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
    /// Icon size in modules (default: font size + 2).
    pub icon_size: Option<i32>,
    /// Space left and right inside each module.
    pub module_padding: Option<i32>,
    /// Space above and below each module's background (islands, hover).
    pub module_margin: Option<i32>,
}

/// One `[modules.<name>]` section. Which keys matter depends on the kind,
/// which comes from the name ("clock", "cpu", "custom/...").
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
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
    /// taskbar: "running", "pinned" or "both"; workspaces: "all" or "occupied"
    pub show: Option<String>,
    /// taskbar: "icons" or "icons-titles"; spacer: "none", "line" or "dots"
    pub style: Option<String>,
    /// taskbar: apps to always show, as .desktop file names ("foot", "firefox-esr")
    pub pinned: Option<Vec<String>>,
    /// taskbar: the most room it takes, in pixels; buttons shrink to fit
    pub max_width: Option<i32>,
    /// taskbar: always take max-width, so other modules never move
    pub fixed_width: Option<bool>,
    /// taskbar, icons-titles: the widest a window button gets
    pub button_width: Option<i32>,
    /// taskbar: windows of "all" workspaces or only the "current" one
    pub workspace: Option<String>,
    /// workspaces: which monitor's workspaces (default: the active one)
    pub output: Option<String>,
    /// spacer: width in pixels
    pub width: Option<i32>,
    /// spacer: take a share of the section's free space (centers what's
    /// between it and the next flexible space)
    pub expand: Option<bool>,
    /// group: its modules, shown together on one background
    pub modules: Option<Vec<String>>,
    /// group: collapsed to its icon; a click shows the modules
    pub drawer: Option<bool>,
    /// Details when the mouse rests on it (default true).
    pub tooltip: Option<bool>,
    /// clock: the first day of the week in its calendar: "monday" or "sunday"
    pub first_weekday: Option<String>,
    /// volume, network, bluetooth, clock: a click opens a popup (default true);
    /// false runs on-click instead. With the popup, on-click is its
    /// "Advanced" button.
    pub popup: Option<bool>,
    /// Size overrides for this module (see [style]).
    pub padding: Option<i32>,
    pub icon_size: Option<i32>,
    pub font_size: Option<i32>,
}

/// A string is shorthand for `{ action = "run-command", arg = "..." }`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Action {
    Command(String),
    Table(ActionTable),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
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

/// The kinds a module name can start with ("cpu", "cpu/2", "custom/x").
pub const KINDS: &str = "clock, cpu, memory, battery, network, volume, bluetooth, taskbar, workspaces, spacer, group/<name>, custom/<name>";

fn check_name(name: &str) -> Result<crate::modules::Kind, String> {
    crate::modules::Kind::from_name(name).ok_or_else(|| format!("unknown module kind '{name}' (built-in: {KINDS}; add /<anything> for more of one kind)"))
}

fn one_of(name: &str, key: &str, v: &Option<String>, allowed: &[&str]) -> Result<(), String> {
    match v {
        Some(v) if !allowed.contains(&v.as_str()) => {
            Err(format!("[modules.\"{name}\"] {key} = \"{v}\": expected {}", allowed.join(", ")))
        }
        _ => Ok(()),
    }
}

pub fn parse(text: &str) -> Result<Config, String> {
    use crate::modules::Kind;
    let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    for name in config.bar.modules_left.iter().chain(&config.bar.modules_center).chain(&config.bar.modules_right) {
        check_name(name)?;
    }
    for (name, m) in &config.modules {
        let kind = check_name(name)?;
        if let Some(a) = &m.on_click {
            if a.command().is_none() {
                return Err(format!("[modules.\"{name}\"] on-click: only the run-command action is supported"));
            }
        }
        match kind {
            Kind::Taskbar => {
                one_of(name, "show", &m.show, &["running", "pinned", "both"])?;
                one_of(name, "style", &m.style, &["icons", "icons-titles"])?;
                one_of(name, "workspace", &m.workspace, &["all", "current"])?;
            }
            Kind::Workspaces => one_of(name, "show", &m.show, &["all", "occupied"])?,
            Kind::Spacer => one_of(name, "style", &m.style, &["none", "line", "dots"])?,
            Kind::Clock => one_of(name, "first-weekday", &m.first_weekday, &["monday", "sunday"])?,
            Kind::Group => {
                for member in m.modules.iter().flatten() {
                    if matches!(check_name(member)?, Kind::Group) {
                        return Err(format!("[modules.\"{name}\"] modules: groups can't contain groups ('{member}')"));
                    }
                }
            }
            _ => {}
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
        assert!(parse("[modules.taskbar]\nstyle = \"big\"").is_err());
        assert!(parse("[modules.\"group/a\"]\nmodules = [\"group/b\"]").is_err());
        assert!(parse("[bar]\nmodules-left = [\"cpu/2\", \"spacer/x\", \"group/sys\"]\n[modules.\"group/sys\"]\nmodules = [\"cpu\", \"memory/big\"]").is_ok());
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_color("#14141c"), Some((0x14, 0x14, 0x1c)));
        assert_eq!(parse_color("#14141c80"), Some((0x14, 0x14, 0x1c)));
        assert_eq!(parse_color("red"), None);
    }
}
