//! Live reload: the bar applies changes to its config (from Appearance or
//! a text editor) by re-executing itself. It checks the file's
//! modification time once a second: one stat() call. Theme changes need no
//! restart: HeroUI follows the theme file itself and re-skins the bar.

use std::path::PathBuf;
use std::time::SystemTime;

pub struct Watch {
    files: Vec<(PathBuf, Option<SystemTime>)>,
    pub config: Option<PathBuf>,
}

fn mtime(p: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Watch {
    pub fn new(config: Option<PathBuf>) -> Watch {
        let files = config.iter().cloned().map(|p| {
            let t = mtime(&p);
            (p, t)
        });
        Watch { files: files.collect(), config }
    }

    /// True if a watched file changed (appeared, was edited or removed)
    /// since the last call.
    pub fn changed(&mut self) -> bool {
        let mut changed = false;
        for (path, last) in &mut self.files {
            let now = mtime(path);
            if now != *last {
                *last = now;
                changed = true;
            }
        }
        changed
    }
}

/// Replaces this process with a fresh copy of the bar, same arguments.
/// Only returns if that fails.
pub fn restart() -> std::io::Error {
    use std::os::unix::process::CommandExt;
    // After a package upgrade the running binary shows as "... (deleted)".
    let exe = std::env::current_exe()
        .map(|p| PathBuf::from(p.to_string_lossy().trim_end_matches(" (deleted)")))
        .unwrap_or_else(|_| PathBuf::from("herobar"));
    std::process::Command::new(exe).args(std::env::args_os().skip(1)).exec()
}
