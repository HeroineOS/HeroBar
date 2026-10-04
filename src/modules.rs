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
    Volume,
    Taskbar,
    Workspaces,
    Bluetooth,
    Spacer,
    Group,
    Custom,
}

impl Kind {
    /// The kind of module `name`: its part before any "/" ("cpu/2" is
    /// a cpu). custom and group modules need a name after the "/".
    pub fn from_name(name: &str) -> Option<Kind> {
        let (base, rest) = match name.split_once('/') {
            Some((b, r)) => (b, Some(r)),
            None => (name, None),
        };
        if rest == Some("") {
            return None;
        }
        Some(match base {
            "clock" => Kind::Clock,
            "cpu" => Kind::Cpu,
            "memory" => Kind::Memory,
            "battery" => Kind::Battery,
            "network" => Kind::Network,
            "volume" => Kind::Volume,
            "taskbar" => Kind::Taskbar,
            "workspaces" => Kind::Workspaces,
            "bluetooth" => Kind::Bluetooth,
            "spacer" => Kind::Spacer,
            "group" if rest.is_some() => Kind::Group,
            "custom" if rest.is_some() => Kind::Custom,
            _ => return None,
        })
    }

    fn default_interval(self) -> f64 {
        match self {
            Kind::Clock => 1.0,
            Kind::Cpu => 2.0,
            Kind::Memory | Kind::Network | Kind::Volume => 5.0,
            Kind::Bluetooth => 10.0,
            Kind::Battery => 30.0,
            _ => 10.0,
        }
    }

    fn default_format(self) -> &'static str {
        match self {
            Kind::Clock => "%H:%M",
            Kind::Cpu => "{usage}%",
            Kind::Memory => "{used}/{total} GiB",
            Kind::Battery => "{capacity}%",
            Kind::Network => "{name}",
            Kind::Bluetooth => "{device}",
            Kind::Volume => "{volume}%",
            _ => "",
        }
    }

    /// The icon shown when the config doesn't name one (dynamic kinds
    /// update it in `refresh`).
    fn default_icon(self) -> &'static str {
        match self {
            Kind::Cpu => "cpu",
            Kind::Memory => "memory",
            Kind::Battery => "battery",
            Kind::Network => "network-wired",
            Kind::Volume => "volume-high",
            Kind::Bluetooth => "bluetooth",
            Kind::Group => "apps",
            _ => "",
        }
    }
}

/// Prints "<percent> <muted 0|1>" for the default output, through
/// PipeWire (wpctl) or PulseAudio/pipewire-pulse (pactl); nothing if
/// neither works.
pub const VOLUME_CMD: &str = r#"v=$(wpctl get-volume @DEFAULT_AUDIO_SINK@ 2>/dev/null) && { echo "$v" | awk '{ printf "%d %d\n", $2 * 100 + 0.5, /MUTED/ }'; exit; }
p=$(pactl get-sink-volume @DEFAULT_SINK@ 2>/dev/null | awk '/Volume/ { gsub("%", "", $5); print $5; exit }')
[ -n "$p" ] && echo "$p $(pactl get-sink-mute @DEFAULT_SINK@ 2>/dev/null | grep -c yes)""#;

/// A module's state; `text` is what the bar shows ("" hides the module).
pub struct Module {
    /// Its name in the config ("cpu", "custom/menu").
    pub name: String,
    pub kind: Kind,
    pub text: String,
    /// `text` with its changing numbers at their widest (CPU at 100%,
    /// speeds like "00.0 MB/s"): the module is sized for this, so it
    /// keeps one width while the numbers change.
    pub reserve: String,
    /// Icon shown before the text ("" = none).
    pub icon: String,
    /// Shown when the mouse rests on it.
    pub tooltip: String,
    /// The config's `icon`; None picks the kind's (possibly dynamic) icon.
    icon_cfg: Option<String>,
    pub command: Option<String>,
    pub interval: f64,
    format: String,
    format_disconnected: String,
    fixed_text: Option<String>,
    pub exec: Option<String>,
    battery: Option<String>,
    /// taskbar settings (the windows themselves are in `Bar::desktop`)
    pub taskbar: Option<crate::taskbar::Config>,
    /// The config section (spacer, group and workspaces settings).
    pub cfg: config::Module,
    /// Size overrides (None: the [style] / theme value).
    pub padding: Option<i32>,
    pub icon_size: Option<i32>,
    pub font_size: Option<i32>,
    /// cpu: last (busy, total) jiffies
    last_cpu: (u64, u64),
    /// battery: (percent, status, time to empty or full) at the last refresh
    pub battery_info: Option<(u32, String, Option<String>)>,
    /// Nothing to show (no Bluetooth adapter): takes no space.
    pub absent: bool,
    /// network: the Wi-Fi network's name, from NetworkManager
    pub essid: Option<String>,
    /// network: (interface, received, sent, when) at the last refresh
    last_net: Option<(String, u64, u64, std::time::Instant)>,
}

