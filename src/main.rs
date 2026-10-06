//! herobar: a lightweight status bar built on HeroUI.
//!
//! A layer-shell panel on Wayland compositors that support it (HeroWM,
//! sway, Hyprland, KDE...), a dock window with a strut on X11.

mod apps;
mod config;
mod edit;
mod fade;
mod fit;
mod glide;
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
use heroui::hover::hover_amount;
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
    /// When each module refreshes next.
    due: Vec<std::time::Instant>,
    /// Modules shrinking away before a rebuild (by name).
    leaving: HashSet<String>,
    /// A changed config waiting for the leaving modules to shrink.
    pending: Option<config::Config>,
    /// Background watchers started with the bar: (desktop, audio, network).
    workers: (bool, bool, bool),
    /// The first heartbeat happened: layout changes animate from now on.
    started: bool,
    /// The taskbar folder whose popup is open.
    folder: Option<taskbar::OpenFolder>,
    /// A taskbar right-click menu that's open.
    menu: Option<TaskMenu>,
    /// A button carried out of the open folder, over the bar.
    carry: Option<taskbar::CarryOut>,
}

/// A taskbar button's right-click menu.
#[derive(Debug, Clone)]
pub struct TaskMenu {
    /// The taskbar module.
    pub module: usize,
    /// Opened from a button of the open folder's popup (Some), or of the
    /// taskbar.
    pub row: Option<usize>,
    pub labels: Vec<String>,
    pub acts: Vec<taskbar::TaskAction>,
    /// Where it opens, relative to the taskbar (or the folder's buttons).
    pub rect: (i32, i32, i32, i32),
}

/// Modules from `config`, reusing `old` ones with the same name and
/// settings (their state carries over: CPU counters, last output).
/// Returns (modules, group of each, left, center, right).
type Built = (Vec<Module>, Vec<Option<usize>>, Vec<usize>, Vec<usize>, Vec<usize>);

fn build_modules(config: &config::Config, mut old: Vec<Module>) -> Built {
    let mut modules = Vec::new();
    let mut parent = Vec::new();
    let mut make = |name: &str| {
        let cfg = config.modules.get(name);
        let same = |m: &Module| m.name == name && m.cfg == cfg.cloned().unwrap_or_default();
        match old.iter().position(same) {
            Some(k) => old.swap_remove(k),
            None => Module::new(name, cfg),
        }
    };
    let mut section = |names: &[String]| {
        let mut idx = Vec::new();
        for name in names {
            modules.push(make(name));
            parent.push(None);
            let g = modules.len() - 1;
            idx.push(g);
            // A group's modules follow it.
            if modules[g].kind == Kind::Group {
                let members = config.modules.get(name).and_then(|c| c.modules.clone()).unwrap_or_default();
                for member in members {
                    modules.push(make(&member));
                    parent.push(Some(g));
                }
            }
        }
        idx
    };
    let left = section(&config.bar.modules_left);
    let center = section(&config.bar.modules_center);
    let right = section(&config.bar.modules_right);
    (modules, parent, left, center, right)
}

/// Which background watchers `modules` need: (desktop, audio, network).
fn workers_for(modules: &[Module]) -> (bool, bool, bool) {
    (
        modules.iter().any(|m| matches!(m.kind, Kind::Taskbar | Kind::Workspaces)),
        modules.iter().any(|m| m.kind == Kind::Volume && m.exec.is_none()) && system::have("pactl"),
        modules.iter().any(|m| m.kind == Kind::Network) && system::have("nmcli"),
    )
}

/// Changes the window itself (or its watchers) can't follow live.
fn needs_restart(old: &config::Config, new: &config::Config, running: (bool, bool, bool), wanted: (bool, bool, bool)) -> bool {
    let (a, b) = (&old.bar, &new.bar);
    let output = |c: &config::Config| c.modules.iter().find(|(n, _)| n.starts_with("workspaces")).and_then(|(_, m)| m.output.clone());
    a.position != b.position
        || a.height != b.height
        || a.reserve_space != b.reserve_space
        || a.islands != b.islands
        || (wanted.0 && !running.0)
        || (wanted.1 && !running.1)
        || (wanted.2 && !running.2)
        || output(old) != output(new)
}

