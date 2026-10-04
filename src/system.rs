//! Audio, network and Bluetooth state and actions, for the volume, network
//! and bluetooth modules and their popups. Each talks to the standard
//! command-line tool of its service, in its scripting format:
//!
//! - audio: `pactl` (PulseAudio, or PipeWire through pipewire-pulse), JSON
//! - network: `nmcli` (NetworkManager), terse mode
//! - Bluetooth: `bluetoothctl` (BlueZ)
//!
//! Every call here blocks (it runs a program), so they run on background
//! threads (`Task::perform`); results come back as messages.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `prog args` and returns its stdout (None if it couldn't run or
/// failed).
fn run(prog: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(prog).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like `run`, but returns the error output on failure, for showing.
fn act(prog: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_owned())
    }
}

pub fn have(prog: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}

// --- Audio ----------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioDev {
    pub name: String,
    pub desc: String,
    /// Percent (of the first channel).
    pub volume: u32,
    pub muted: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Audio {
    pub sinks: Vec<AudioDev>,
    pub sources: Vec<AudioDev>,
    pub default_sink: String,
    pub default_source: String,
}

impl Audio {
    pub fn sink(&self) -> Option<&AudioDev> {
        self.sinks.iter().find(|d| d.name == self.default_sink).or(self.sinks.first())
    }
    pub fn source(&self) -> Option<&AudioDev> {
        self.sources.iter().find(|d| d.name == self.default_source).or(self.sources.first())
    }
}

#[derive(serde::Deserialize)]
struct PaDev {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: std::collections::BTreeMap<String, PaVol>,
}

#[derive(serde::Deserialize)]
struct PaVol {
    value_percent: String,
}

fn parse_devs(json: &str) -> Vec<AudioDev> {
    let devs: Vec<PaDev> = serde_json::from_str(json).unwrap_or_default();
    devs.into_iter()
        // Monitors of outputs aren't microphones.
        .filter(|d| !d.name.ends_with(".monitor"))
        .map(|d| AudioDev {
            volume: d
                .volume
                .values()
                .next()
                .and_then(|v| v.value_percent.trim_end_matches('%').trim().parse().ok())
                .unwrap_or(0),
            desc: if d.description.is_empty() { d.name.clone() } else { d.description },
            name: d.name,
            muted: d.mute,
        })
        .collect()
}

/// The current audio state; None without pactl or a sound server.
pub fn audio() -> Option<Audio> {
    let sinks = run("pactl", &["-f", "json", "list", "sinks"])?;
    let sources = run("pactl", &["-f", "json", "list", "sources"]).unwrap_or_default();
    Some(Audio {
        sinks: parse_devs(&sinks),
        sources: parse_devs(&sources),
        default_sink: run("pactl", &["get-default-sink"]).unwrap_or_default().trim().to_owned(),
        default_source: run("pactl", &["get-default-source"]).unwrap_or_default().trim().to_owned(),
    })
}

#[derive(Debug, Clone)]
pub enum AudioCmd {
    Volume { input: bool, name: String, percent: u32 },
    Mute { input: bool, name: String, muted: bool },
    Default { input: bool, name: String },
}

pub fn audio_do(cmd: &AudioCmd) -> Result<(), String> {
    let kind = |input: bool| if input { "source" } else { "sink" };
    match cmd {
        AudioCmd::Volume { input, name, percent } => {
            act("pactl", &[&format!("set-{}-volume", kind(*input)), name, &format!("{percent}%")])
        }
        AudioCmd::Mute { input, name, muted } => {
            act("pactl", &[&format!("set-{}-mute", kind(*input)), name, if *muted { "1" } else { "0" }])
        }
        AudioCmd::Default { input, name } => act("pactl", &[&format!("set-default-{}", kind(*input)), name]),
    }
}

/// Blocks, calling `changed` whenever the sound server reports a change
/// to devices or the defaults (`pactl subscribe`). Returns when pactl
/// exits or `changed` returns false.
pub fn audio_watch(changed: impl Fn() -> bool) {
    use std::io::BufRead;
    let Ok(mut child) = Command::new("pactl").arg("subscribe").stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return;
    };
    let Some(out) = child.stdout.take() else { return };
    for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
        // "Event 'change' on sink #56", "... on server"...
        if (line.contains(" sink ") || line.contains(" source ") || line.contains(" server")) && !changed() {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

// --- Network --------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Wifi {
    pub ssid: String,
    pub signal: u32,
    pub secure: bool,
    pub active: bool,
    /// A saved connection with this name exists (no password needed).
    pub known: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conn {
    pub name: String,
    pub uuid: String,
    /// "ethernet", "vpn", "wifi"...
    pub kind: String,
    pub active: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Net {
    pub wifi_enabled: bool,
    pub has_wifi: bool,
    pub networks: Vec<Wifi>,
    /// Saved connections that aren't Wi-Fi (wired, VPN...): any number
    /// can be up at once.
    pub others: Vec<Conn>,
}

/// Splits a line of `nmcli -t` output into fields (":" separated, with
/// "\:" and "\\" escaped).
pub fn nm_fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.last_mut().expect("non-empty").push(n);
                }
            }
            ':' => out.push(String::new()),
            c => out.last_mut().expect("non-empty").push(c),
        }
    }
    out
}