impl Module {
    pub fn new(name: &str, cfg: Option<&config::Module>) -> Module {
        let kind = Kind::from_name(name).expect("validated by config::parse");
        let empty = config::Module::default();
        let cfg = cfg.unwrap_or(&empty);
        let icon_cfg = cfg.icon.clone();
        let mut m = Module {
            name: name.to_owned(),
            kind,
            text: String::new(),
            reserve: String::new(),
            battery_info: None,
            tooltip: String::new(),
            icon: icon_cfg.clone().unwrap_or_else(|| kind.default_icon().to_owned()),
            icon_cfg,
            command: cfg.on_click.as_ref().and_then(|a| a.command()).map(str::to_owned),
            interval: cfg.interval.unwrap_or(kind.default_interval()).max(0.1),
            format: cfg.format.clone().unwrap_or_else(|| kind.default_format().to_owned()),
            format_disconnected: cfg.format_disconnected.clone().unwrap_or_else(|| "offline".into()),
            fixed_text: cfg.text.clone(),
            exec: match kind {
                // With pactl, volume follows the sound server's events
                // (popups.rs); without it, a command polls.
                Kind::Volume if cfg.exec.is_none() && crate::system::have("pactl") => None,
                Kind::Volume => Some(cfg.exec.clone().unwrap_or_else(|| VOLUME_CMD.to_owned())),
                _ => cfg.exec.clone(),
            },
            battery: cfg.name.clone(),
            taskbar: (kind == Kind::Taskbar).then(|| crate::taskbar::Config::new(Some(cfg))),
            cfg: cfg.clone(),
            padding: cfg.padding,
            icon_size: cfg.icon_size,
            font_size: cfg.font_size,
            last_cpu: (0, 0),
            absent: kind == Kind::Bluetooth,
            essid: None,
            last_net: None,
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
        match self.kind {
            Kind::Custom | Kind::Volume => self.exec.is_some(),
            // Updated by compositor events, or not at all.
            Kind::Taskbar | Kind::Workspaces | Kind::Spacer | Kind::Group => false,
            _ => true,
        }
    }

    /// Takes the output of `exec` (run on a background thread).
    pub fn set_output(&mut self, out: String) {
        if self.kind == Kind::Custom {
            // {"text": "...", "icon": "..."} sets the icon too.
            if out.starts_with('{') {
                if let Ok(j) = serde_json::from_str::<CustomOutput>(&out) {
                    self.text = j.text.unwrap_or_default();
                    if let Some(i) = j.icon {
                        self.icon = i;
                    }
                    return;
                }
            }
            self.text = out;
            return;
        }
        if self.kind != Kind::Volume {
            self.text = out;
            return;
        }
        let mut f = out.split_whitespace();
        let (Some(Ok(vol)), muted) = (f.next().map(str::parse::<u32>), f.next() == Some("1")) else {
            // No audio: hide the module.
            self.text.clear();
            return;
        };
        self.text = fill(&self.format, &[("volume", slot(vol.to_string()))]);
        self.reserve = fill(&self.format, &[("volume", slot("100"))]);
        self.tooltip = if muted { format!("Volume: {vol}% (muted)") } else { format!("Volume: {vol}%") };
        self.set_icon(if muted || vol == 0 {
            "volume-muted"
        } else if vol < 50 {
            "volume-low"
        } else {
            "volume-high"
        });
    }

    /// Shows the Bluetooth state: hidden without an adapter, off, on, or
    /// the connected device.
    pub fn set_bt(&mut self, bt: Option<&crate::system::Bt>) {
        let Some(bt) = bt else {
            self.absent = true;
            return;
        };
        self.absent = false;
        let connected: Vec<&str> = bt.connected().map(|d| d.name.as_str()).collect();
        self.set_icon(if !bt.powered {
            "bluetooth-off"
        } else if connected.is_empty() {
            "bluetooth"
        } else {
            "bluetooth-connected"
        });
        self.tooltip = if !bt.powered {
            "Bluetooth: off".into()
        } else if connected.is_empty() {
            "Bluetooth: on, nothing connected".into()
        } else {
            format!("Bluetooth: connected to {}", connected.join(", "))
        };
        self.text = if connected.is_empty() {
            String::new()
        } else {
            fill(&self.format, &[("device", connected[0].to_owned()), ("count", connected.len().to_string())])
        };
        self.reserve = self.text.clone();
    }

    /// Sets the dynamic icon, unless the config chose one.
    fn set_icon(&mut self, name: &str) {
        if self.icon_cfg.is_none() && self.icon != name {
            self.icon = name.to_owned();
        }
    }

    /// Re-reads the module's source. Custom `exec` modules are refreshed
    /// by the caller on a background thread instead.
    pub fn refresh(&mut self) {
        let mut reserve = None;
        let text = match self.kind {
            Kind::Clock => {
                self.tooltip = strftime("%A %-d %B %Y");
                strftime(&self.format)
            }
            Kind::Cpu => {
                let (busy, total) = cpu_jiffies();
                let (db, dt) = (busy.saturating_sub(self.last_cpu.0), total.saturating_sub(self.last_cpu.1));
                self.last_cpu = (busy, total);
                let usage = if dt > 0 { 100 * db / dt } else { 0 };
                self.tooltip = format!("CPU: {usage}% busy");
                reserve = Some(fill(&self.format, &[("usage", slot("100"))]));
                fill(&self.format, &[("usage", slot(usage.to_string()))])
            }
            Kind::Memory => {
                let (avail, total) = meminfo();
                if total == 0 {
                    String::new()
                } else {
                    let gib = |kib: u64| kib as f64 / 1024.0 / 1024.0;
                    let used = total.saturating_sub(avail);
                    self.tooltip = format!("Memory: {:.1} GiB used of {:.1} GiB ({}%)", gib(used), gib(total), 100 * used / total);
                    let t = format!("{:.1}", gib(total));
                    reserve = Some(fill(&self.format, &[("used", slot(t.clone())), ("total", t), ("percent", slot("100"))]));
                    fill(
                        &self.format,
                        &[
                            ("used", slot(format!("{:.1}", gib(used)))),
                            ("total", format!("{:.1}", gib(total))),
                            ("percent", slot((100 * used / total).to_string())),
                        ],
                    )
                }
            }
            Kind::Battery => match self.battery.clone() {
                Some(b) => {
                    let read = |f: &str| {
                        std::fs::read_to_string(format!("{}/{b}/{f}", power_dir()))
                            .map(|s| s.trim().to_owned())
                            .unwrap_or_default()
                    };
                    let capacity = read("capacity");
                    if capacity.is_empty() {
                        String::new()
                    } else {
                        let status = read("status");
                        let level: u32 = capacity.parse().unwrap_or(100);
                        let charging = status == "Charging";
                        self.set_icon(&format!("battery-{}{}", (level + 5) / 10 * 10, if charging { "-charging" } else { "" }));
                        let num = |f: &str| read(f).parse::<f64>().ok();
                        let time = battery_time(
                            &status,
                            num("energy_now").or(num("charge_now")),
                            num("energy_full").or(num("charge_full")),
                            num("power_now").or(num("current_now")),
                        );
                        self.tooltip = match (&time, status.as_str()) {
                            (Some(t), "Charging") => format!("Battery: {level}%\nCharging, full in {t}"),
                            (Some(t), _) => format!("Battery: {level}%\n{t} left"),
                            (None, "Full") | (None, "Not charging") => format!("Battery: {level}%\nFully charged"),
                            (None, s) => format!("Battery: {level}%\n{s}"),
                        };
                        self.battery_info = Some((level, status.clone(), time.clone()));
                        let widest_time = if time.is_some() { "00 h 00 min" } else { "" };
                        reserve = Some(fill(&self.format, &[("capacity", slot("100")), ("status", status.clone()), ("time", slot(widest_time))]));
                        fill(&self.format, &[("capacity", slot(capacity)), ("status", status), ("time", slot(time.unwrap_or_default()))])
                    }
                }
                None => String::new(),
            },
            Kind::Network => match default_route_interface() {
                Some(ifname) => {
                    let wireless = std::path::Path::new(&format!("/sys/class/net/{ifname}/wireless")).exists();
                    let signal = if wireless { crate::system::wifi_signal(&ifname) } else { None };
                    match signal {
                        Some(s) => self.set_icon(&format!("network-wireless-{s}")),
                        None if wireless => self.set_icon("network-wireless"),
                        None => self.set_icon("network-wired"),
                    }
                    let state = std::fs::read_to_string(format!("/sys/class/net/{ifname}/operstate"))
                        .map(|s| s.trim().to_owned())
                        .unwrap_or_default();
                    // Speeds since the last refresh.
                    let now = std::time::Instant::now();
                    let (rx, tx) = crate::system::traffic(&ifname).unwrap_or((0, 0));
                    let (down, up) = match &self.last_net {
                        Some((i, r0, t0, at)) if *i == ifname => {
                            let dt = now.duration_since(*at).as_secs_f64().max(0.001);
                            (rx.saturating_sub(*r0) as f64 / dt, tx.saturating_sub(*t0) as f64 / dt)
                        }
                        _ => (0.0, 0.0),
                    };
                    self.last_net = Some((ifname.clone(), rx, tx, now));
                    let essid = if wireless { self.essid.clone().unwrap_or_default() } else { String::new() };
                    let name = if essid.is_empty() { ifname.clone() } else { essid.clone() };
                    let b = crate::system::bytes;
                    self.tooltip = format!(
                        "{}{}\nDown {}/s  ·  Up {}/s\nReceived {}  ·  Sent {}",
                        if wireless { format!("Wi-Fi: {name}") } else { format!("Wired: {ifname}") },
                        signal.map(|s| format!("\nSignal: {s}%")).unwrap_or_default(),
                        b(down),
                        b(up),
                        b(rx as f64),
                        b(tx as f64),
                    );
                    let short = self.cfg.units.as_deref() == Some("short");
                    let speed = |v: f64| if short { crate::system::bytes_short(v) } else { format!("{}/s", b(v)) };
                    let total = |v: f64| if short { crate::system::bytes_short(v) } else { b(v) };
                    // Speeds and totals at their widest (3 digits, see
                    // `system::bytes`).
                    reserve = Some(fill(
                        &self.format,
                        &[
                            ("name", name.clone()),
                            ("essid", essid.clone()),
                            ("ifname", ifname.clone()),
                            ("state", state.clone()),
                            ("signal", slot("100")),
                            ("down", slot(if short { "000M" } else { "000 MB/s" })),
                            ("up", slot(if short { "000M" } else { "000 MB/s" })),
                            ("down-total", slot(if short { "000G" } else { "000 GB" })),
                            ("up-total", slot(if short { "000G" } else { "000 GB" })),
                        ],
                    ));
                    fill(
                        &self.format,
                        &[
                            ("name", name),
                            ("essid", essid),
                            ("ifname", ifname),
                            ("state", state),
                            ("signal", slot(signal.map(|s| s.to_string()).unwrap_or_default())),
                            ("down", slot(speed(down))),
                            ("up", slot(speed(up))),
                            ("down-total", slot(total(rx as f64))),
                            ("up-total", slot(total(tx as f64))),
                        ],
                    )
                }
                None => {
                    self.set_icon("network-offline");
                    self.tooltip = "Not connected".into();
                    self.format_disconnected.clone()
                }
            },
            _ => return,
        };
        self.reserve = reserve.unwrap_or_else(|| text.clone());
        self.text = text;
    }
}

#[derive(serde::Deserialize)]
struct CustomOutput {
    text: Option<String>,
    icon: Option<String>,
}

/// "2 h 13 min" until empty (discharging) or full (charging), from the
/// battery's energy (or charge) and power (or current); None when it
/// can't be told (idle, full, missing values).
pub fn battery_time(status: &str, now: Option<f64>, full: Option<f64>, rate: Option<f64>) -> Option<String> {
    let (now, rate) = (now?, rate?);
    if rate <= 0.0 {
        return None;
    }
    let hours = match status {
        "Discharging" => now / rate,
        "Charging" => (full? - now).max(0.0) / rate,
        _ => return None,
    };
    let mins = (hours * 60.0).round() as u64;
    if mins > 48 * 60 {
        return None;
    }
    Some(if mins >= 60 { format!("{} h {} min", mins / 60, mins % 60) } else { format!("{mins} min") })
}

/// Marks a changing number in module text: the bar gives it a fixed slot
/// (as wide as the same slot in `Module::reserve`), right-aligned, so the
/// text around it stays put. See `main.rs` `segments`.
pub const SLOT_START: char = '\u{1}';
pub const SLOT_END: char = '\u{2}';

fn slot(v: impl Into<String>) -> String {
    let v = v.into();
    if v.is_empty() {
        v
    } else {
        format!("{SLOT_START}{v}{SLOT_END}")
    }
}

/// `text` without slot marks.
#[cfg(test)]
fn plain(text: &str) -> String {
    text.chars().filter(|&c| c != SLOT_START && c != SLOT_END).collect()
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

/// Where batteries are (HEROBAR_POWER_SUPPLY replaces it, for testing).
fn power_dir() -> String {
    std::env::var("HEROBAR_POWER_SUPPLY").unwrap_or_else(|_| "/sys/class/power_supply".into())
}

fn first_battery() -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(power_dir())
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            std::fs::read_to_string(format!("{}/{n}/type", power_dir())).is_ok_and(|t| t.trim() == "Battery")
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

/// Today: (year, month 1-12, day).
pub fn today() -> (i32, u32, u32) {
    let d = |f: &str| strftime(f).parse::<i32>().unwrap_or(1);
    (d("%Y"), d("%m") as u32, d("%d") as u32)
}

/// Formats a date of the calendar (`format` as strftime), in the user's
/// language.
pub fn format_date(year: i32, month: u32, day: u32, format: &str) -> String {
    let Ok(fmt) = CString::new(format) else { return String::new() };
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        tm.tm_year = year - 1900;
        tm.tm_mon = month as c_int - 1;
        tm.tm_mday = day as c_int;
        tm.tm_wday = weekday(year, month, day) as c_int;
        let mut buf = [0u8; 128];
        let n = c_strftime(buf.as_mut_ptr() as *mut c_char, buf.len(), fmt.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// Day of the week, 0 = Sunday (Sakamoto's method).
pub fn weekday(year: i32, month: u32, day: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year - 1 } else { year };
    ((y + y / 4 - y / 100 + y / 400 + T[month as usize - 1] + day as i32).rem_euclid(7)) as u32
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

/// The month `offset` months from (year, month).
pub fn add_months(year: i32, month: u32, offset: i32) -> (i32, u32) {
    let m = year * 12 + month as i32 - 1 + offset;
    (m.div_euclid(12), m.rem_euclid(12) as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_math() {
        assert_eq!(weekday(2026, 10, 3), 6, "a Saturday");
        assert_eq!(weekday(2024, 1, 1), 1, "a Monday");
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 12, 1), (2027, 1));
    }

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
        assert_eq!(Kind::from_name("cpu/2"), Some(Kind::Cpu));
        assert_eq!(Kind::from_name("spacer"), Some(Kind::Spacer));
        assert_eq!(Kind::from_name("group"), None);
        assert_eq!(Kind::from_name("group/sys"), Some(Kind::Group));
    }

    #[test]
    fn battery_estimates() {
        assert_eq!(battery_time("Discharging", Some(30.0), Some(50.0), Some(13.5)).as_deref(), Some("2 h 13 min"));
        assert_eq!(battery_time("Charging", Some(40.0), Some(50.0), Some(20.0)).as_deref(), Some("30 min"));
        assert_eq!(battery_time("Full", Some(50.0), Some(50.0), Some(0.0)), None);
        assert_eq!(battery_time("Discharging", Some(30.0), None, None), None);
    }

    #[test]
    fn custom_json_output() {
        let cfg = config::Module { exec: Some("x".into()), ..Default::default() };
        let mut m = Module::new("custom/w", Some(&cfg));
        m.set_output(r#"{"text": "21°", "icon": "weather-clear"}"#.into());
        assert_eq!((m.text.as_str(), m.icon.as_str()), ("21°", "weather-clear"));
        m.set_output("plain".into());
        assert_eq!(m.text, "plain");
    }

    #[test]
    fn volume_output() {
        let mut m = Module::new("volume", None);
        m.set_output("40 0".into());
        assert_eq!((plain(&m.text).as_str(), m.icon.as_str()), ("40%", "volume-low"));
        m.set_output("75 1".into());
        assert_eq!((plain(&m.text).as_str(), m.icon.as_str()), ("75%", "volume-muted"));
        m.set_output(String::new());
        assert_eq!(m.text, "");
        let custom = config::Module { icon: Some(String::new()), ..Default::default() };
        let mut m = Module::new("volume", Some(&custom));
        m.set_output("90 0".into());
        assert_eq!(m.icon, "", "a configured icon (here: none) stays");
    }

    #[test]
    fn system_readers_work_here() {
        let mut m = Module::new("memory", None);
        m.refresh();
        assert!(m.text.ends_with(" GiB") && m.text.contains('/'), "{}", m.text);
        assert_eq!(m.icon, "memory");
        assert_eq!(run_exec("echo hi; echo there"), "hi");
    }
}
