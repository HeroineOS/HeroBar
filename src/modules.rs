//! Built-in modules. System readers use procfs/sysfs files, which are in
//! memory and take microseconds, so they run on the UI thread; `custom`
//! commands run on a background thread (Task::perform).

use std::ffi::{c_char, c_int, c_long, CString};

use crate::config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Clock,
    Cpu,
    Memory,
    Battery,
    Network,
    Custom,
}

impl Kind {
    pub fn from_name(name: &str) -> Option<Kind> {
        Some(match name {
            "clock" => Kind::Clock,
            "cpu" => Kind::Cpu,
            "memory" => Kind::Memory,
            "battery" => Kind::Battery,
            "network" => Kind::Network,
            n if n.starts_with("custom/") && n.len() > 7 => Kind::Custom,
            _ => return None,
        })
    }

    fn default_interval(self) -> f64 {
        match self {
            Kind::Clock => 1.0,
            Kind::Cpu => 2.0,
            Kind::Memory | Kind::Network => 5.0,
            Kind::Battery => 30.0,
            Kind::Custom => 10.0,
        }
    }

    fn default_format(self) -> &'static str {
        match self {
            Kind::Clock => "%H:%M",
            Kind::Cpu => "CPU {usage}%",
            Kind::Memory => "RAM {used} GiB",
            Kind::Battery => "BAT {capacity}%",
            Kind::Network => "{ifname}",
            Kind::Custom => "",
        }
    }
}

/// A module's state; `text` is what the bar shows ("" hides the module).
pub struct Module {
    pub kind: Kind,
    pub text: String,
    pub command: Option<String>,
    pub interval: f64,
    format: String,
    format_disconnected: String,
    fixed_text: Option<String>,
    pub exec: Option<String>,
    battery: Option<String>,
    /// cpu: last (busy, total) jiffies
    last_cpu: (u64, u64),
}

impl Module {
    pub fn new(name: &str, cfg: Option<&config::Module>) -> Module {
        let kind = Kind::from_name(name).expect("validated by config::parse");
        let empty = config::Module::default();
        let cfg = cfg.unwrap_or(&empty);
        let mut m = Module {
            kind,
            text: String::new(),
            command: cfg.on_click.as_ref().and_then(|a| a.command()).map(str::to_owned),
            interval: cfg.interval.unwrap_or(kind.default_interval()).max(0.1),
            format: cfg.format.clone().unwrap_or_else(|| kind.default_format().to_owned()),
            format_disconnected: cfg.format_disconnected.clone().unwrap_or_else(|| "offline".into()),
            fixed_text: cfg.text.clone(),
            exec: cfg.exec.clone(),
            battery: cfg.name.clone(),
            last_cpu: (0, 0),
        };
        if kind == Kind::Battery && m.battery.is_none() {
            m.battery = first_battery();
        }
        if kind == Kind::Cpu {
            m.last_cpu = cpu_jiffies();
        }
        if let Some(t) = &m.fixed_text {
            m.text = t.clone();
        }
        m.refresh();
        m
    }

    /// True if the module changes over time (needs a timer).
    pub fn is_dynamic(&self) -> bool {
        self.kind != Kind::Custom || self.exec.is_some()
    }

    /// Re-reads the module's source. Custom `exec` modules are refreshed
    /// by the caller on a background thread instead.
    pub fn refresh(&mut self) {
        let text = match self.kind {
            Kind::Clock => strftime(&self.format),
            Kind::Cpu => {
                let (busy, total) = cpu_jiffies();
                let (db, dt) = (busy.saturating_sub(self.last_cpu.0), total.saturating_sub(self.last_cpu.1));
                self.last_cpu = (busy, total);
                let usage = if dt > 0 { 100 * db / dt } else { 0 };
                fill(&self.format, &[("usage", usage.to_string())])
            }
            Kind::Memory => {
                let (avail, total) = meminfo();
                if total == 0 {
                    String::new()
                } else {
                    let gib = |kib: u64| kib as f64 / 1024.0 / 1024.0;
                    let used = total.saturating_sub(avail);
                    fill(
                        &self.format,
                        &[
                            ("used", format!("{:.1}", gib(used))),
                            ("total", format!("{:.1}", gib(total))),
                            ("percent", (100 * used / total).to_string()),
                        ],
                    )
                }
            }
            Kind::Battery => match &self.battery {
                Some(b) => {
                    let read = |f: &str| {
                        std::fs::read_to_string(format!("/sys/class/power_supply/{b}/{f}"))
                            .map(|s| s.trim().to_owned())
                            .unwrap_or_default()
                    };
                    let capacity = read("capacity");
                    if capacity.is_empty() {
                        String::new()
                    } else {
                        fill(&self.format, &[("capacity", capacity), ("status", read("status"))])
                    }
                }
                None => String::new(),
            },
            Kind::Network => match default_route_interface() {
                Some(ifname) => {
                    let state = std::fs::read_to_string(format!("/sys/class/net/{ifname}/operstate"))
                        .map(|s| s.trim().to_owned())
                        .unwrap_or_default();
                    fill(&self.format, &[("ifname", ifname), ("state", state)])
                }
                None => self.format_disconnected.clone(),
            },
            Kind::Custom => return,
        };
        self.text = text;
    }
}