#[derive(Clone)]
enum Msg {
    /// Time to refresh module `i`.
    Tick(usize),
    /// Output of module `i`'s command.
    Output(usize, String),
    /// Module `i` was clicked.
    Click(usize),
    /// The launcher module `i` was clicked; its x in the bar.
    OpenLauncher(usize, i32),
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
    /// Once a second: refresh what's due, check the config.
    Heartbeat,
    /// Apply the pending config (removed modules have shrunk away).
    Rebuild,
    /// Taskbar module `i`'s right-click menu choice (`usize::MAX`: from
    /// the open folder's popup).
    TaskAction(usize, taskbar::TaskAction),
    /// Open a taskbar folder (module, folder, button rectangle).
    OpenFolder(usize, edit::PinPath, (i32, i32, i32, i32)),
    /// A button of the open folder was clicked.
    FolderEntry(taskbar::Item),
    /// A button carried out of the open folder is over the bar (None: back
    /// in the folder).
    CarryOut(Option<taskbar::CarryOut>),
    /// ... and dropped there.
    CarryDrop,
    FolderBack,
    CloseFolder,
    OpenMenu(TaskMenu),
    /// Item `k` of the open menu.
    MenuPick(usize),
    CloseMenu,
}

impl Bar {
    fn new(config: config::Config, watch: reload::Watch) -> Bar {
        let (modules, parent, left, center, right) = build_modules(&config, Vec::new());
        let now = std::time::Instant::now();
        Bar {
            due: vec![now; modules.len()],
            workers: workers_for(&modules),
            config,
            watch,
            modules,
            left,
            center,
            right,
            parent,
            open: HashSet::new(),
            desktop: taskbar::Desktop::default(),
            sys: popups::Sys::default(),
            leaving: HashSet::new(),
            pending: None,
            started: false,
            folder: None,
            menu: None,
            carry: None,
        }
    }

    /// Switches to `config` in place: same window, new modules (state of
    /// unchanged ones kept), sizes animating from the old layout.
    fn apply_config(&mut self, config: config::Config) -> Task<Msg> {
        let old = std::mem::take(&mut self.modules);
        let (modules, parent, left, center, right) = build_modules(&config, old);
        let now = std::time::Instant::now();
        self.due = vec![now; modules.len()];
        (self.modules, self.parent, self.left, self.center, self.right) = (modules, parent, left, center, right);
        self.config = config;
        self.open.clear();
        self.sys.open = None;
        self.folder = None;
        self.leaving.clear();
        set_globals(&self.config);
        heroui::theme::set_current(self.theme());
        // New Bluetooth/command modules show something right away.
        let first: Vec<usize> = (0..self.modules.len())
            .filter(|&i| (self.modules[i].exec.is_some() && self.modules[i].text.is_empty()) || self.modules[i].kind == Kind::Bluetooth)
            .collect();
        let mut tasks: Vec<Task<Msg>> = first.into_iter().map(|i| self.update(Msg::Tick(i))).collect();
        // Shared state reaches the new modules.
        tasks.push(self.update_sys(popups::SysMsg::Refresh));
        tasks.push(Task::rebuild());
        Task::batch(tasks)
    }

