//! The volume, network and bluetooth popups: state, messages and views.
//!
//! A click on one of those modules opens its popup right under it (above
//! it on a bottom bar), as a Wayland popup of the bar (HeroUI `popover`).
//! The state behind them comes from `system.rs` on background threads;
//! audio and NetworkManager changes arrive as events, so nothing polls
//! while they're idle.

use std::time::{Duration, Instant};

use heroui::fltk::draw;
use heroui::fltk::enums::Align;
use heroui::fltk::prelude::*;
use heroui::hover::hover_amount;
use heroui::prelude::*;

use crate::modules::Kind;
use crate::system::{self, Audio, AudioCmd, Bt, BtCmd, Net, NetCmd};
use crate::{Bar, Msg};

/// Popup width.
pub const WIDTH: i32 = 330;
const ROW: i32 = 36;
/// Most list rows shown before the list scrolls.
const MAX_ROWS: usize = 7;

#[derive(Default)]
pub struct Sys {
    /// The module whose popup is open.
    pub open: Option<usize>,
    /// When a popup last closed (a click on its module then closes it
    /// rather than reopening it).
    closed: Option<(usize, Instant)>,
    pub audio: Option<Audio>,
    /// Device names for the output/input lists.
    pub sink_names: Vec<String>,
    pub source_names: Vec<String>,
    audio_busy: bool,
    /// A volume change waiting for the previous one to finish.
    audio_pending: Option<AudioCmd>,
    audio_stale: bool,
    pub net: Option<Net>,
    pub ssid: Option<String>,
    pub scanning: bool,
    /// The secured network a password is being typed for.
    pub password_for: Option<String>,
    pub password: String,
    pub bt: Option<Bt>,
    pub bt_scanning: bool,
    /// What's going on / what went wrong, per popup.
    pub status: String,
}

#[derive(Clone, Debug)]
pub enum SysMsg {
    /// Show the current audio/network/Bluetooth state in all modules
    /// (after a rebuild).
    Refresh,
    Open(usize),
    Closed(usize),
    /// Run a module's "advanced" command (pavucontrol...) and close.
    Advanced(usize),
    Audio(Option<Audio>),
    AudioChanged,
    AudioDo(AudioCmd),
    AudioDone(Result<(), String>),
    Volume(bool, f64),
    Mute(bool),
    Device(bool, usize),
    Net(Option<Net>, bool),
    NetChanged,
    Ssid(Option<String>),
    NetDo(NetCmd),
    NetDone(Result<(), String>),
    Rescan,
    Wifi(usize),
    Password(String),
    SendPassword,
    Other(usize),
    Bt(Option<Bt>),
    BtDo(BtCmd),
    BtDone(Result<(), String>),
    BtDevice(usize),
}

fn task(f: impl FnOnce() -> SysMsg + Send + 'static) -> Task<Msg> {
    Task::perform(move || Msg::Sys(f()))
}

pub fn has_popup(kind: Kind) -> bool {
    matches!(kind, Kind::Volume | Kind::Network | Kind::Bluetooth)
}

/// The command behind "Advanced...": the module's on-click, else the
/// usual settings program of its service.
pub fn advanced(bar: &Bar, i: usize) -> String {
    let m = &bar.modules[i];
    m.command.clone().unwrap_or_else(|| {
        match m.kind {
            Kind::Volume => "pavucontrol",
            Kind::Network => "nm-connection-editor",
            _ => "blueman-manager",
        }
        .into()
    })
}