/// Replaces `{key}` placeholders.
fn fill(format: &str, values: &[(&str, String)]) -> String {
    let mut out = format.to_owned();
    for (k, v) in values {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Runs `command` with `sh -c` and returns its first output line.
pub fn run_exec(command: &str) -> String {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().next().map(|l| l.trim().to_owned()))
        .unwrap_or_default()
}

/// Starts `command` with `sh -c` and returns at once; the shell
/// backgrounds it, so it's adopted by init and never becomes our zombie.
pub fn launch(command: &str) {
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{command} &"))
        .stdin(std::process::Stdio::null())
        .status();
}

fn cpu_jiffies() -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    let nums: Vec<u64> = stat.lines().next().unwrap_or("").split_whitespace().skip(1).filter_map(|n| n.parse().ok()).collect();
    let total: u64 = nums.iter().take(8).sum();
    let idle = nums.get(3).copied().unwrap_or(0) + nums.get(4).copied().unwrap_or(0);
    (total.saturating_sub(idle), total)
}

/// (MemAvailable, MemTotal) in KiB.
fn meminfo() -> (u64, u64) {
    let info = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |name: &str| {
        info.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    (field("MemAvailable:"), field("MemTotal:"))
}

fn first_battery() -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/power_supply")
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            std::fs::read_to_string(format!("/sys/class/power_supply/{n}/type")).is_ok_and(|t| t.trim() == "Battery")
        })
        .collect();
    names.sort();
    names.into_iter().next()
}

/// The interface of the default IPv4 route, from /proc/net/route.
fn default_route_interface() -> Option<String> {
    let routes = std::fs::read_to_string("/proc/net/route").ok()?;
    routes.lines().skip(1).find_map(|l| {
        let mut f = l.split_whitespace();
        let (iface, dest) = (f.next()?, f.next()?);
        (dest == "00000000").then(|| iface.to_owned())
    })
}

// libc time formatting: honors TZ and the system zone, no extra crates.
#[repr(C)]
struct Tm {
    tm_sec: c_int,
    tm_min: c_int,
    tm_hour: c_int,
    tm_mday: c_int,
    tm_mon: c_int,
    tm_year: c_int,
    tm_wday: c_int,
    tm_yday: c_int,
    tm_isdst: c_int,
    tm_gmtoff: c_long,
    tm_zone: *const c_char,
}

extern "C" {
    fn time(t: *mut c_long) -> c_long;
    fn tzset();
    fn localtime_r(t: *const c_long, tm: *mut Tm) -> *mut Tm;
    #[link_name = "strftime"]
    fn c_strftime(s: *mut c_char, max: usize, format: *const c_char, tm: *const Tm) -> usize;
}

/// Call once at startup, before any thread could change TZ.
pub fn init_time() {
    unsafe { tzset() }
}

pub fn strftime(format: &str) -> String {
    let Ok(fmt) = CString::new(format) else { return String::new() };
    unsafe {
        let now = time(std::ptr::null_mut());
        let mut tm: Tm = std::mem::zeroed();
        if localtime_r(&now, &mut tm).is_null() {
            return String::new();
        }
        let mut buf = [0u8; 256];
        let n = c_strftime(buf.as_mut_ptr() as *mut c_char, buf.len(), fmt.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        assert_eq!(fill("CPU {usage}%", &[("usage", "7".into())]), "CPU 7%");
        assert_eq!(fill("{a}-{a}-{b}", &[("a", "x".into())]), "x-x-{b}");
    }

    #[test]
    fn clock_formats() {
        init_time();
        let year = strftime("%Y");
        assert_eq!(year.len(), 4, "{year}");
        assert_eq!(strftime("100%%"), "100%");
    }

    #[test]
    fn kinds() {
        assert_eq!(Kind::from_name("custom/x"), Some(Kind::Custom));
        assert_eq!(Kind::from_name("custom/"), None);
        assert_eq!(Kind::from_name("weather"), None);
    }

    #[test]
    fn system_readers_work_here() {
        let mut m = Module::new("memory", None);
        m.refresh();
        assert!(m.text.starts_with("RAM "), "{}", m.text);
        assert_eq!(run_exec("echo hi; echo there"), "hi");
    }
}
