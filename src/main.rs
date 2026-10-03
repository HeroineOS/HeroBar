//! herobar: a lightweight status bar built on HeroUI.
//!
//! A layer-shell panel on Wayland compositors that support it (HeroWM,
//! sway, Hyprland, KDE...), a dock window with a strut on X11.

mod apps;
mod config;
mod fit;
mod modules;
mod popups;
mod reload;
mod system;
mod taskbar;
mod windows;
mod workspaces;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use heroui::fltk::draw;
use heroui::fltk::enums::{Align, Color, FrameType};
use heroui::fltk::frame::Frame;
use heroui::fltk::group::Flex;
use heroui::fltk::prelude::*;
use heroui::hover::is_hovered;
use heroui::prelude::*;

use modules::{Kind, Module};

struct Bar {
    config: config::Config,
    watch: reload::Watch,
    /// Every module, including the ones inside groups.
    modules: Vec<Module>,
    /// Module indexes per section.
    left: Vec<usize>,
    center: Vec<usize>,
    right: Vec<usize>,
    /// Group of each module (by index), if it's in one.
    parent: Vec<Option<usize>>,
    /// Groups whose drawer is open.
    open: HashSet<usize>,
    /// Windows and workspaces, for taskbar and workspaces modules.
    desktop: taskbar::Desktop,
    /// Audio, network, Bluetooth and their popups.
    sys: popups::Sys,
}

#[derive(Clone)]
enum Msg {
    /// Time to refresh module `i`.
    Tick(usize),
    /// Output of module `i`'s command.
    Output(usize, String),
    /// Module `i` was clicked.
    Click(usize),
    Launched,
    /// Once a second: did the config or the theme change?
    CheckReload,
    /// News from the compositor about windows and workspaces.
    Windows(windows::Update),
    /// A taskbar button was clicked (with mouse button 1-3).
    Task(taskbar::Item, i32),
    /// Switch to a workspace.
    Workspace(u64),
    /// Open or close group `i`'s drawer.
    Drawer(usize),
    /// Volume, network and Bluetooth popups and state.
    Sys(popups::SysMsg),
}

impl Bar {
    fn new(config: config::Config, watch: reload::Watch) -> Bar {
        let mut modules = Vec::new();
        let mut parent = Vec::new();
        let mut section = |names: &[String]| {
            let mut idx = Vec::new();
            for name in names {
                let cfg = config.modules.get(name);
                modules.push(Module::new(name, cfg));
                parent.push(None);
                let g = modules.len() - 1;
                idx.push(g);
                // A group's modules follow it.
                if modules[g].kind == Kind::Group {
                    for member in cfg.and_then(|c| c.modules.as_ref()).into_iter().flatten() {
                        modules.push(Module::new(member, config.modules.get(member)));
                        parent.push(Some(g));
                    }
                }
            }
            idx
        };
        let left = section(&config.bar.modules_left);
        let center = section(&config.bar.modules_center);
        let right = section(&config.bar.modules_right);
        Bar { config, watch, modules, left, center, right, parent, open: HashSet::new(), desktop: taskbar::Desktop::default(), sys: popups::Sys::default() }
    }

    /// The modules in group `g`.
    fn members(&self, g: usize) -> Vec<usize> {
        (0..self.modules.len()).filter(|&i| self.parent[i] == Some(g)).collect()
    }

    /// True if module `i` is in a closed drawer.
    fn hidden(&self, i: usize) -> bool {
        self.parent[i].is_some_and(|g| self.modules[g].cfg.drawer == Some(true) && !self.open.contains(&g))
    }

    fn launch(cmd: String) -> Task<Msg> {
        Task::perform(move || {
            modules::launch(&cmd);
            Msg::Launched
        })
    }