impl Bar {
    pub fn update_sys(&mut self, msg: SysMsg) -> Task<Msg> {
        let s = &mut self.sys;
        match msg {
            SysMsg::Refresh => {
                self.show_audio();
                self.show_net();
                self.show_bt();
            }
            SysMsg::Open(i) if s.open == Some(i) => {
                // A second click on the module closes it.
                s.open = None;
            }
            SysMsg::Open(i) => {
                // The click that closed it (outside the popup, on its
                // module) shouldn't open it again.
                if s.closed.is_some_and(|(j, t)| j == i && t.elapsed() < Duration::from_millis(300)) {
                    s.closed = None;
                    return Task::none();
                }
                s.open = Some(i);
                s.status.clear();
                s.password_for = None;
                return match self.modules[i].kind {
                    Kind::Volume => task(|| SysMsg::Audio(system::audio())),
                    Kind::Network => {
                        s.scanning = true;
                        Task::batch([task(|| SysMsg::Net(system::net(false), false)), task(|| SysMsg::Net(system::net(true), true))])
                    }
                    _ => task(|| SysMsg::Bt(system::bt())),
                };
            }
            SysMsg::Closed(i) => {
                if s.open == Some(i) {
                    s.open = None;
                    s.closed = Some((i, Instant::now()));
                }
            }
            SysMsg::Advanced(i) => {
                s.open = None;
                let cmd = advanced(self, i);
                return Task::perform(move || {
                    crate::modules::launch(&cmd);
                    Msg::Launched
                });
            }

            // Audio
            SysMsg::Audio(a) => {
                s.audio = a;
                self.show_audio();
            }
            SysMsg::AudioChanged => {
                // Our own volume changes echo back; read once they're done.
                if s.audio_busy {
                    s.audio_stale = true;
                } else {
                    return task(|| SysMsg::Audio(system::audio()));
                }
            }
            SysMsg::AudioDo(cmd) => {
                if s.audio_busy {
                    s.audio_pending = Some(cmd);
                } else {
                    s.audio_busy = true;
                    return task(move || SysMsg::AudioDone(system::audio_do(&cmd)));
                }
            }
            SysMsg::AudioDone(r) => {
                s.audio_busy = false;
                if let Err(e) = r {
                    s.status = e;
                }
                if let Some(cmd) = s.audio_pending.take() {
                    s.audio_busy = true;
                    return task(move || SysMsg::AudioDone(system::audio_do(&cmd)));
                }
                if std::mem::take(&mut s.audio_stale) {
                    return task(|| SysMsg::Audio(system::audio()));
                }
            }
            SysMsg::Volume(input, v) => {
                let Some(a) = s.audio.as_mut() else { return Task::none() };
                let percent = v.round() as u32;
                let dev = if input { a.default_source.clone() } else { a.default_sink.clone() };
                let list = if input { &mut a.sources } else { &mut a.sinks };
                let k = list.iter().position(|d| d.name == dev).unwrap_or(0);
                let Some(d) = list.get_mut(k) else { return Task::none() };
                if d.volume == percent {
                    return Task::none();
                }
                d.volume = percent;
                let name = d.name.clone();
                self.show_audio();
                return self.update_sys(SysMsg::AudioDo(AudioCmd::Volume { input, name, percent }));
            }
            SysMsg::Mute(input) => {
                let Some(a) = s.audio.as_mut() else { return Task::none() };
                let d = if input { a.source().cloned() } else { a.sink().cloned() };
                let Some(d) = d else { return Task::none() };
                let list = if input { &mut a.sources } else { &mut a.sinks };
                if let Some(x) = list.iter_mut().find(|x| x.name == d.name) {
                    x.muted = !d.muted;
                }
                self.show_audio();
                return self.update_sys(SysMsg::AudioDo(AudioCmd::Mute { input, name: d.name, muted: !d.muted }));
            }
            SysMsg::Device(input, i) => {
                let Some(a) = s.audio.as_mut() else { return Task::none() };
                let list = if input { &a.sources } else { &a.sinks };
                let Some(name) = list.get(i).map(|d| d.name.clone()) else { return Task::none() };
                if input {
                    a.default_source = name.clone();
                } else {
                    a.default_sink = name.clone();
                }
                self.show_audio();
                return self.update_sys(SysMsg::AudioDo(AudioCmd::Default { input, name }));
            }

            // Network
            SysMsg::Net(n, scanned) => {
                if scanned {
                    s.scanning = false;
                }
                if let Some(n) = n {
                    s.ssid = n.networks.iter().find(|w| w.active).map(|w| w.ssid.clone());
                    s.net = Some(n);
                } else if scanned {
                    s.net = None;
                }
                self.show_net();
            }
            SysMsg::NetChanged => {
                // The popup's list if it's open, else just the name shown.
                return if self.sys_open(Kind::Network) {
                    task(|| SysMsg::Net(system::net(false), false))
                } else {
                    task(|| SysMsg::Ssid(system::active_ssid()))
                };
            }
            SysMsg::Ssid(ssid) => {
                s.ssid = ssid;
                self.show_net();
            }
            SysMsg::Rescan => {
                s.scanning = true;
                return task(|| SysMsg::Net(system::net(true), true));
            }
            SysMsg::NetDo(cmd) => {
                s.status = match &cmd {
                    NetCmd::Connect { ssid, .. } => format!("Connecting to {ssid}..."),
                    NetCmd::Up(_) => "Connecting...".into(),
                    NetCmd::Down(_) | NetCmd::DownId(_) => "Disconnecting...".into(),
                    NetCmd::WifiEnabled(_) => String::new(),
                };
                return task(move || SysMsg::NetDone(system::net_do(&cmd)));
            }
            SysMsg::NetDone(r) => {
                s.status = match r {
                    Ok(()) => String::new(),
                    Err(e) => e,
                };
                return task(|| SysMsg::Net(system::net(false), false));
            }
            SysMsg::Wifi(i) => {
                let Some(w) = s.net.as_ref().and_then(|n| n.networks.get(i)).cloned() else { return Task::none() };
                if w.active {
                    return self.update_sys(SysMsg::NetDo(NetCmd::DownId(w.ssid)));
                }
                if w.secure && !w.known {
                    s.password_for = Some(w.ssid);
                    s.password.clear();
                    return Task::none();
                }
                return self.update_sys(SysMsg::NetDo(NetCmd::Connect { ssid: w.ssid, password: None, known: w.known }));
            }
            SysMsg::Password(p) => s.password = p,
            SysMsg::SendPassword => {
                let Some(ssid) = s.password_for.take() else { return Task::none() };
                let pw = std::mem::take(&mut s.password);
                return self.update_sys(SysMsg::NetDo(NetCmd::Connect { ssid, password: Some(pw), known: false }));
            }
            SysMsg::Other(i) => {
                let Some(c) = s.net.as_ref().and_then(|n| n.others.get(i)).cloned() else { return Task::none() };
                let cmd = if c.active { NetCmd::Down(c.uuid) } else { NetCmd::Up(c.uuid) };
                return self.update_sys(SysMsg::NetDo(cmd));
            }

            // Bluetooth
            SysMsg::Bt(b) => {
                s.bt = b;
                self.show_bt();
            }
            SysMsg::BtDo(cmd) => {
                s.status = match &cmd {
                    BtCmd::Connect(_) | BtCmd::Pair(_) => "Connecting...".into(),
                    BtCmd::Disconnect(_) => "Disconnecting...".into(),
                    BtCmd::Scan => {
                        s.bt_scanning = true;
                        "Looking for devices...".into()
                    }
                    BtCmd::Power(_) => String::new(),
                };
                return task(move || SysMsg::BtDone(system::bt_do(&cmd)));
            }
            SysMsg::BtDone(r) => {
                s.bt_scanning = false;
                s.status = match r {
                    Ok(()) => String::new(),
                    Err(e) => e,
                };
                return task(|| SysMsg::Bt(system::bt()));
            }
            SysMsg::BtDevice(i) => {
                let Some(d) = s.bt.as_ref().and_then(|b| b.devices.get(i)).cloned() else { return Task::none() };
                let cmd = if d.connected {
                    BtCmd::Disconnect(d.mac)
                } else if d.paired {
                    BtCmd::Connect(d.mac)
                } else {
                    BtCmd::Pair(d.mac)
                };
                return self.update_sys(SysMsg::BtDo(cmd));
            }
        }
        Task::none()
    }