fn short_kind(t: &str) -> &str {
    match t {
        "802-3-ethernet" => "ethernet",
        "802-11-wireless" => "wifi",
        t => t,
    }
}

pub fn parse_net(radio: &str, wifi_list: &str, conns: &str) -> Net {
    let saved: Vec<Conn> = conns
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f = nm_fields(l);
            let get = |i: usize| f.get(i).cloned().unwrap_or_default();
            Conn { name: get(0), uuid: get(1), kind: short_kind(&get(2)).to_owned(), active: !get(3).is_empty() }
        })
        .collect();
    let mut networks: Vec<Wifi> = Vec::new();
    for l in wifi_list.lines().filter(|l| !l.is_empty()) {
        let f = nm_fields(l);
        let get = |i: usize| f.get(i).cloned().unwrap_or_default();
        let ssid = get(1);
        // Hidden networks have no name; the same network seen through
        // several access points shows once, with the best signal.
        if ssid.is_empty() {
            continue;
        }
        let w = Wifi {
            active: get(0) == "*",
            signal: get(2).parse().unwrap_or(0),
            secure: !get(3).is_empty() && get(3) != "--",
            known: saved.iter().any(|c| c.kind == "wifi" && c.name == ssid),
            ssid,
        };
        match networks.iter_mut().find(|n| n.ssid == w.ssid) {
            Some(n) => {
                n.active |= w.active;
                n.signal = n.signal.max(w.signal);
            }
            None => networks.push(w),
        }
    }
    // Connected first, then by signal.
    networks.sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    Net {
        wifi_enabled: radio.trim() == "enabled",
        has_wifi: !radio.trim().is_empty(),
        networks,
        others: saved.into_iter().filter(|c| c.kind != "wifi" && c.kind != "loopback").collect(),
    }
}

/// The network state; None without NetworkManager. `rescan` looks for
/// networks first (takes a few seconds).
pub fn net(rescan: bool) -> Option<Net> {
    let conns = run("nmcli", &["-t", "-f", "NAME,UUID,TYPE,DEVICE", "connection", "show"])?;
    let radio = run("nmcli", &["-t", "-f", "WIFI", "radio"]).unwrap_or_default();
    let list = run(
        "nmcli",
        &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", if rescan { "yes" } else { "no" }],
    )
    .unwrap_or_default();
    Some(parse_net(&radio, &list, &conns))
}

#[derive(Debug, Clone)]
pub enum NetCmd {
    WifiEnabled(bool),
    /// Connect to a Wi-Fi network (saved, open, or with this password).
    Connect { ssid: String, password: Option<String>, known: bool },
    Up(String),
    Down(String),
    /// Disconnect a connection by name (a Wi-Fi network's is its name).
    DownId(String),
}

pub fn net_do(cmd: &NetCmd) -> Result<(), String> {
    match cmd {
        NetCmd::WifiEnabled(on) => act("nmcli", &["radio", "wifi", if *on { "on" } else { "off" }]),
        NetCmd::Connect { ssid, known: true, password: None } => act("nmcli", &["connection", "up", "id", ssid]),
        NetCmd::Connect { ssid, password: None, .. } => act("nmcli", &["device", "wifi", "connect", ssid]),
        NetCmd::Connect { ssid, password: Some(pw), .. } => {
            // The password goes in through stdin (--ask), never on the
            // command line where other users could see it.
            let mut child = Command::new("nmcli")
                .args(["--ask", "device", "wifi", "connect", ssid])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("nmcli: {e}"))?;
            if let Some(mut i) = child.stdin.take() {
                let _ = writeln!(i, "{pw}");
            }
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                let err = String::from_utf8_lossy(&out.stderr);
                Err(err.lines().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_owned())
            }
        }
        NetCmd::Up(uuid) => act("nmcli", &["connection", "up", "uuid", uuid]),
        NetCmd::Down(uuid) => act("nmcli", &["connection", "down", "uuid", uuid]),
        NetCmd::DownId(id) => act("nmcli", &["connection", "down", "id", id]),
    }
}

