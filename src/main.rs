//! herobar: a lightweight status bar built on HeroUI.
//!
//! A layer-shell panel on Wayland compositors that support it (HeroWM,
//! sway, Hyprland, KDE...), a dock window with a strut on X11.

mod apps;
mod config;
mod modules;
mod reload;
mod taskbar;
mod windows;

use std::cell::{Cell, RefCell};
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

use modules::Module;

struct Bar {
    config: config::Config,
    watch: reload::Watch,
    modules: Vec<Module>,
    /// Module indexes per section.
    left: Vec<usize>,
    center: Vec<usize>,
    right: Vec<usize>,
    /// Open windows, for taskbar modules.
    taskbar: taskbar::Taskbar,
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
    /// News from the compositor about windows.
    Windows(windows::Update),
    /// A taskbar button was clicked (with mouse button 1-3).
    Task(taskbar::Item, i32),
}

impl Bar {
    fn new(config: config::Config, watch: reload::Watch) -> Bar {
        let mut modules = Vec::new();
        let mut section = |names: &[String]| {
            names
                .iter()
                .map(|name| {
                    modules.push(Module::new(name, config.modules.get(name)));
                    modules.len() - 1
                })
                .collect::<Vec<_>>()
        };
        let left = section(&config.bar.modules_left);
        let center = section(&config.bar.modules_center);
        let right = section(&config.bar.modules_right);
        Bar { config, watch, modules, left, center, right, taskbar: taskbar::Taskbar::default() }
    }
}