    fn sys_open(&self, kind: Kind) -> bool {
        self.sys.open.is_some_and(|i| self.modules[i].kind == kind)
    }

    /// Volume modules show the default output.
    fn show_audio(&mut self) {
        let a = self.sys.audio.clone().unwrap_or_default();
        self.sys.sink_names = a.sinks.iter().map(|d| d.desc.clone()).collect();
        self.sys.source_names = a.sources.iter().map(|d| d.desc.clone()).collect();
        let sink = a.sink().cloned();
        for m in self.modules.iter_mut().filter(|m| m.kind == Kind::Volume) {
            m.set_output(sink.as_ref().map(|d| format!("{} {}", d.volume, u8::from(d.muted))).unwrap_or_default());
        }
    }

    fn show_net(&mut self) {
        let ssid = self.sys.ssid.clone();
        for m in self.modules.iter_mut().filter(|m| m.kind == Kind::Network) {
            m.essid = ssid.clone();
            m.refresh();
        }
    }

    fn show_bt(&mut self) {
        let bt = self.sys.bt.clone();
        for m in self.modules.iter_mut().filter(|m| m.kind == Kind::Bluetooth) {
            m.set_bt(bt.as_ref());
        }
    }
}

// --- Views ----------------------------------------------------------------

/// How a list row looks.
#[derive(Clone, PartialEq, Default)]
struct Look {
    icon: String,
    label: String,
    /// Small text on the right ("Connected", "Saved"...).
    note: String,
    /// An extra icon before the note (a lock).
    badge: String,
    active: bool,
}