    /// The view of module `i` (any kind).
    fn module_element(&self, i: usize, center: bool) -> Element<Bar, Msg> {
        let m = &self.modules[i];
        let in_group = self.parent[i].is_some();
        match m.kind {
            Kind::Taskbar => taskbar::view(i),
            Kind::Workspaces => workspaces::view(i, m.font_size, in_group),
            Kind::Spacer => spacer_view(&m.cfg, center || in_group),
            Kind::Group => {
                let members = self.members(i).into_iter().map(|j| self.module_element(j, center)).collect();
                group_view(i, members, m.cfg.drawer == Some(true), m.icon.clone())
            }
            k if popups::has_popup(k) && m.cfg.popup != Some(false) => {
                let (content, size): (Element<Bar, Msg>, PopupSize) = match k {
                    Kind::Volume => (popups::volume_view(i), popups::volume_size),
                    Kind::Network => (popups::net_view(i), popups::net_size),
                    _ => (popups::bt_view(i), popups::bt_size),
                };
                popover(
                    module_view(i, Click::Popup, in_group),
                    move |b: &Bar| b.sys.open == Some(i),
                    Msg::Sys(popups::SysMsg::Closed(i)),
                    size,
                    content,
                )
            }
            _ => module_view(i, if m.command.is_some() { Click::Command } else { Click::None }, in_group),
        }
    }
}

impl App for Bar {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Sys(m) => return self.update_sys(m),
            Msg::Tick(i) if self.modules[i].kind == Kind::Bluetooth => {
                return Task::perform(|| Msg::Sys(popups::SysMsg::Bt(system::bt())));
            }
            Msg::Tick(i) => {
                let m = &mut self.modules[i];
                if let Some(cmd) = m.exec.clone() {
                    return Task::perform(move || Msg::Output(i, modules::run_exec(&cmd)));
                }
                m.refresh();
            }
            Msg::Output(i, out) => self.modules[i].set_output(out),
            Msg::Click(i) => {
                if let Some(cmd) = self.modules[i].command.clone() {
                    return Self::launch(cmd);
                }
            }
            Msg::Launched => {}
            Msg::Windows(windows::Update::Ready(c)) => self.desktop.control = Some(c),
            Msg::Windows(windows::Update::Windows(w)) => self.desktop.windows = w,
            Msg::Windows(windows::Update::Workspaces(w)) => self.desktop.workspaces = w,
            Msg::Task(item, button) => {
                if let Some(cmd) = taskbar::click(&item, button, &self.desktop.windows, self.desktop.control.as_ref()) {
                    return Self::launch(cmd);
                }
            }
            Msg::Workspace(id) => {
                if let Some(c) = &self.desktop.control {
                    c.send(windows::Cmd::FocusWorkspace(id));
                }
            }
            Msg::Drawer(g) => {
                if !self.open.remove(&g) {
                    self.open.insert(g);
                }
            }
            Msg::CheckReload => {
                if self.watch.changed() {
                    // Don't trade a working bar for a broken config.
                    let broken = self.watch.config.as_ref().and_then(|p| {
                        let text = std::fs::read_to_string(p).ok()?;
                        config::parse(&text).err().map(|e| format!("{}: {e}", p.display()))
                    });
                    match broken {
                        Some(e) => eprintln!("herobar: not reloading, the config has errors:\n{e}"),
                        None => {
                            let e = reload::restart();
                            eprintln!("herobar: reload failed: {e}");
                        }
                    }
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<Self, Msg> {
        let section = |which: Section, idx: &[usize]| {
            let center = which == Section::Center;
            let mut items: Vec<Element<Bar, Msg>> = idx.iter().map(|&i| self.module_element(i, center)).collect();
            // Left items pack to the left, right items to the right: the
            // free space is at the inner end (shared with expanding spacers).
            match which {
                Section::Left => items.push(flexible_space()),
                Section::Right => items.insert(0, flexible_space()),
                Section::Center => {}
            }
            let row = row(items).spacing(self.config.bar.spacing);
            if center {
                content_sized(row).fixed(0)
            } else {
                row
            }
        };
        let pad = self.config.bar.padding;
        with_margins(
            row(vec![
                section(Section::Left, &self.left),
                section(Section::Center, &self.center),
                section(Section::Right, &self.right),
            ])
            .spacing(0),
            (pad, 0, pad, 0),
        )
    }

    fn init(&mut self) -> Task<Msg> {
        // Commands and Bluetooth produce their first output right away.
        let first: Vec<usize> = (0..self.modules.len())
            .filter(|&i| self.modules[i].exec.is_some() || self.modules[i].kind == Kind::Bluetooth)
            .collect();
        let mut tasks: Vec<Task<Msg>> = first.into_iter().map(|i| self.update(Msg::Tick(i))).collect();
        let has = |k: Kind| self.modules.iter().any(|m| m.kind == k);
        if has(Kind::Volume) && system::have("pactl") {
            tasks.push(Task::perform(|| Msg::Sys(popups::SysMsg::Audio(system::audio()))));
        }
        if has(Kind::Network) && system::have("nmcli") {
            tasks.push(Task::perform(|| Msg::Sys(popups::SysMsg::Ssid(system::active_ssid()))));
        }
        Task::batch(tasks)
    }

    fn subscriptions(&self) -> Vec<Subscription<Msg>> {
        let desktop = self.modules.iter().any(|m| matches!(m.kind, Kind::Taskbar | Kind::Workspaces));
        // The monitor whose workspaces to show, if one is configured.
        let output = self.modules.iter().find(|m| m.kind == Kind::Workspaces).and_then(|m| m.cfg.output.clone());
        self.modules
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_dynamic())
            .map(|(i, m)| Subscription::every(Duration::from_secs_f64(m.interval), Msg::Tick(i)))
            .chain([Subscription::every(Duration::from_secs(1), Msg::CheckReload)])
            .chain(desktop.then(|| {
                Subscription::worker(move |tx: heroui::Sender<Msg>| windows::run(output, move |u| tx.send(Msg::Windows(u))))
            }))
            // Sound server and NetworkManager events, instead of polling.
            .chain((self.modules.iter().any(|m| m.kind == Kind::Volume && m.exec.is_none()) && system::have("pactl")).then(|| {
                Subscription::worker(|tx: heroui::Sender<Msg>| system::audio_watch(move || tx.send(Msg::Sys(popups::SysMsg::AudioChanged))))
            }))
            .chain((self.modules.iter().any(|m| m.kind == Kind::Network) && system::have("nmcli")).then(|| {
                Subscription::worker(|tx: heroui::Sender<Msg>| system::net_watch(move || tx.send(Msg::Sys(popups::SysMsg::NetChanged))))
            }))
            .collect()
    }

    fn theme(&self) -> Theme {
        let mut t = Theme::load();
        let s = &self.config.style;
        let color = |c: &Option<String>| c.as_deref().and_then(config::parse_color).map(|(r, g, b)| Color::from_rgb(r, g, b));
        if let Some(c) = color(&s.background) {
            t.background = c;
        }
        if let Some(c) = color(&s.foreground) {
            t.text = c;
        }
        if let Some(c) = color(&s.hover) {
            t.surface_alt = c;
        }
        if let Some(f) = &s.font {
            t.font = f.clone();
        }
        if let Some(n) = s.font_size {
            t.font_size = n;
        }
        t
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Section {
    Left,
    Center,
    Right,
}

/// Sets a row/column's margins per side (left, top, right, bottom).
fn with_margins(el: Element<Bar, Msg>, (l, t, r, b): (i32, i32, i32, i32)) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let w = el.build(ctx);
        if let Some(mut f) = Flex::from_dyn_widget(&w) {
            f.set_margins(l, t, r, b);
        }
        w
    })
}

