//! hero-bar: a lightweight status bar built on HeroUI.
//!
//! A layer-shell panel on Wayland compositors that support it (HeroWM,
//! sway, Hyprland, KDE...), a dock window with a strut on X11.

mod config;
mod modules;

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
    modules: Vec<Module>,
    /// Module indexes per section.
    left: Vec<usize>,
    center: Vec<usize>,
    right: Vec<usize>,
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
}

impl Bar {
    fn new(config: config::Config) -> Bar {
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
        Bar { config, modules, left, center, right }
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
            Msg::Output(i, text) => self.modules[i].text = text,
            Msg::Click(i) => {
                if let Some(cmd) = self.modules[i].command.clone() {
                    return Task::perform(move || {
                        modules::launch(&cmd);
                        Msg::Launched
                    });
                }
            }
            Msg::Launched => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<Self, Msg> {
        let widths = Sections::default();
        let section = |which: Section, idx: &[usize]| {
            let mut items: Vec<Element<Bar, Msg>> =
                idx.iter().map(|&i| module_view(i, self.modules[i].command.is_some(), which, widths.clone())).collect();
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
enum Section {
    Left,
    Center,
    Right,
}

/// Measured widths of the center section's modules, to keep the center
/// section exactly as wide as its content (and so truly centered).
#[derive(Clone, Default)]
struct Sections {
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

/// A module: its text, sized to fit; clickable if it has an on-click action.
fn module_view(i: usize, clickable: bool, section: Section, widths: Sections) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let t = ctx.theme_rc();
        let text: Rc<RefCell<String>> = Rc::default();
        let paint = {
            let (t, text) = (t.clone(), text.clone());
            move |w: &mut dyn WidgetExt, hovered: bool| {
                if hovered {
                    draw::set_draw_color(t.surface_alt);
                    let h = w.h() - 6;
                    draw::draw_rounded_rectf(w.x(), w.y() + 3, w.w(), h, t.radius.min(h / 2));
                }
                draw::set_draw_color(t.text);
                draw::set_font(t.font(), t.font_size);
                draw::draw_text2(&text.borrow(), w.x(), w.y(), w.w(), w.h(), Align::Center);
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
        let font = (t.font(), t.font_size);
        let last_width = Cell::new(-1);
        ctx.bind(move |bar: &Bar| {
            let new = &bar.modules[i].text;
            if *text.borrow() == *new {
                return;
            }
            text.borrow_mut().clone_from(new);
            draw::set_font(font.0, font.1);
            let width = if new.is_empty() { 0 } else { draw::width(new).ceil() as i32 + 2 * PAD };
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
fn resize_module(w: &mut heroui::fltk::widget::Widget, width: i32, section: Section, i: usize, widths: &Sections) {
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
    "Usage: hero-bar [--config FILE] [--check] [--print-default-config]

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
                    eprintln!("hero-bar: --config needs a file\n\n{}", usage());
                    std::process::exit(2);
                }
            },
            "--check" => check = true,
            "--print-default-config" => {
                print!("{}", config::DEFAULT);
                return;
            }
            "--version" | "-V" => {
                println!("hero-bar {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!("{}", usage());
                return;
            }
            other => {
                eprintln!("hero-bar: unknown argument '{other}'\n\n{}", usage());
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
    let config = config::load(path);
    let edge = match config.bar.position {
        config::Position::Top => Edge::Top,
        config::Position::Bottom => Edge::Bottom,
    };
    let height = config.bar.height.max(1);
    let mut settings = Settings::panel("hero-bar", edge, height).class("hero-bar");
    settings.reserve = Some((edge, if config.bar.reserve_space { height } else { 0 }));
    if let Err(e) = heroui::run(Bar::new(config), settings) {
        eprintln!("hero-bar: {e}");
        std::process::exit(1);
    }
}