/// A clickable row of a popup list.
fn item(look: impl Fn(&Bar) -> Look + 'static, msg: SysMsg) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let cur = std::rc::Rc::new(std::cell::RefCell::new(Look::default()));
        let mut b = custom_button({
            let cur = cur.clone();
            move |b| {
                let t = heroui::theme::current();
                let l = cur.borrow();
                let a = if b.value() { 1.0 } else { hover_amount(b) };
                if a > 0.0 {
                    draw::set_draw_color(heroui::widgets::mix(t.background, t.surface_alt, a));
                    draw::draw_rounded_rectf(b.x(), b.y(), b.w(), b.h(), t.radius.min(10));
                }
                let (x, y, w, h) = (b.x() + 8, b.y(), b.w() - 16, b.h());
                let fg = if l.active { t.accent } else { t.text };
                if !l.icon.is_empty() {
                    heroui::icons::draw(&l.icon, x, y + (h - 18) / 2, 18, fg);
                }
                draw::set_font(if l.active { t.bold_font() } else { t.font() }, t.font_size);
                draw::set_draw_color(fg);
                let tx = x + 28;
                let mut right = x + w;
                if !l.note.is_empty() {
                    draw::set_font(t.font(), t.font_size - 2);
                    let nw = draw::width(&l.note) as i32;
                    draw::set_draw_color(t.text_dim);
                    draw::draw_text2(&l.note, right - nw, y, nw, h, Align::Right | Align::Inside);
                    right -= nw + 6;
                    draw::set_font(if l.active { t.bold_font() } else { t.font() }, t.font_size);
                    draw::set_draw_color(fg);
                }
                if !l.badge.is_empty() {
                    heroui::icons::draw(&l.badge, right - 14, y + (h - 14) / 2, 14, t.text_dim);
                    right -= 20;
                }
                draw::push_clip(tx, y, (right - tx).max(0), h);
                draw::draw_text2(&l.label, tx, y, (right - tx).max(0), h, Align::Left | Align::Inside);
                draw::pop_clip();
            }
        });
        let emit = ctx.emitter();
        b.set_callback(move |_| emit(Msg::Sys(msg.clone())));
        let mut w = b.clone();
        ctx.bind(move |bar: &Bar| {
            let l = look(bar);
            if *cur.borrow() != l {
                *cur.borrow_mut() = l;
                heroui::widgets::repaint(&mut w);
            }
        });
        b.as_base_widget()
    })
    .fixed(ROW)
}