/// Marks a row as sized to its content (see `fit`).
fn content_sized(el: Element<Bar, Msg>) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let w = el.build(ctx);
        if let Some(f) = Flex::from_dyn_widget(&w) {
            fit::content_sized(&f);
        }
        w
    })
}

/// Empty space that shares the row's free room.
fn flexible_space() -> Element<Bar, Msg> {
    Element::new(|_| {
        let mut f = Frame::default();
        f.set_frame(FrameType::NoBox);
        fit::flexible(&f);
        f.as_base_widget()
    })
}

/// Sizes from [style] (None: from the theme).
#[derive(Clone, Copy)]
struct Sizes {
    padding: i32,
    margin: i32,
    icon: Option<i32>,
}

thread_local! {
    /// The island style, when modules sit on islands.
    static ISLANDS: Cell<Option<config::IslandStyle>> = const { Cell::new(None) };
    static SIZES: Cell<Sizes> = const { Cell::new(Sizes { padding: 10, margin: 3, icon: None }) };
}

/// Space above and below module backgrounds.
pub fn margin() -> i32 {
    SIZES.with(Cell::get).margin
}

/// Gap between a module's icon and its text.
const ICON_GAP: i32 = 6;

fn island_radius(h: i32) -> i32 {
    let t = heroui::theme::current();
    match ISLANDS.with(Cell::get) {
        Some(config::IslandStyle::Sharp) => 0,
        Some(config::IslandStyle::Pill) => h / 2,
        _ => t.radius.min(h / 2).min(10),
    }
}

