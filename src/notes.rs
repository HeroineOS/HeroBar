//! The `notifications` module: HeroNotify's history and do-not-disturb.
//!
//! HeroNotify keeps the history in `~/.local/state/hero/notifications.jsonl`
//! (one notification per line, newest last) and the newest id seen in
//! `notifications.json`; they're re-read only when they change (checked
//! with the heartbeat). Changes go through the `heronotify` command
//! (clear, seen, dnd), which holds HeroNotify's lock while writing.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::Deserialize;

/// A notification, as HeroNotify keeps it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Note {
    pub id: u32,
    pub app: String,
    pub entry: String,
    pub icon: String,
    pub summary: String,
    pub body: String,
    pub urgency: u8,
    pub time: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Seen {
    seen: u32,
}

#[derive(Default)]
pub struct Notes {
    /// Newest first.
    pub list: Vec<Note>,
    pub unread: usize,
    /// Do-not-disturb is on now (by hand or by a schedule).
    pub dnd: bool,
    /// ... by hand (the switch's state).
    pub dnd_manual: bool,
    /// HeroNotify is installed.
    pub installed: bool,
    stamps: [Option<SystemTime>; 2],
    /// The settings file's time and the minute DND was last asked about.
    dnd_asked: Option<(Option<SystemTime>, u64)>,
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("hero")
}

pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("hero/notifications.toml")
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Notes {
    /// Re-reads the history if it changed; true if it did.
    pub fn reload(&mut self) -> bool {
        let (h, s) = (state_dir().join("notifications.jsonl"), state_dir().join("notifications.json"));
        let stamps = [mtime(&h), mtime(&s)];
        if stamps == self.stamps && self.installed {
            return false;
        }
        self.installed = crate::system::have("heronotify");
        self.stamps = stamps;
        let mut list: Vec<Note> = std::fs::read_to_string(h).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        list.reverse();
        let seen = std::fs::read_to_string(s).ok().and_then(|t| serde_json::from_str::<Seen>(&t).ok()).unwrap_or_default().seen;
        self.unread = list.iter().filter(|n| n.id > seen).count();
        self.list = list;
        true
    }

    /// Whether to ask HeroNotify about do-not-disturb again: its settings
    /// changed, or a minute turned (schedules).
    pub fn dnd_due(&mut self) -> bool {
        let now = (mtime(&config_path()), crate::notes::unix_now() / 60);
        if self.dnd_asked == Some(now) {
            return false;
        }
        self.dnd_asked = Some(now);
        true
    }

    /// Asks again at the next heartbeat (after changing it).
    pub fn forget_dnd(&mut self) {
        self.dnd_asked = None;
    }
}

/// (on now, on by hand), from `heronotify dnd` and the settings file;
/// None without HeroNotify.
pub fn dnd() -> Option<(bool, bool)> {
    let st = std::process::Command::new("heronotify").arg("dnd").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().ok()?;
    let manual = std::fs::read_to_string(config_path())
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| t.get("do-not-disturb")?.get("on")?.as_bool())
        .unwrap_or(false);
    Some((st.success(), manual))
}

/// Runs `heronotify ARGS`; the error said, if any.
pub fn run(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("heronotify").args(args).stdin(std::process::Stdio::null()).output().map_err(|e| format!("heronotify: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// "now", "5 min", "2 h", "yesterday", "3 days".
pub fn ago(then: u64) -> String {
    let s = unix_now().saturating_sub(then);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{} min", s / 60),
        3600..=86399 => format!("{} h", s / 3600),
        _ if s / 86400 == 1 => "yesterday".into(),
        _ => format!("{} days", s / 86400),
    }
}