/// A dim line of text that changes (status, errors).
fn note(f: impl Fn(&Bar) -> String + 'static) -> Element<Bar, Msg> {
    canvas(f, |s: &String, x, y, w, h, t: &Theme| {
        draw::set_font(t.font(), t.font_size - 2);
        draw::set_draw_color(t.text_dim);
        draw::draw_text2(s, x, y, w, h, Align::Left | Align::Inside | Align::Clip);
    })
}

fn heading_row(text: &'static str) -> Element<Bar, Msg> {
    heading(text).fixed(30)
}

fn advanced_button(i: usize, label: &'static str) -> Element<Bar, Msg> {
    row(vec![spacer(), button(label, Msg::Sys(SysMsg::Advanced(i))).fixed(170)]).fixed(34)
}

/// Height of `n` list rows (the list puts the theme spacing between them).
fn rows_height(n: usize) -> i32 {
    let n = n.min(MAX_ROWS) as i32;
    let gap = heroui::theme::current().spacing;
    (n * (ROW + gap) - gap).max(0)
}

/// A mute button showing `icon(state)`.
fn mute_button(input: bool, icon: impl Fn(&Bar) -> String + 'static) -> Element<Bar, Msg> {
    let icon = std::rc::Rc::new(icon);
    Element::new(move |ctx| {
        let cur = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let mut b = custom_button({
            let cur = cur.clone();
            move |b| {
                let t = heroui::theme::current();
                let a = if b.value() { 1.0 } else { hover_amount(b) };
                if a > 0.0 {
                    draw::set_draw_color(heroui::widgets::mix(t.background, t.surface_alt, a));
                    draw::draw_rounded_rectf(b.x(), b.y(), b.w(), b.h(), t.radius.min(b.h() / 2));
                }
                heroui::icons::draw(&cur.borrow(), b.x() + (b.w() - 20) / 2, b.y() + (b.h() - 20) / 2, 20, t.text);
            }
        });
        let emit = ctx.emitter();
        b.set_callback(move |_| emit(Msg::Sys(SysMsg::Mute(input))));
        let mut w = b.clone();
        let icon = icon.clone();
        ctx.bind(move |bar: &Bar| {
            let i = icon(bar);
            if *cur.borrow() != i {
                *cur.borrow_mut() = i;
                heroui::widgets::repaint(&mut w);
            }
        });
        b.as_base_widget()
    })
    .fixed(34)
}

fn level_icon(base: &str, d: Option<&system::AudioDev>) -> String {
    match d {
        None => format!("{base}-muted"),
        Some(d) if d.muted || d.volume == 0 => format!("{base}-muted"),
        Some(_) if base == "microphone" => "microphone".into(),
        Some(d) if d.volume < 50 => "volume-low".into(),
        Some(_) => "volume-high".into(),
    }
}