/// The active Wi-Fi network's name, if any (cheap: no scan).
pub fn active_ssid() -> Option<String> {
    let out = run("nmcli", &["-t", "-f", "NAME,TYPE", "connection", "show", "--active"])?;
    out.lines().map(nm_fields).find(|f| f.get(1).map(String::as_str) == Some("802-11-wireless")).map(|f| f[0].clone())
}

/// Blocks, calling `changed` on every NetworkManager change (`nmcli
/// monitor`).
pub fn net_watch(changed: impl Fn() -> bool) {
    use std::io::BufRead;
    let Ok(mut child) = Command::new("nmcli").arg("monitor").stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return;
    };
    let Some(out) = child.stdout.take() else { return };
    for _ in std::io::BufReader::new(out).lines().map_while(Result::ok) {
        if !changed() {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Wi-Fi signal of `ifname` in percent, from /proc/net/wireless.
pub fn wifi_signal(ifname: &str) -> Option<u32> {
    let text = std::fs::read_to_string("/proc/net/wireless").ok()?;
    parse_wireless(&text, ifname)
}

pub fn parse_wireless(text: &str, ifname: &str) -> Option<u32> {
    text.lines().skip(2).find_map(|l| {
        let (name, rest) = l.split_once(':')?;
        if name.trim() != ifname {
            return None;
        }
        // "status link level noise...": link quality out of 70.
        let link: f64 = rest.split_whitespace().nth(1)?.trim_end_matches('.').parse().ok()?;
        Some(((link / 70.0) * 100.0).round().clamp(0.0, 100.0) as u32)
    })
}

/// (received, sent) bytes of `ifname` since it came up.
pub fn traffic(ifname: &str) -> Option<(u64, u64)> {
    let read = |f: &str| -> Option<u64> {
        std::fs::read_to_string(format!("/sys/class/net/{ifname}/statistics/{f}")).ok()?.trim().parse().ok()
    };
    Some((read("rx_bytes")?, read("tx_bytes")?))
}

/// "512 B", "1.4 KB", "23 MB", "1.2 GB" (powers of 1000, like most UIs).
pub fn bytes(n: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 || v >= 100.0 {
        format!("{v:.0} {}", units[u])
    } else {
        format!("{v:.1} {}", units[u])
    }
}

// --- Brightness -----------------------------------------------------------

/// The screen's brightness in percent, from brightnessctl (None: no
/// brightnessctl or no backlight).
pub fn brightness() -> Option<u32> {
    parse_brightness(&run("brightnessctl", &["-m", "-c", "backlight", "info"])?)
}

/// `brightnessctl -m info`: "intel_backlight,backlight,400,42%,937".
fn parse_brightness(out: &str) -> Option<u32> {
    out.lines().next()?.split(',').nth(3)?.trim().trim_end_matches('%').parse().ok()
}

/// Sets the screen's brightness (brightnessctl goes through logind when
/// the user can't write the backlight).
pub fn set_brightness(percent: u32) -> Result<(), String> {
    act("brightnessctl", &["-q", "-c", "backlight", "set", &format!("{percent}%")])
}

// --- Bluetooth ------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BtDev {
    pub mac: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bt {
    pub powered: bool,
    pub devices: Vec<BtDev>,
}

impl Bt {
    pub fn connected(&self) -> impl Iterator<Item = &BtDev> {
        self.devices.iter().filter(|d| d.connected)
    }
}

/// "Device AA:BB:CC:DD:EE:FF Some Name" lines → (mac, name).
pub fn parse_bt_devices(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Device ")?;
            let (mac, name) = rest.split_once(' ').unwrap_or((rest, rest));
            Some((mac.to_owned(), name.trim().to_owned()))
        })
        .collect()
}