impl App for Bar {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
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
                    return Task::perform(move || {
                        modules::launch(&cmd);
                        Msg::Launched
                    });
                }
            }
            Msg::Launched => {}
            Msg::Windows(windows::Update::Ready(c)) => self.taskbar.control = Some(c),
            Msg::Windows(windows::Update::Windows(w)) => self.taskbar.windows = w,
            Msg::Task(item, button) => {
                if let Some(cmd) = taskbar::click(&item, button, &self.taskbar.windows, self.taskbar.control.as_ref()) {
                    return Task::perform(move || {
                        modules::launch(&cmd);
                        Msg::Launched
                    });
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
        let widths = Sections::default();
        let section = |which: Section, idx: &[usize]| {
            let mut items: Vec<Element<Bar, Msg>> =
                idx.iter()
                .map(|&i| match self.modules[i].kind {
                    modules::Kind::Taskbar => taskbar::view(i, which, widths.clone()),
                    _ => module_view(i, self.modules[i].command.is_some(), which, widths.clone()),
                })
                .collect();
            // Left items pack to the left, right items to the right.
            match which {
                Section::Left => items.push(spacer()),
                Section::Right => items.insert(0, spacer()),
                Section::Center => {}
            }
            row(items).spacing(self.config.bar.spacing)
        };
        let pad = self.config.bar.padding;
        with_margins(
            row(vec![
                section(Section::Left, &self.left),
                section(Section::Center, &self.center).fixed(0),
                section(Section::Right, &self.right),
            ])
            .spacing(0),
            (pad, 0, pad, 0),
        )
    }

    fn init(&mut self) -> Task<Msg> {
        // Custom commands produce their first output right away.
        let custom: Vec<usize> = (0..self.modules.len()).filter(|&i| self.modules[i].exec.is_some()).collect();
        Task::batch(custom.into_iter().map(|i| self.update(Msg::Tick(i))))
    }

    fn subscriptions(&self) -> Vec<Subscription<Msg>> {
        self.modules
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_dynamic())
            .map(|(i, m)| Subscription::every(Duration::from_secs_f64(m.interval), Msg::Tick(i)))
            .chain([Subscription::every(Duration::from_secs(1), Msg::CheckReload)])
            .chain(self.modules.iter().any(|m| m.kind == modules::Kind::Taskbar).then(|| {
                Subscription::worker(|tx: heroui::Sender<Msg>| windows::run(move |u| tx.send(Msg::Windows(u))))
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

/// Measured widths of the center section's modules, to keep the center
/// section exactly as wide as its content (and so truly centered).
#[derive(Clone, Default)]
pub struct Sections {
    center: Rc<RefCell<Vec<(usize, i32)>>>,
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

/// Horizontal padding inside a module.
const PAD: i32 = 10;
/// Gap between a module's icon and its text.
const ICON_GAP: i32 = 6;

thread_local! {
    /// The island style, when modules sit on islands.
    static ISLANDS: Cell<Option<config::IslandStyle>> = const { Cell::new(None) };
}

/// Paints a module's island (when islands are on): its own background,
/// with the bar's see-through gaps around it.
pub fn island(x: i32, y: i32, w: i32, h: i32) {
    let Some(style) = ISLANDS.with(Cell::get) else { return };
    let t = heroui::theme::current();
    // On a see-through bar the island is the bar color; on an opaque one
    // (X11, no fork) it has to stand out from it.
    draw::set_draw_color(if heroui::is_transparent() { t.background } else { t.surface });
    let (y, h) = (y + 3, h - 6);
    let r = match style {
        config::IslandStyle::Sharp => 0,
        config::IslandStyle::Rounded => t.radius.min(h / 2).min(10),
        config::IslandStyle::Pill => h / 2,
    };
    if r == 0 {
        draw::draw_rectf(x, y, w, h);
    } else {
        draw::draw_rounded_rectf(x, y, w, h, r);
    }
}

fn icon_size(t: &Theme) -> i32 {
    t.font_size + 2
}

/// A module's width for its icon and text (0 hides it). Built-in modules
/// with nothing to report (no battery, no audio) hide, icon and all; a
/// custom one can be just an icon.
fn module_width(icon: &str, text: &str, custom: bool) -> i32 {
    if text.is_empty() && !custom {
        return 0;
    }
    let t = heroui::theme::current();
    draw::set_font(t.font(), t.font_size);
    let text_w = if text.is_empty() { 0 } else { draw::width(text).ceil() as i32 };
    let icon_w = if icon.is_empty() { 0 } else { icon_size(&t) + if text.is_empty() { 0 } else { ICON_GAP } };
    if text_w + icon_w == 0 {
        0
    } else {
        text_w + icon_w + 2 * PAD
    }
}

/// A module: its icon and text, sized to fit; clickable if it has an
/// on-click action.
fn module_view(i: usize, clickable: bool, section: Section, widths: Sections) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        // (icon, text) shown.
        let shown: Rc<RefCell<(String, String)>> = Rc::default();
        let paint = {
            let shown = shown.clone();
            move |w: &mut dyn WidgetExt, hovered: bool| {
                // The theme in use now: it changes live (Appearance).
                let t = heroui::theme::current();
                island(w.x(), w.y(), w.w(), w.h());
                if hovered {
                    draw::set_draw_color(t.surface_alt);
                    let h = w.h() - 6;
                    let r = match ISLANDS.with(Cell::get) {
                        Some(config::IslandStyle::Sharp) => 0,
                        Some(config::IslandStyle::Pill) => h / 2,
                        _ => t.radius.min(h / 2),
                    };
                    draw::draw_rounded_rectf(w.x(), w.y() + 3, w.w(), h, r);
                }
                let (icon, text) = &*shown.borrow();
                let mut x = w.x() + PAD;
                if !icon.is_empty() {
                    let s = icon_size(&t);
                    heroui::icons::draw(icon, x, w.y() + (w.h() - s) / 2, s, t.text);
                    x += s + ICON_GAP;
                }
                draw::set_draw_color(t.text);
                draw::set_font(t.font(), t.font_size);
                draw::draw_text2(text, x, w.y(), w.x() + w.w() - x, w.h(), Align::Left | Align::Inside);
            }
        };
        // Clickable modules are buttons (FLTK handles the clicks, HeroUI the
        // hover); the others are plain frames.
        let widget = if clickable {
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
        ctx.bind(move |bar: &Bar| {
            let m = &bar.modules[i];
            // The first run always sizes the module (last_width starts at
            // -1), so one with nothing to show takes no space instead of a
            // share of the bar.
            {
                let cur = shown.borrow();
                if cur.0 == m.icon && cur.1 == m.text && last_width.get() >= 0 {
                    return;
                }
            }
            *shown.borrow_mut() = (m.icon.clone(), m.text.clone());
            let width = module_width(&m.icon, &m.text, m.kind == modules::Kind::Custom);
            if width != last_width.replace(width) {
                resize_module(&mut w, width, section, i, &widths);
            }
            repaint(&mut w);
        });
        widget
    })
}

/// Gives a module its new width in its section, and keeps the center
/// section as wide as its content.
pub fn resize_module(w: &mut heroui::fltk::widget::Widget, width: i32, section: Section, i: usize, widths: &Sections) {
    if width == 0 {
        w.hide();
    } else {
        w.show();
    }
    let Some(parent) = w.parent() else { return };
    let Some(mut flex) = Flex::from_dyn_widget(&parent) else { return };
    flex.fixed(&*w, width);
    flex.recalc();
    if section == Section::Center {
        let mut c = widths.center.borrow_mut();
        match c.iter_mut().find(|(j, _)| *j == i) {
            Some(e) => e.1 = width,
            None => c.push((i, width)),
        }
        let shown: Vec<i32> = c.iter().map(|&(_, w)| w).filter(|&w| w > 0).collect();
        let total = shown.iter().sum::<i32>() + flex.pad() * (shown.len() as i32 - 1).max(0);
        if let Some(outer) = flex.parent().and_then(|p| Flex::from_dyn_widget(&p)) {
            let mut outer = outer;
            outer.fixed(&flex, total);
            outer.recalc();
        }
    }
    heroui::relayout_parent(&flex);
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
    let mut settings = Settings::panel("herobar", edge, height).class("herobar").transparent(config.bar.islands);
    settings.reserve = Some((edge, if config.bar.reserve_space { height } else { 0 }));
    if let Err(e) = heroui::run(Bar::new(config, watch), settings) {
        eprintln!("herobar: {e}");
        std::process::exit(1);
    }
}