pub fn volume_view(i: usize) -> Element<Bar, Msg> {
    let audio = |b: &Bar| b.sys.audio.clone().unwrap_or_default();
    column(vec![
        heading_row("Output"),
        row(vec![
            mute_button(false, move |b| level_icon("volume", audio(b).sink())),
            slider(0.0..=100.0, move |b: &Bar| audio(b).sink().map_or(0.0, |d| d.volume as f64), |v| Msg::Sys(SysMsg::Volume(false, v))),
            text(move |b: &Bar| audio(b).sink().map(|d| format!("{}%", d.volume)).unwrap_or_default()).fixed(44),
        ])
        .fixed(34),
        dropdown(
            |b: &Bar| &b.sys.sink_names[..],
            move |b: &Bar| {
                let a = audio(b);
                a.sinks.iter().position(|d| d.name == a.default_sink).unwrap_or(0)
            },
            |k| Msg::Sys(SysMsg::Device(false, k)),
        )
        .fixed(34),
        heading_row("Input"),
        row(vec![
            mute_button(true, move |b| level_icon("microphone", audio(b).source())),
            slider(0.0..=100.0, move |b: &Bar| audio(b).source().map_or(0.0, |d| d.volume as f64), |v| Msg::Sys(SysMsg::Volume(true, v))),
            text(move |b: &Bar| audio(b).source().map(|d| format!("{}%", d.volume)).unwrap_or_default()).fixed(44),
        ])
        .fixed(34)
        .visible(move |b: &Bar| !audio(b).sources.is_empty()),
        dropdown(
            |b: &Bar| &b.sys.source_names[..],
            move |b: &Bar| {
                let a = audio(b);
                a.sources.iter().position(|d| d.name == a.default_source).unwrap_or(0)
            },
            |k| Msg::Sys(SysMsg::Device(true, k)),
        )
        .fixed(34)
        .visible(move |b: &Bar| !audio(b).sources.is_empty()),
        note(|b: &Bar| {
            if b.sys.audio.is_none() {
                "No sound server found (needs pactl: pulseaudio-utils).".into()
            } else {
                b.sys.status.clone()
            }
        })
        .fixed(22),
        advanced_button(i, "Advanced..."),
    ])
    .padding(12)
    .spacing(6)
}

pub fn volume_size(b: &Bar) -> (i32, i32) {
    let input = b.sys.audio.as_ref().is_some_and(|a| !a.sources.is_empty());
    (WIDTH, if input { 330 } else { 230 })
}

pub fn net_view(i: usize) -> Element<Bar, Msg> {
    let net = |b: &Bar| b.sys.net.clone().unwrap_or_default();
    column(vec![
        row(vec![
            heading("Wi-Fi"),
            button("Scan", Msg::Sys(SysMsg::Rescan)).fixed(70).enabled(|b: &Bar| !b.sys.scanning),
            toggle("", move |b: &Bar| net(b).wifi_enabled, |on| Msg::Sys(SysMsg::NetDo(NetCmd::WifiEnabled(on)))).fixed(56),
        ])
        .fixed(32)
        .visible(move |b: &Bar| net(b).has_wifi),
        scroll(vec![list(
            move |b: &Bar| if net(b).wifi_enabled { net(b).networks.len() } else { 0 },
            move |k| {
                item(
                    move |b: &Bar| {
                        let n = b.sys.net.as_ref().and_then(|n| n.networks.get(k)).cloned().unwrap_or_default();
                        Look {
                            icon: format!("network-wireless-{}", n.signal),
                            note: if n.active { "Connected".into() } else if n.known { "Saved".into() } else { String::new() },
                            badge: if n.secure { "lock".into() } else { String::new() },
                            active: n.active,
                            label: n.ssid,
                        }
                    },
                    SysMsg::Wifi(k),
                )
            },
        )])
        .fixed_with(move |b: &Bar| if net(b).wifi_enabled { rows_height(net(b).networks.len()) } else { 0 }),
        // Password for a secured network.
        note(|b: &Bar| b.sys.password_for.as_ref().map(|s| format!("Password for {s}")).unwrap_or_default())
            .fixed(20)
            .visible(|b: &Bar| b.sys.password_for.is_some()),
        row(vec![
            text_input_submit(|b: &Bar| b.sys.password.clone(), |p| Msg::Sys(SysMsg::Password(p)), Msg::Sys(SysMsg::SendPassword)),
            primary_button("Connect", Msg::Sys(SysMsg::SendPassword)).fixed(90),
        ])
        .fixed(34)
        .visible(|b: &Bar| b.sys.password_for.is_some()),
        heading_row("Connections").visible(move |b: &Bar| !net(b).others.is_empty()),
        list(
            move |b: &Bar| net(b).others.len(),
            move |k| {
                item(
                    move |b: &Bar| {
                        let c = b.sys.net.as_ref().and_then(|n| n.others.get(k)).cloned().unwrap_or_default();
                        Look {
                            icon: if c.kind == "ethernet" { "network-wired".into() } else { "lock".into() },
                            note: if c.active { "Connected".into() } else { c.kind.clone() },
                            badge: String::new(),
                            active: c.active,
                            label: c.name,
                        }
                    },
                    SysMsg::Other(k),
                )
            },
        ),
        note(|b: &Bar| {
            if b.sys.net.is_none() && !b.sys.scanning {
                "NetworkManager isn't running (needs nmcli).".into()
            } else if b.sys.scanning && b.sys.status.is_empty() {
                "Looking for networks...".into()
            } else {
                b.sys.status.clone()
            }
        })
        .fixed(22),
        advanced_button(i, "Network settings..."),
    ])
    .padding(12)
    .spacing(6)
}