/// Paints a module's island (when islands are on): its own background,
/// with the bar's see-through gaps around it.
pub fn island(x: i32, y: i32, w: i32, h: i32) {
    if ISLANDS.with(Cell::get).is_none() {
        return;
    }
    let t = heroui::theme::current();
    // On a see-through bar the island is the bar color; on an opaque one
    // (X11, no fork) it has to stand out from it.
    draw::set_draw_color(if heroui::is_transparent() { t.background } else { t.surface });
    let m = margin();
    let (y, h) = (y + m, h - 2 * m);
    let r = island_radius(h);
    if r == 0 {
        draw::draw_rectf(x, y, w, h);
    } else {
        draw::draw_rounded_rectf(x, y, w, h, r);
    }
}

/// A module's resolved sizes: its own, else [style]'s, else the theme's.
#[derive(Clone, Copy)]
struct ModSizes {
    padding: i32,
    icon: Option<i32>,
    font: Option<i32>,
}

impl ModSizes {
    fn of(m: &Module) -> ModSizes {
        let s = SIZES.with(Cell::get);
        ModSizes { padding: m.padding.unwrap_or(s.padding), icon: m.icon_size.or(s.icon), font: m.font_size }
    }
    fn font(&self, t: &Theme) -> i32 {
        self.font.unwrap_or(t.font_size)
    }
    fn icon(&self, t: &Theme) -> i32 {
        self.icon.unwrap_or(self.font(t) + 2)
    }
}

/// A module's width for its icon and text (0 hides it). Built-in modules
/// with nothing to report (no battery, no audio) hide, icon and all; a
/// custom one can be just an icon.
fn module_width(icon: &str, text: &str, custom: bool, sz: ModSizes) -> i32 {
    if text.is_empty() && !custom {
        return 0;
    }
    let t = heroui::theme::current();
    draw::set_font(t.font(), sz.font(&t));
    let text_w = if text.is_empty() { 0 } else { draw::width(text).ceil() as i32 };
    let icon_w = if icon.is_empty() { 0 } else { sz.icon(&t) + if text.is_empty() { 0 } else { ICON_GAP } };
    if text_w + icon_w == 0 {
        0
    } else {
        text_w + icon_w + 2 * sz.padding
    }
}

/// Paints a module: island (unless in a group), hover, icon, text.
fn paint_module(w: &dyn WidgetExt, icon: &str, text: &str, hovered: bool, in_group: bool, sz: ModSizes) {
    let t = heroui::theme::current();
    if !in_group {
        island(w.x(), w.y(), w.w(), w.h());
    }
    if hovered {
        draw::set_draw_color(t.surface_alt);
        let m = margin();
        let h = w.h() - 2 * m;
        let r = if ISLANDS.with(Cell::get).is_some() { island_radius(h) } else { t.radius.min(h / 2) };
        draw::draw_rounded_rectf(w.x(), w.y() + m, w.w(), h, r);
    }
    let mut x = w.x() + sz.padding;
    if !icon.is_empty() {
        let s = sz.icon(&t);
        heroui::icons::draw(icon, x, w.y() + (w.h() - s) / 2, s, t.text);
        x += s + ICON_GAP;
    }
    draw::set_draw_color(t.text);
    draw::set_font(t.font(), sz.font(&t));
    draw::draw_text2(text, x, w.y(), w.x() + w.w() - x, w.h(), Align::Left | Align::Inside);
}

/// A popup's size from the state.
type PopupSize = fn(&Bar) -> (i32, i32);

/// What clicking a module does.
#[derive(Clone, Copy, PartialEq)]
enum Click {
    None,
    /// Runs its on-click command (on release, like a button).
    Command,
    /// Opens its popup (on press: Wayland grants popups for a press).
    Popup,
}