    /// True if module `i` is hidden: in a closed drawer, or on its way out.
    fn gone(&self, i: usize) -> bool {
        self.hidden(i) || self.leaving.contains(&self.modules[i].name)
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

    /// The view of module `i` (any kind), named for `fit`.
    fn module_element(&self, i: usize, center: bool) -> Element<Bar, Msg> {
        let el = self.module_element_(i, center);
        let name = self.modules[i].name.clone();
        Element::new(move |ctx| {
            let w = el.build(ctx);
            fit::name(&w, &name);
            w
        })
    }

    fn module_element_(&self, i: usize, center: bool) -> Element<Bar, Msg> {
        let m = &self.modules[i];
        let in_group = self.parent[i].is_some();
        match m.kind {
            Kind::Taskbar => popover_at(
                popover_at(
                    taskbar::view(i),
                    |b: &Bar| b.folder.as_ref().map(|f| f.rect),
                    move |b: &Bar| b.folder.as_ref().is_some_and(|f| f.module == i),
                    Msg::CloseFolder,
                    taskbar::folder_size,
                    taskbar::folder_view(),
                ),
                |b: &Bar| b.menu.as_ref().map(|m| m.rect),
                move |b: &Bar| b.menu.as_ref().is_some_and(|m| m.module == i && m.row.is_none()),
                Msg::CloseMenu,
                menu_size,
                menu_view(),
            ),
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
                    Kind::Clock => (popups::calendar_view(i), popups::calendar_size),
                    Kind::Battery => (popups::battery_view(i), popups::battery_size),
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
            Kind::Launcher if m.command.is_none() => module_view(i, Click::Launcher, in_group),
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
            Msg::OpenLauncher(i, x) => {
                // Running it again closes it (it toggles).
                let cmd = if self.modules[i].cfg.mode.as_deref() == Some("center") {
                    "herolauncher".to_owned()
                } else {
                    let edge = if self.config.bar.position == config::Position::Bottom { "bottom" } else { "top" };
                    format!("herolauncher --menu --edge {edge} --x {x} --offset {}", self.config.bar.height + 4)
                };
                return Self::launch(cmd);
            }
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
            Msg::Heartbeat => {
                if !self.started {
                    self.started = true;
                    fit::animate(true);
                }
                let now = std::time::Instant::now();
                let due: Vec<usize> = (0..self.modules.len())
                    .filter(|&i| self.modules[i].is_dynamic() && self.due[i] <= now)
                    .collect();
                let mut tasks = Vec::new();
                for i in due {
                    let secs = self.modules[i].interval.max(1.0);
                    // A little early, so 1-second clocks don't skip.
                    self.due[i] = now + Duration::from_secs_f64(secs - 0.05);
                    tasks.push(self.update(Msg::Tick(i)));
                }
                tasks.push(self.update(Msg::CheckReload));
                return Task::batch(tasks);
            }
            Msg::CheckReload => {
                if !self.watch.changed() {
                    return Task::none();
                }
                // Don't trade a working bar for a broken config.
                let Some(p) = self.watch.config.clone() else { return Task::none() };
                let new = match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| config::parse(&t)) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("herobar: not reloading, {}: {e}", p.display());
                        return Task::none();
                    }
                };
                let (wanted, _, _, _, _) = build_modules(&new, Vec::new());
                if needs_restart(&self.config, &new, self.workers, workers_for(&wanted)) {
                    let e = reload::restart();
                    eprintln!("herobar: reload failed: {e}");
                    return Task::none();
                }
                // Removed modules shrink away first, then the bar is rebuilt.
                let names: HashSet<String> = wanted.iter().map(|m| m.name.clone()).collect();
                self.leaving = self.modules.iter().map(|m| m.name.clone()).filter(|n| !names.contains(n)).collect();
                if self.leaving.is_empty() || !heroui::anim::enabled() {
                    return self.apply_config(new);
                }
                self.pending = Some(new);
                return Task::perform(|| {
                    std::thread::sleep(Duration::from_millis(330));
                    Msg::Rebuild
                });
            }
            Msg::OpenFolder(i, path, rect) => {
                // A second click on the same folder closes it.
                self.folder = match &self.folder {
                    Some(f) if f.module == i && f.path == path => None,
                    _ => Some(taskbar::OpenFolder { module: i, path, rect }),
                };
            }
            Msg::CloseFolder => {
                self.folder = None;
                self.carry = None;
            }
            Msg::CarryOut(c) => self.carry = c,
            Msg::CarryDrop => {
                self.carry = None;
                if let Some((module, act)) = taskbar::take_out_drop() {
                    return self.update(Msg::TaskAction(module, act));
                }
            }
            Msg::OpenMenu(m) => self.menu = Some(m),
            Msg::CloseMenu => self.menu = None,
            Msg::MenuPick(k) => {
                let Some(m) = self.menu.take() else { return Task::none() };
                let Some(act) = m.acts.get(k).cloned() else { return Task::none() };
                let module = if m.row.is_some() { usize::MAX } else { m.module };
                return self.update(Msg::TaskAction(module, act));
            }
            Msg::FolderBack => {
                if let Some(f) = &mut self.folder {
                    f.path.pop();
                }
            }
            Msg::FolderEntry(item) => {
                if let (Some(f), Some(path)) = (&mut self.folder, item.folder.clone()) {
                    f.path = path;
                    return Task::none();
                }
                self.folder = None;
                return self.update(Msg::Task(item, 1));
            }
            Msg::TaskAction(i, act) => {
                use taskbar::TaskAction as A;
                let module = if i == usize::MAX { self.folder.as_ref().map(|f| f.module) } else { Some(i) };
                let Some(module) = module else { return Task::none() };
                let name = self.modules[module].name.clone();
                let edit = |f: &dyn Fn(&mut Vec<config::Pinned>)| {
                    let path = self.watch.config.clone().or_else(config::default_path);
                    match path {
                        Some(p) => edit::pinned(&p, &name, f).map_err(|e| eprintln!("herobar: {e}")).is_ok(),
                        None => false,
                    }
                };
                let changed = match act {
                    A::Pin(id) => edit(&|l| edit::pin(l, &id)),
                    A::Unpin(p) => edit(&|l| {
                        edit::remove(l, &p);
                    }),
                    A::MoveTo(id, f) => edit(&|l| edit::move_to(l, &id, f.clone())),
                    A::NewFolder(id) => edit(&|l| edit::new_folder(l, &id, "New folder")),
                    A::MoveOut(p) => edit(&|l| edit::move_out(l, &p)),
                    A::Dissolve(p) => edit(&|l| edit::dissolve(l, &p)),
                    A::Close(ids) => {
                        if let Some(c) = &self.desktop.control {
                            for id in ids {
                                c.send(windows::Cmd::Close(id));
                            }
                        }
                        false
                    }
                    A::Launch(cmd) => return Self::launch(cmd),
                    A::Put(from, id, dest, before) => edit(&|l| edit::put(l, from.clone(), &id, &dest, before)),
                    A::Merge(from, id, target, target_id) => edit(&|l| edit::merge(l, from.clone(), &id, target.clone(), &target_id, "New folder")),
                };
                if changed {
                    self.folder = None;
                    // Picked up like any config edit: animated, no restart.
                    return self.update(Msg::CheckReload);
                }
            }
            Msg::Rebuild => {
                if let Some(c) = self.pending.take() {
                    return self.apply_config(c);
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<Self, Msg> {
        // A new view (also after a config change): widths carry over by
        // module name.
        fit::forget_widgets();
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
        // The monitor whose workspaces to show, if one is configured.
        let output = self.modules.iter().find(|m| m.kind == Kind::Workspaces).and_then(|m| m.cfg.output.clone());
        // One timer for everything (it survives config changes); modules
        // refresh on it when due.
        let (desktop, audio, net) = self.workers;
        std::iter::once(Subscription::every(Duration::from_secs(1), Msg::Heartbeat))
            .chain(desktop.then(|| {
                Subscription::worker(move |tx: heroui::Sender<Msg>| windows::run(output, move |u| tx.send(Msg::Windows(u))))
            }))
            // Sound server and NetworkManager events, instead of polling.
            .chain(audio.then(|| {
                Subscription::worker(|tx: heroui::Sender<Msg>| system::audio_watch(move || tx.send(Msg::Sys(popups::SysMsg::AudioChanged))))
            }))
            .chain(net.then(|| {
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

/// Island style and sizes from the config, read while drawing.
fn set_globals(config: &config::Config) {
    ISLANDS.with(|c| c.set(config.bar.islands.then_some(config.bar.island_style)));
    // Popups get the corners of the bar's islands.
    heroui::widgets::set_popover_radius(match (config.bar.islands, config.bar.island_style) {
        (true, config::IslandStyle::Sharp) => Some(0),
        (true, config::IslandStyle::Pill) => Some(16),
        // Islands cap their rounding at 10 px; popups match.
        (true, config::IslandStyle::Rounded) => Some(heroui::theme::current().radius.min(10)),
        _ => None,
    });
    let st = &config.style;
    let height = config.bar.height.max(1);
    SIZES.with(|c| {
        c.set(Sizes {
            padding: st.module_padding.unwrap_or(10).max(0),
            margin: st.module_margin.unwrap_or(3).min(height / 2 - 4).max(0),
            icon: st.icon_size,
        })
    });
}

/// True if modules sit on islands.
pub fn islands_on() -> bool {
    ISLANDS.with(Cell::get).is_some()
}

/// The color of an island (what a hover blends from).
pub fn island_color() -> Color {
    let t = heroui::theme::current();
    if heroui::is_transparent() {
        t.background
    } else {
        t.surface
    }
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
/// custom one can be just an icon. Changing numbers get their slot's
/// widest width (from `reserve`), so the module keeps one width.
fn module_width(icon: &str, text: &str, reserve: &str, custom: bool, sz: ModSizes) -> i32 {
    if text.is_empty() && !custom {
        return 0;
    }
    let t = heroui::theme::current();
    draw::set_font(t.font(), sz.font(&t));
    let slots = slot_widths(text, reserve);
    let text_w = text_width(text, &slots, sz.icon(&t)).max(text_width(&zeros(reserve), &slots, sz.icon(&t)));
    let icon_w = if icon.is_empty() { 0 } else { sz.icon(&t) + if text.is_empty() { 0 } else { ICON_GAP } };
    if text_w + icon_w == 0 {
        0
    } else {
        text_w + icon_w + 2 * sz.padding
    }
}

/// Every digit as "0" (digits are about as wide as each other, but not
/// quite in every font).
fn zeros(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_digit() { '0' } else { c }).collect()
}

/// A piece of module text: words, an inline icon (`{icon:name}` in a
/// format, e.g. arrows before network speeds), or the `k`th changing
/// number (marked by `modules::slot`).
enum Seg<'a> {
    Text(&'a str),
    Icon(&'a str),
    Slot(&'a str, usize),
}

fn segments(text: &str) -> Vec<Seg<'_>> {
    use modules::{SLOT_END, SLOT_START};
    let mut out = Vec::new();
    let mut slot = 0;
    let mut rest = text;
    loop {
        let icon = rest.find("{icon:");
        let mark = rest.find(SLOT_START);
        let (i, is_icon) = match (icon, mark) {
            (Some(a), Some(b)) if a < b => (a, true),
            (_, Some(b)) => (b, false),
            (Some(a), None) => (a, true),
            (None, None) => break,
        };
        let end_mark = if is_icon { '}' } else { SLOT_END };
        let Some(end) = rest[i..].find(end_mark) else { break };
        if i > 0 {
            out.push(Seg::Text(&rest[..i]));
        }
        if is_icon {
            out.push(Seg::Icon(&rest[i + 6..i + end]));
        } else {
            out.push(Seg::Slot(&rest[i + SLOT_START.len_utf8()..i + end], slot));
            slot += 1;
        }
        rest = &rest[i + end + end_mark.len_utf8()..];
    }
    if !rest.is_empty() {
        out.push(Seg::Text(rest));
    }
    out
}

/// Each slot's width: the widest of its value now and in `reserve`, digits
/// as "0" (font set).
fn slot_widths(text: &str, reserve: &str) -> Vec<i32> {
    let mut out: Vec<i32> = Vec::new();
    for src in [text, reserve] {
        for seg in segments(src) {
            if let Seg::Slot(v, k) = seg {
                let w = draw::width(&zeros(v)).ceil() as i32;
                if k < out.len() {
                    out[k] = out[k].max(w);
                } else {
                    out.push(w);
                }
            }
        }
    }
    out
}

/// Gap after an inline icon.
const INLINE_GAP: i32 = 2;

/// Width of module text with inline icons `icon` px wide (font set).
fn text_width(text: &str, slots: &[i32], icon: i32) -> i32 {
    segments(text)
        .iter()
        .map(|s| match s {
            Seg::Text(t) => draw::width(t).ceil() as i32,
            Seg::Icon(_) => icon * 4 / 5 + INLINE_GAP,
            Seg::Slot(v, k) => slots.get(*k).copied().unwrap_or_else(|| draw::width(v).ceil() as i32),
        })
        .sum()
}

/// Paints a module: island (unless in a group), hover, icon, text.
fn paint_module(w: &dyn WidgetExt, icon: &str, text: &str, reserve: &str, hovered: f32, in_group: bool, sz: ModSizes) {
    // While it grows or shrinks, nothing spills onto its neighbors.
    draw::push_clip(w.x(), w.y(), w.w(), w.h());
    paint_module_(w, icon, text, reserve, hovered, in_group, sz);
    draw::pop_clip();
}

fn paint_module_(w: &dyn WidgetExt, icon: &str, text: &str, reserve: &str, hovered: f32, in_group: bool, sz: ModSizes) {
    let t = heroui::theme::current();
    if !in_group {
        island(w.x(), w.y(), w.w(), w.h());
    }
    // Pressing squeezes the highlight in a little (HeroUI's press_amount).
    let press = heroui::hover::press_amount(w);
    let hovered = hovered.max(press);
    if hovered > 0.0 {
        // Fades in and out (HeroUI's hover_amount): blend from what's under.
        let under = if ISLANDS.with(Cell::get).is_some() {
            if heroui::is_transparent() { t.background } else { t.surface }
        } else {
            t.background
        };
        let m = margin();
        let h = w.h() - 2 * m;
        let r = if ISLANDS.with(Cell::get).is_some() { island_radius(h) } else { t.radius.min(h / 2) };
        let i = 1.5 * press as f64;
        let color = heroui::widgets::mix(under, t.surface_alt, hovered);
        heroui::fx::fill_rounded(w.x() as f64 + i, (w.y() + m) as f64 + i, w.w() as f64 - 2.0 * i, h as f64 - 2.0 * i, r as f64 - i, color, 1.0);
    }
    let mut x = w.x() + sz.padding;
    if !icon.is_empty() {
        let s = sz.icon(&t);
        heroui::icons::draw(icon, x, w.y() + (w.h() - s) / 2, s, t.text);
        x += s + ICON_GAP;
    }
    draw::set_font(t.font(), sz.font(&t));
    // Inline icons are a bit smaller than the module's icon.
    let small = sz.icon(&t) * 4 / 5;
    let slots = slot_widths(text, reserve);
    for seg in segments(text) {
        match seg {
            Seg::Text(s) => {
                draw::set_draw_color(t.text);
                let tw = draw::width(s).ceil() as i32;
                draw::draw_text2(s, x, w.y(), tw + 2, w.h(), Align::Left | Align::Inside);
                x += tw;
            }
            Seg::Icon(name) => {
                heroui::icons::draw(name, x, w.y() + (w.h() - small) / 2, small, t.text);
                x += small + INLINE_GAP;
            }
            Seg::Slot(v, k) => {
                // Right-aligned in its slot: what follows stays put.
                draw::set_draw_color(t.text);
                let sw = slots.get(k).copied().unwrap_or(0);
                let tw = draw::width(v).ceil() as i32;
                draw::draw_text2(v, x + sw - tw, w.y(), tw + 2, w.h(), Align::Left | Align::Inside);
                x += sw;
            }
        }
    }
}

/// The open taskbar menu: a list of its items.
fn menu_view() -> Element<Bar, Msg> {
    column(vec![list(
        |b: &Bar| b.menu.as_ref().map_or(0, |m| m.labels.len()),
        |k| {
            Element::new(move |ctx| {
                let label = Rc::new(RefCell::new(String::new()));
                let mut b = custom_button({
                    let label = label.clone();
                    move |b| {
                        let t = heroui::theme::current();
                        let l = label.borrow();
                        let (line, text) = match l.strip_prefix('-') {
                            Some(t) => (true, t),
                            None => (false, l.as_str()),
                        };
                        let top = if line { 7 } else { 0 };
                        if line {
                            draw::set_draw_color(t.border);
                            draw::draw_line(b.x() + 6, b.y() + 3, b.x() + b.w() - 6, b.y() + 3);
                        }
                        let a = if b.value() { 1.0 } else { hover_amount(b) };
                        if a > 0.0 {
                            draw::set_draw_color(heroui::widgets::mix(t.background, t.surface_alt, a));
                            draw::draw_rounded_rectf(b.x(), b.y() + top, b.w(), b.h() - top, t.radius.min(8));
                        }
                        draw::set_draw_color(t.text);
                        draw::set_font(t.font(), t.font_size);
                        draw::draw_text2(text, b.x() + 10, b.y() + top, b.w() - 20, b.h() - top, Align::Left | Align::Inside);
                    }
                });
                let emit = ctx.emitter();
                b.set_callback(move |_| emit(Msg::MenuPick(k)));
                let mut w = b.clone();
                ctx.bind(move |bar: &Bar| {
                    let l = bar.menu.as_ref().and_then(|m| m.labels.get(k).cloned()).unwrap_or_default();
                    if *label.borrow() != l {
                        *label.borrow_mut() = l;
                        repaint(&mut w);
                    }
                });
                b.as_base_widget()
            })
            .fixed_with(move |b: &Bar| {
                let line = b.menu.as_ref().and_then(|m| m.labels.get(k)).is_some_and(|l| l.starts_with('-'));
                MENU_ROW + if line { 7 } else { 0 }
            })
        },
    )])
    .padding(6)
    .spacing(0)
}

const MENU_ROW: i32 = 30;

fn menu_size(b: &Bar) -> (i32, i32) {
    let Some(m) = &b.menu else { return (200, 40) };
    let t = heroui::theme::current();
    draw::set_font(t.font(), t.font_size);
    let w = m.labels.iter().map(|l| draw::width(l.trim_start_matches('-')).ceil() as i32).max().unwrap_or(100) + 32;
    let lines = m.labels.iter().filter(|l| l.starts_with('-')).count() as i32;
    let gap = t.spacing;
    let n = m.labels.len() as i32;
    (w.max(160), 12 + n * MENU_ROW + (n - 1).max(0) * gap + lines * 7)
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
    /// Opens HeroLauncher at the module (on release, like a button).
    Launcher,
}

/// A module: its icon and text, sized to fit; clickable if it has an
/// on-click action or a popup.
fn module_view(i: usize, click: Click, in_group: bool) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        // (icon, text, reserve) shown.
        let shown: Rc<RefCell<(String, String, String)>> = Rc::default();
        let sizes: Rc<Cell<ModSizes>> = Rc::new(Cell::new(ModSizes { padding: 10, icon: None, font: None }));
        let paint = {
            let shown = shown.clone();
            let sizes = sizes.clone();
            move |w: &mut dyn WidgetExt, hovered: f32| {
                let (icon, text, reserve) = &*shown.borrow();
                paint_module(w, icon, text, reserve, hovered, in_group, sizes.get());
            }
        };
        // Clickable modules are buttons (FLTK handles the clicks, HeroUI the
        // hover); the others are plain frames.
        let widget = if click == Click::Popup {
            let mut b = press_button(move |b| {
                let hovered = hover_amount(b);
                paint(b, hovered)
            });
            let emit = ctx.emitter();
            b.set_callback(move |b| {
                if b.value() {
                    emit(Msg::Sys(popups::SysMsg::Open(i)))
                }
            });
            b.as_base_widget()
        } else if click == Click::Command || click == Click::Launcher {
            let mut b = custom_button(move |b| {
                let hovered = hover_amount(b);
                paint(b, hovered)
            });
            let emit = ctx.emitter();
            b.set_callback(move |b| emit(if click == Click::Launcher { Msg::OpenLauncher(i, b.x()) } else { Msg::Click(i) }));
            b.as_base_widget()
        } else {
            let mut f = Frame::default();
            f.set_frame(FrameType::NoBox);
            f.draw(move |f| paint(f, 0.0));
            f.as_base_widget()
        };
        let mut w = widget.clone();
        let last_width = Cell::new(-1);
        let was_hidden = Cell::new(false);
        ctx.bind(move |bar: &Bar| {
            let m = &bar.modules[i];
            let hidden = bar.gone(i);
            // Details on hover (XFCE-style); FLTK shows them.
            if m.cfg.tooltip != Some(false) && w.tooltip().unwrap_or_default() != m.tooltip {
                w.set_tooltip(&m.tooltip);
            }
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
            *shown.borrow_mut() = (m.icon.clone(), m.text.clone(), m.reserve.clone());

            // Custom, Bluetooth and network modules may be just an icon.
            let icon_only = matches!(m.kind, Kind::Custom | Kind::Bluetooth | Kind::Network | Kind::Launcher);
            // Changing numbers (CPU %, network speeds) would make the
            // module and its neighbors jitter: it's sized for its numbers
            // at their widest.
            let width = if hidden || m.absent { 0 } else { module_width(&m.icon, &m.text, &m.reserve, icon_only, sizes.get()) };
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
                let hovered = if open.get() { 1.0 } else { hover_amount(b) };
                let sz = ModSizes { padding: sizes.padding, icon: sizes.icon, font: None };
                paint_module(b, &icon, "", "", hovered, true, sz);
            }
        });
        let emit = ctx.emitter();
        b.set_callback(move |_| emit(Msg::Drawer(g)));
        let mut w = b.as_base_widget();
        let first = Cell::new(true);
        ctx.bind(move |bar: &Bar| {
            let is_open = bar.open.contains(&g);
            if bar.gone(g) {
                fit::set_width(&mut w, 0);
                return;
            }
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
    set_globals(&config);
    let mut settings = Settings::panel("herobar", edge, height).class("herobar").transparent(config.bar.islands);
    settings.reserve = Some((edge, if config.bar.reserve_space { height } else { 0 }));
    if let Err(e) = heroui::run(Bar::new(config, watch), settings) {
        eprintln!("herobar: {e}");
        std::process::exit(1);
    }
}
