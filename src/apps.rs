//! Installed applications: `.desktop` files, for the taskbar's pinned apps
//! and to find a running window's icon and name from its app id.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct App {
    /// Desktop file id, e.g. "firefox-esr" for firefox-esr.desktop.
    pub id: String,
    pub name: String,
    /// `Icon=`: a theme icon name or a path.
    pub icon: String,
    /// `Exec=` with the field codes (%U, %f...) removed.
    pub exec: String,
}

fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => dirs.push(PathBuf::from(d)),
        None => dirs.extend(std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))),
    }
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let sys = if sys.is_empty() { "/usr/local/share:/usr/share".to_owned() } else { sys };
    dirs.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// Parses the `[Desktop Entry]` group; None for hidden entries.
pub fn parse(id: &str, text: &str) -> Option<(App, Option<String>)> {
    let mut app = App { id: id.to_owned(), ..Default::default() };
    let mut wm_class = None;
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "Name" => app.name = v.to_owned(),
            "Icon" => app.icon = v.to_owned(),
            "Exec" => app.exec = strip_field_codes(v),
            "StartupWMClass" => wm_class = Some(v.to_owned()),
            "Hidden" if v == "true" => return None,
            _ => {}
        }
    }
    if app.name.is_empty() {
        app.name = id.to_owned();
    }
    Some((app, wm_class))
}

fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            // "%%" is a percent sign; %f %u %i %c %k ... are dropped (no
            // files to open).
            if let Some('%') = chars.next() {
                out.push('%');
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The app with desktop file id `id` ("foot", "org.gnome.Nautilus").
pub fn by_id(id: &str) -> Option<App> {
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    for dir in application_dirs() {
        if let Ok(text) = std::fs::read_to_string(dir.join(format!("{id}.desktop"))) {
            return parse(id, &text).map(|(a, _)| a);
        }
    }
    None
}

/// Finds a running window's app from its Wayland app id (or X11 class).
/// Exact desktop id first, then the lowercase one, then a scan matching
/// `StartupWMClass` or the last part of reverse-DNS ids. Cached, misses
/// too, so each app id costs a lookup once.
#[derive(Default)]
pub struct Finder {
    cache: HashMap<String, Option<App>>,
    /// Lazily read: (app, StartupWMClass) of every installed app.
    all: Option<Vec<(App, Option<String>)>>,
}

impl Finder {
    pub fn find(&mut self, app_id: &str) -> Option<App> {
        if app_id.is_empty() {
            return None;
        }
        if let Some(hit) = self.cache.get(app_id) {
            return hit.clone();
        }
        let found = by_id(app_id)
            .or_else(|| by_id(&app_id.to_lowercase()))
            .or_else(|| self.scan(app_id));
        self.cache.insert(app_id.to_owned(), found.clone());
        found
    }

    fn scan(&mut self, app_id: &str) -> Option<App> {
        let all = self.all.get_or_insert_with(|| {
            let mut v = Vec::new();
            for dir in application_dirs() {
                let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let Some(id) = name.strip_suffix(".desktop") else { continue };
                    if let Some(entry) = std::fs::read_to_string(e.path()).ok().and_then(|t| parse(id, &t)) {
                        v.push(entry);
                    }
                }
            }
            v
        });
        let want = app_id.to_lowercase();
        all.iter()
            .find(|(_, class)| class.as_deref().is_some_and(|c| c.to_lowercase() == want))
            .or_else(|| all.iter().find(|(a, _)| a.id.rsplit('.').next().is_some_and(|last| last.to_lowercase() == want)))
            .map(|(a, _)| a.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries() {
        let text = "[Desktop Entry]\nName=Foot\nExec=foot %F\nIcon=foot\nStartupWMClass=Foot\n[Desktop Action new]\nName=Other\n";
        let (app, class) = parse("foot", text).unwrap();
        assert_eq!((app.name.as_str(), app.exec.as_str(), app.icon.as_str()), ("Foot", "foot", "foot"));
        assert_eq!(class.as_deref(), Some("Foot"));
        assert!(parse("x", "[Desktop Entry]\nHidden=true\n").is_none());
        assert_eq!(strip_field_codes("app --x=100%% %U"), "app --x=100%");
    }
}