/// A module: its icon and text, sized to fit; clickable if it has an
/// on-click action or a popup.
fn module_view(i: usize, click: Click, in_group: bool) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        // (icon, text) shown.
        let shown: Rc<RefCell<(String, String)>> = Rc::default();
        let sizes: Rc<Cell<ModSizes>> = Rc::new(Cell::new(ModSizes { padding: 10, icon: None, font: None }));
        let paint = {
            let shown = shown.clone();
            let sizes = sizes.clone();
            move |w: &mut dyn WidgetExt, hovered: bool| {
                let (icon, text) = &*shown.borrow();
                paint_module(w, icon, text, hovered, in_group, sizes.get());
            }
        };
        // Clickable modules are buttons (FLTK handles the clicks, HeroUI the
        // hover); the others are plain frames.
        let widget = if click == Click::Popup {
            let mut b = press_button(move |b| {
                let hovered = is_hovered(b) || b.value();
                paint(b, hovered)
            });
            let emit = ctx.emitter();
            b.set_callback(move |b| {
                if b.value() {
                    emit(Msg::Sys(popups::SysMsg::Open(i)))
                }
            });
            b.as_base_widget()
        } else if click == Click::Command {
            let mut b = custom_button(move |b| {
                let hovered = is_hovered(b) || b.value();
                paint(b, hovered)
            });
            let emit = ctx.emitter();
            b.set_callback(move |_| emit(Msg::Click(i)));
            b.as_base_widget()
        } else {
            let mut f = Frame::default();
            f.set_frame(FrameType::NoBox);
            f.draw(move |f| paint(f, false));
            f.as_base_widget()
        };
        let mut w = widget.clone();
        let last_width = Cell::new(-1);
        let was_hidden = Cell::new(false);
        ctx.bind(move |bar: &Bar| {
            let m = &bar.modules[i];
            let hidden = bar.hidden(i);
            // The first run always sizes the module (last_width starts at
            // -1), so one with nothing to show takes no space instead of a
            // share of the bar.
            {
                let cur = shown.borrow();
                let hidden = hidden || m.absent;
                if cur.0 == m.icon && cur.1 == m.text && last_width.get() >= 0 && was_hidden.get() == hidden {
                    return;
                }
            }
            was_hidden.set(hidden || m.absent);
            sizes.set(ModSizes::of(m));
            *shown.borrow_mut() = (m.icon.clone(), m.text.clone());
            // Custom and Bluetooth modules may be just an icon.
            let icon_only = matches!(m.kind, Kind::Custom | Kind::Bluetooth);
            let mut width = if hidden || m.absent { 0 } else { module_width(&m.icon, &m.text, icon_only, sizes.get()) };
            // Changing numbers (network speeds) would make the module and
            // its neighbors jitter: it grows at once but only shrinks when
            // it's clearly narrower.
            if m.jittery() && width > 0 && width < last_width.get() && width * 4 > last_width.get() * 3 {
                width = last_width.get();
            }
            if width != last_width.replace(width) {
                fit::set_width(&mut w, width);
            }
            repaint(&mut w);
        });
        widget
    })
}

/// A spacer: fixed `width`, or (`expand`, outside the center) a share of
/// the section's free space. Optionally a line or dots in the middle.
fn spacer_view(cfg: &config::Module, fixed_only: bool) -> Element<Bar, Msg> {
    let expand = cfg.expand == Some(true) && !fixed_only;
    let width = cfg.width.unwrap_or(if expand { 0 } else { 12 }).max(0);
    let style = cfg.style.clone().unwrap_or_default();
    Element::new(move |ctx| {
        let mut f = Frame::default();
        f.set_frame(FrameType::NoBox);
        let style = style.clone();
        f.draw(move |f| {
            let t = heroui::theme::current();
            let m = margin() + 4;
            let (cx, y0, y1) = (f.x() + f.w() / 2, f.y() + m, f.y() + f.h() - m);
            draw::set_draw_color(heroui::widgets::mix(t.text, t.background, 0.6));
            match style.as_str() {
                "line" => draw::draw_line(cx, y0, cx, y1),
                "dots" => {
                    let mut y = y0 + 1;
                    while y < y1 {
                        draw::draw_rectf(cx, y, 2, 2);
                        y += 5;
                    }
                }
                _ => {}
            }
        });
        let mut w = f.as_base_widget();
        if expand {
            fit::flexible(&f);
        } else {
            let done = Cell::new(false);
            ctx.bind(move |_: &Bar| {
                if !done.replace(true) {
                    fit::set_width(&mut w, width);
                }
            });
        }
        f.as_base_widget()
    })
}