/// The Bluetooth state; None without bluetoothctl or an adapter.
pub fn bt() -> Option<Bt> {
    let show = run("bluetoothctl", &["show"])?;
    if !show.contains("Controller") {
        return None;
    }
    let powered = show.lines().any(|l| l.trim() == "Powered: yes");
    let all = parse_bt_devices(&run("bluetoothctl", &["devices"]).unwrap_or_default());
    let paired = parse_bt_devices(
        &run("bluetoothctl", &["devices", "Paired"]).or_else(|| run("bluetoothctl", &["paired-devices"])).unwrap_or_default(),
    );
    let connected = parse_bt_devices(&run("bluetoothctl", &["devices", "Connected"]).unwrap_or_default());
    let mut devices: Vec<BtDev> = all
        .into_iter()
        .map(|(mac, name)| BtDev {
            paired: paired.iter().any(|(m, _)| *m == mac),
            connected: connected.iter().any(|(m, _)| *m == mac),
            mac,
            name,
        })
        // Unnamed devices found by a scan are just noise.
        .filter(|d| d.paired || d.name.replace('-', ":") != d.mac)
        .collect();
    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.paired.cmp(&a.paired)).then(a.name.cmp(&b.name)));
    Some(Bt { powered, devices })
}

#[derive(Debug, Clone)]
pub enum BtCmd {
    Power(bool),
    Connect(String),
    Disconnect(String),
    /// Pair, trust and connect a new device.
    Pair(String),
    /// Look for new devices for a few seconds.
    Scan,
}

pub fn bt_do(cmd: &BtCmd) -> Result<(), String> {
    match cmd {
        BtCmd::Power(on) => act("bluetoothctl", &["power", if *on { "on" } else { "off" }]),
        BtCmd::Connect(mac) => act("bluetoothctl", &["connect", mac]),
        BtCmd::Disconnect(mac) => act("bluetoothctl", &["disconnect", mac]),
        BtCmd::Pair(mac) => {
            act("bluetoothctl", &["pair", mac])?;
            act("bluetoothctl", &["trust", mac])?;
            act("bluetoothctl", &["connect", mac])
        }
        BtCmd::Scan => {
            let _ = act("bluetoothctl", &["--timeout", "8", "scan", "on"]);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_info() {
        assert_eq!(parse_brightness("intel_backlight,backlight,400,42%,937\n"), Some(42));
        assert_eq!(parse_brightness(""), None);
    }

    #[test]
    fn pactl_json() {
        let j = r#"[{"index":56,"name":"alsa_output.pci.analog-stereo","description":"Built-in Audio","mute":false,
            "volume":{"front-left":{"value":26214,"value_percent":"40%","db":"-23.88 dB"},"front-right":{"value":26214,"value_percent":"40%","db":"-23.88 dB"}}},
            {"index":57,"name":"alsa_output.pci.analog-stereo.monitor","description":"Monitor","mute":false,"volume":{}}]"#;
        let d = parse_devs(j);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].volume, d[0].desc.as_str(), d[0].muted), (40, "Built-in Audio", false));
    }

    #[test]
    fn nmcli_terse() {
        assert_eq!(nm_fields(r"a\:b:c\\d::e"), ["a:b", r"c\d", "", "e"]);
        let net = parse_net(
            "enabled\n",
            "*:Home:82:WPA2\n:Cafe\\: free:40:\n:Home:60:WPA2\n:Office:55:WPA1 WPA2\n::30:WPA2\n",
            "Home:u1:802-11-wireless:wlan0\nWired:u2:802-3-ethernet:\nwork-vpn:u3:vpn:\nlo:u4:loopback:lo\n",
        );
        assert!(net.wifi_enabled);
        let names: Vec<&str> = net.networks.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["Home", "Office", "Cafe: free"]);
        assert!(net.networks[0].active && net.networks[0].known && net.networks[0].secure);
        assert!(!net.networks[2].secure && !net.networks[2].known);
        let others: Vec<(&str, bool)> = net.others.iter().map(|c| (c.name.as_str(), c.active)).collect();
        assert_eq!(others, [("Wired", false), ("work-vpn", false)]);
    }

    #[test]
    fn proc_wireless() {
        let t = "Inter-| sta-|   Quality        |   Discarded packets\n face | tus | link level noise |  nwid  crypt\n wlan0: 0000   56.  -54.  -256        0      0\n";
        assert_eq!(parse_wireless(t, "wlan0"), Some(80));
        assert_eq!(parse_wireless(t, "wlan1"), None);
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(1430.0), "1.4 KB");
        assert_eq!(bytes(23_400_000.0), "23.4 MB");
        assert_eq!(bytes(234_000_000.0), "234 MB");
    }

    #[test]
    fn bluetoothctl_devices() {
        let d = parse_bt_devices("Device 00:11:22:33:44:55 Headphones X\nDevice AA:BB:CC:DD:EE:FF AA-BB-CC-DD-EE-FF\nnoise\n");
        assert_eq!(d[0], ("00:11:22:33:44:55".into(), "Headphones X".into()));
        assert_eq!(d.len(), 2);
    }
}