pub fn net_size(b: &Bar) -> (i32, i32) {
    let n = b.sys.net.as_ref();
    let wifi = n.filter(|n| n.wifi_enabled).map_or(0, |n| n.networks.len());
    let others = n.map_or(0, |n| n.others.len());
    let gap = 6;
    // padding, header, list, password, connections, note, button
    let mut h = 12 + 32 + gap + rows_height(wifi) + gap;
    if b.sys.password_for.is_some() {
        h += 20 + gap + 34 + gap;
    }
    if others > 0 {
        h += 30 + gap + rows_height(others) + gap;
    }
    h += 22 + gap + 34 + 12;
    (WIDTH, h.clamp(150, 640))
}

pub fn bt_view(i: usize) -> Element<Bar, Msg> {
    let bt = |b: &Bar| b.sys.bt.clone().unwrap_or_default();
    column(vec![
        row(vec![
            heading("Bluetooth"),
            button("Scan", Msg::Sys(SysMsg::BtDo(BtCmd::Scan))).fixed(70).enabled(move |b: &Bar| !b.sys.bt_scanning && bt(b).powered),
            toggle("", move |b: &Bar| bt(b).powered, |on| Msg::Sys(SysMsg::BtDo(BtCmd::Power(on)))).fixed(56),
        ])
        .fixed(32),
        scroll(vec![list(
            move |b: &Bar| if bt(b).powered { bt(b).devices.len() } else { 0 },
            move |k| {
                item(
                    move |b: &Bar| {
                        let d = b.sys.bt.as_ref().and_then(|x| x.devices.get(k)).cloned().unwrap_or_default();
                        Look {
                            icon: if d.connected { "bluetooth-connected".into() } else { "bluetooth".into() },
                            note: if d.connected {
                                "Connected".into()
                            } else if d.paired {
                                "Paired".into()
                            } else {
                                "New".into()
                            },
                            badge: String::new(),
                            active: d.connected,
                            label: d.name,
                        }
                    },
                    SysMsg::BtDevice(k),
                )
            },
        )])
        .fixed_with(move |b: &Bar| if bt(b).powered { rows_height(bt(b).devices.len()) } else { 0 }),
        note(|b: &Bar| {
            if b.sys.bt.is_none() {
                "No Bluetooth adapter (or bluetoothctl) found.".into()
            } else {
                b.sys.status.clone()
            }
        })
        .fixed(22),
        advanced_button(i, "Bluetooth settings..."),
    ])
    .padding(12)
    .spacing(6)
}

pub fn bt_size(b: &Bar) -> (i32, i32) {
    let n = b.sys.bt.as_ref().filter(|x| x.powered).map_or(0, |x| x.devices.len());
    (WIDTH, (12 + 32 + 6 + rows_height(n) + 6 + 22 + 6 + 34 + 12).clamp(130, 520))
}