/// A group: its modules side by side on one background. With `drawer`,
/// only an icon shows until it's clicked.
fn group_view(g: usize, members: Vec<Element<Bar, Msg>>, drawer: bool, icon: String) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let mut children = Vec::with_capacity(members.len() + 1);
        if drawer {
            children.push(drawer_toggle(g, icon));
        }
        children.extend(members);
        let w = row(children).spacing(0).build(ctx);
        if let Some(mut f) = Flex::from_dyn_widget(&w) {
            fit::content_sized(&f);
            // One island behind all the members; FLTK draws them on top.
            f.super_draw_first(false);
            f.draw(|f| island(f.x(), f.y(), f.w(), f.h()));
        }
        w
    })
}

/// A drawer's button: the group's icon, highlighted while open.
fn drawer_toggle(g: usize, icon: String) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let open = Rc::new(Cell::new(false));
        let sizes = SIZES.with(Cell::get);
        let mut b = custom_button({
            let open = open.clone();
            move |b| {
                let hovered = is_hovered(b) || b.value() || open.get();
                let sz = ModSizes { padding: sizes.padding, icon: sizes.icon, font: None };
                paint_module(b, &icon, "", hovered, true, sz);
            }
        });
        let emit = ctx.emitter();
        b.set_callback(move |_| emit(Msg::Drawer(g)));
        let mut w = b.as_base_widget();
        let first = Cell::new(true);
        ctx.bind(move |bar: &Bar| {
            let is_open = bar.open.contains(&g);
            if first.replace(false) {
                let t = heroui::theme::current();
                let width = sizes.icon.unwrap_or(t.font_size + 2) + 2 * sizes.padding;
                fit::set_width(&mut w, width);
            }
            if open.replace(is_open) != is_open {
                repaint(&mut w);
            }
        });
        b.as_base_widget()
    })
}

fn usage() -> &'static str {
    "Usage: herobar [--config FILE] [--check] [--print-default-config]

  --config FILE           Use FILE instead of ~/.config/hero/bar.toml
  --check                 Validate the config and exit (status 1 if invalid)
  --print-default-config  Print the commented default config
  --version               Print the version"
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut check = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => match args.next() {
                Some(p) => path = Some(p.into()),
                None => {
                    eprintln!("herobar: --config needs a file\n\n{}", usage());
                    std::process::exit(2);
                }
            },
            "--check" => check = true,
            "--print-default-config" => {
                print!("{}", config::DEFAULT);
                return;
            }
            "--version" | "-V" => {
                println!("herobar {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!("{}", usage());
                return;
            }
            other => {
                eprintln!("herobar: unknown argument '{other}'\n\n{}", usage());
                std::process::exit(2);
            }
        }
    }

    if check {
        let path = path.or_else(config::default_path).expect("no config directory");
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| config::parse(&t)) {
            Ok(_) => println!("{}: ok", path.display()),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    modules::init_time();
    let watch = reload::Watch::new(path.clone().or_else(config::default_path));
    let config = config::load(path);
    let edge = match config.bar.position {
        config::Position::Top => Edge::Top,
        config::Position::Bottom => Edge::Bottom,
    };
    let height = config.bar.height.max(1);
    if config.bar.islands {
        ISLANDS.with(|c| c.set(Some(config.bar.island_style)));
    }
    let st = &config.style;
    SIZES.with(|c| {
        c.set(Sizes {
            padding: st.module_padding.unwrap_or(10).max(0),
            margin: st.module_margin.unwrap_or(3).min(height / 2 - 4).max(0),
            icon: st.icon_size,
        })
    });
    let mut settings = Settings::panel("herobar", edge, height).class("herobar").transparent(config.bar.islands);
    settings.reserve = Some((edge, if config.bar.reserve_space { height } else { 0 }));
    if let Err(e) = heroui::run(Bar::new(config, watch), settings) {
        eprintln!("herobar: {e}");
        std::process::exit(1);
    }
}
