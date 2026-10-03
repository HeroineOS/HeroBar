//! The taskbar module: pinned apps and open windows.
//!
//! Styles: "icons" (one button per app, dots for its windows, like KDE's
//! icons-only task manager) and "icons-titles" (one button per window with
//! its title, like XFCE's). It takes the room its buttons need up to
//! `max-width` (or always that, with `fixed-width`); past that, buttons
//! shrink and titles are cut short.
//!
//! One widget with one event handler for all buttons; the window list
//! comes from `windows.rs` and only changes when the compositor says so.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use heroui::fltk::app;
use heroui::fltk::draw;
use heroui::fltk::app::MouseButton;
use heroui::fltk::enums::{Align, Event, FrameType};
use heroui::fltk::frame::Frame;
use heroui::fltk::prelude::*;
use heroui::prelude::*;
use heroui::widgets::{mix, repaint};

use crate::apps::App;
use crate::config;
use crate::windows::{Cmd, Control, Win, Ws};
use crate::{Bar, Msg};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TaskShow {
    Running,
    Pinned,
    #[default]
    Both,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TaskStyle {
    #[default]
    Icons,
    IconsTitles,
}

pub struct Config {
    pub show: TaskShow,
    pub style: TaskStyle,
    pub pinned: Vec<App>,
    pub max_width: i32,
    pub fixed_width: bool,
    pub button_width: i32,
    /// Only windows on the current workspace.
    pub current_workspace: bool,
}

impl Config {
    pub fn new(cfg: Option<&config::Module>) -> Config {
        let empty = config::Module::default();
        let c = cfg.unwrap_or(&empty);
        Config {
            show: match c.show.as_deref() {
                Some("running") => TaskShow::Running,
                Some("pinned") => TaskShow::Pinned,
                _ => TaskShow::Both,
            },
            style: match c.style.as_deref() {
                Some("icons-titles") => TaskStyle::IconsTitles,
                _ => TaskStyle::Icons,
            },
            current_workspace: c.workspace.as_deref() == Some("current"),
            // Pinned apps that aren't installed are left out.
            pinned: c.pinned.iter().flatten().filter_map(|id| crate::apps::by_id(id)).collect(),
            max_width: c.max_width.unwrap_or(600).max(40),
            fixed_width: c.fixed_width.unwrap_or(false),
            button_width: c.button_width.unwrap_or(180).max(40),
        }
    }
}

/// What the compositor tells about windows and workspaces.
#[derive(Default)]
pub struct Desktop {
    pub windows: Vec<Win>,
    pub workspaces: Vec<Ws>,
    pub control: Option<Control>,
}

impl Desktop {
    /// The workspace shown on the bar's monitor, if known.
    pub fn current_workspace(&self) -> Option<u64> {
        self.workspaces.iter().find(|w| w.active).map(|w| w.id)
    }
}

/// One button.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub icon: String,
    /// Shown in icons-titles style ("" = icon only).
    pub label: String,
    /// Window ids (one in icons-titles style).
    pub windows: Vec<u64>,
    pub focused: bool,
    /// What starts the app, for pinned apps.
    pub exec: Option<String>,
}

fn key(w: &Win) -> &str {
    w.app.as_ref().map(|a| a.id.as_str()).unwrap_or(&w.app_id)
}

fn icon_of(w: &Win) -> String {
    match w.app.as_ref().map(|a| a.icon.as_str()).filter(|i| !i.is_empty()) {
        Some(i) => i.to_owned(),
        // Many apps' icon is named like their app id.
        None if !w.app_id.is_empty() => w.app_id.to_lowercase(),
        None => "app".into(),
    }
}

/// The buttons, in order: pinned apps first, then other running apps by
/// when their first window opened.
pub fn items(cfg: &Config, windows: &[Win], current: Option<u64>) -> Vec<Item> {
    // Windows on other workspaces are left out (pinned apps stay); when
    // the compositor doesn't tell workspaces, all are shown.
    let filtered: Vec<Win>;
    let windows = match current {
        Some(ws) if cfg.current_workspace => {
            filtered = windows.iter().filter(|w| w.workspace.is_none_or(|x| x == ws)).cloned().collect();
            &filtered[..]
        }
        _ => windows,
    };
    let mut out: Vec<Item> = Vec::new();
    let pinned = cfg.show != TaskShow::Running;
    let running = cfg.show != TaskShow::Pinned;
    match cfg.style {
        TaskStyle::Icons => {
            let mut keys: Vec<String> = Vec::new();
            if pinned {
                keys.extend(cfg.pinned.iter().map(|a| a.id.clone()));
            }
            if running {
                for w in windows {
                    if !keys.iter().any(|k| k == key(w)) {
                        keys.push(key(w).to_owned());
                    }
                }
            }
            for k in keys {
                let wins: Vec<&Win> = windows.iter().filter(|w| key(w) == k).collect();
                let app = cfg.pinned.iter().find(|a| a.id == k);
                if app.is_none() && wins.is_empty() {
                    continue;
                }
                out.push(Item {
                    icon: app.map(|a| a.icon.clone()).filter(|i| !i.is_empty()).unwrap_or_else(|| icon_of(wins[0])),
                    label: String::new(),
                    windows: wins.iter().map(|w| w.id).collect(),
                    focused: wins.iter().any(|w| w.focused),
                    exec: app.map(|a| a.exec.clone()).or_else(|| wins.first().and_then(|w| w.app.as_ref()).map(|a| a.exec.clone())),
                });
            }
        }
        TaskStyle::IconsTitles => {
            if pinned {
                // Launchers for pinned apps that aren't open.
                for a in &cfg.pinned {
                    if !windows.iter().any(|w| key(w) == a.id) {
                        out.push(Item { icon: a.icon.clone(), label: String::new(), windows: vec![], focused: false, exec: Some(a.exec.clone()) });
                    }
                }
            }
            for w in windows {
                let is_pinned = cfg.pinned.iter().any(|a| a.id == key(w));
                if !running && !is_pinned {
                    continue;
                }
                let label = if w.title.is_empty() { w.app.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| w.app_id.clone()) } else { w.title.clone() };
                out.push(Item {
                    icon: icon_of(w),
                    label,
                    windows: vec![w.id],
                    focused: w.focused,
                    exec: w.app.as_ref().map(|a| a.exec.clone()),
                });
            }
        }
    }
    out
}

/// What a click on `item` does: launch, focus, cycle or close.
pub fn click(item: &Item, button: i32, windows: &[Win], control: Option<&Control>) -> Option<String> {
    match (button, item.windows.as_slice()) {
        // Middle click closes the window (the focused one of a group).
        (2, ws) if !ws.is_empty() => {
            let id = ws.iter().copied().find(|id| windows.iter().any(|w| w.id == *id && w.focused)).unwrap_or(ws[0]);
            control?.send(Cmd::Close(id));
            None
        }
        (_, []) => item.exec.clone(),
        (_, ws) => {
            // A group cycles through its windows, starting after the focused one.
            let cur = ws.iter().position(|id| windows.iter().any(|w| w.id == *id && w.focused));
            let next = cur.map_or(0, |i| (i + 1) % ws.len());
            control?.send(Cmd::Focus(ws[next]));
            None
        }
    }
}

const GAP: i32 = 4;
const BTN_PAD: i32 = 8;

fn icon_px() -> i32 {
    heroui::theme::current().font_size + 4
}

/// Button widths for `items` in at most `max` px.
fn widths(items: &[Item], style: TaskStyle, icon: i32, button_width: i32, max: i32) -> Vec<i32> {
    let small = icon + 2 * BTN_PAD;
    let n_titled = items.iter().filter(|i| !i.label.is_empty()).count() as i32;
    let gaps = GAP * (items.len() as i32 - 1).max(0);
    let fixed = items.iter().filter(|i| i.label.is_empty()).count() as i32 * small;
    let titled = if style == TaskStyle::IconsTitles && n_titled > 0 {
        ((max - fixed - gaps) / n_titled).clamp(small, button_width)
    } else {
        small
    };
    items.iter().map(|i| if i.label.is_empty() { small } else { titled }).collect()
}

/// `text` cut to fit `w` px, with an ellipsis.
fn fit(text: &str, w: i32) -> String {
    if draw::width(text) as i32 <= w {
        return text.to_owned();
    }
    let mut s: String = text.to_owned();
    while !s.is_empty() {
        s.pop();
        let t = format!("{}…", s.trim_end());
        if draw::width(&t) as i32 <= w {
            return t;
        }
    }
    String::new()
}

struct View {
    items: Vec<Item>,
    style: TaskStyle,
    button_width: i32,
    hover: Option<usize>,
    pressed: Option<(usize, i32)>,
}

impl View {
    /// Button widths in `w` px (the width the taskbar actually got).
    fn layout(&self, w: i32) -> Vec<i32> {
        widths(&self.items, self.style, icon_px(), self.button_width, w)
    }

    fn at(&self, x0: i32, w: i32, px: i32) -> Option<usize> {
        let mut x = x0;
        for (i, w) in self.layout(w).iter().enumerate() {
            if px >= x && px < x + w {
                return Some(i);
            }
            x += w + GAP;
        }
        None
    }
}

pub fn view(i: usize) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let v = Rc::new(RefCell::new(View { items: vec![], style: TaskStyle::Icons, button_width: 180, hover: None, pressed: None }));
        let mut f = Frame::default();
        f.set_frame(FrameType::NoBox);
        {
            let v = v.clone();
            f.draw(move |f| paint(&v.borrow(), f.x(), f.y(), f.w(), f.h()));
        }
        let emit = ctx.emitter();
        {
            let v = v.clone();
            f.handle(move |f, ev| {
                let px = app::event_x();
                let mut s = v.borrow_mut();
                match ev {
                    Event::Enter | Event::Move => {
                        let h = s.at(f.x(), f.w(), px);
                        if s.hover != h {
                            s.hover = h;
                            repaint(f);
                        }
                        true
                    }
                    Event::Leave => {
                        if s.hover.take().is_some() {
                            repaint(f);
                        }
                        true
                    }
                    Event::Push => {
                        let button = match app::event_mouse_button() {
                            MouseButton::Middle => 2,
                            MouseButton::Right => 3,
                            _ => 1,
                        };
                        s.pressed = s.at(f.x(), f.w(), px).map(|i| (i, button));
                        true
                    }
                    Event::Released => {
                        if let Some((idx, button)) = s.pressed.take() {
                            if s.at(f.x(), f.w(), px) == Some(idx) {
                                if let Some(item) = s.items.get(idx) {
                                    emit(Msg::Task(item.clone(), button));
                                }
                            }
                        }
                        true
                    }
                    _ => false,
                }
            });
        }
        let mut w = f.as_base_widget();
        let last_width = Cell::new(-1);
        let last_room = Cell::new(None);
        ctx.bind(move |bar: &Bar| {
            let Some(cfg) = bar.modules[i].taskbar.as_ref() else { return };
            let items = items(cfg, &bar.desktop.windows, bar.desktop.current_workspace());
            // Runs after every update; only the window list or the room
            // the bar leaves (screen size) make it do anything.
            let room = crate::fit::room(&w);
            let mut s = v.borrow_mut();
            if s.items == items && last_width.get() >= 0 && last_room.get() == room {
                return;
            }
            last_room.set(room);
            let ws = widths(&items, cfg.style, icon_px(), cfg.button_width, cfg.max_width);
            let natural = ws.iter().sum::<i32>() + GAP * (ws.len() as i32 - 1).max(0);
            let want = if cfg.fixed_width { cfg.max_width } else { natural.min(cfg.max_width) };
            // Never into the next section (the clock): buttons shrink instead.
            let width = room.map_or(want, |r| want.min(r));
            s.items = items;
            s.style = cfg.style;
            s.button_width = cfg.button_width;
            s.hover = None;
            drop(s);
            if width != last_width.replace(width) {
                crate::fit::set_width(&mut w, width);
            }
            repaint(&mut w);
        });
        f.as_base_widget()
    })
}

fn paint(v: &View, x: i32, y: i32, w: i32, h: i32) {
    let t = heroui::theme::current();
    crate::island(x, y, w, h);
    let icon = icon_px();
    let bh = h - 8;
    let by = y + 4;
    draw::push_clip(x, y, w, h);
    let mut bx = x;
    draw::set_font(t.font(), t.font_size - 1);
    for (idx, (item, &bw)) in v.items.iter().zip(&v.layout(w)).enumerate() {
        let running = !item.windows.is_empty();
        let bg = if item.focused {
            Some(mix(t.surface_alt, t.accent, 0.18))
        } else if v.hover == Some(idx) {
            Some(t.surface_alt)
        } else {
            None
        };
        if let Some(bg) = bg {
            draw::set_draw_color(bg);
            draw::draw_rounded_rectf(bx, by, bw, bh, t.radius.min(bh / 2).min(8));
        }
        let iy = by + (bh - icon) / 2 - if running { 1 } else { 0 };
        let titled = !item.label.is_empty();
        let ix = if titled { bx + BTN_PAD } else { bx + (bw - icon) / 2 };
        if !heroui::icons::draw(&item.icon, ix, iy, icon, t.text) {
            heroui::icons::draw("app", ix, iy, icon, t.text);
        }
        if titled {
            let tx = ix + icon + 6;
            let tw = bx + bw - BTN_PAD - tx;
            if tw > 8 {
                draw::set_draw_color(if item.focused { t.text } else { mix(t.text, t.background, 0.15) });
                draw::draw_text2(&fit(&item.label, tw), tx, by, tw, bh, Align::Left | Align::Inside);
            }
        }
        // Running windows: a dot each (up to 3); the focused app's is a bar.
        if running {
            let dy = by + bh - 3;
            if item.focused {
                draw::set_draw_color(t.accent);
                draw::draw_rounded_rectf(bx + bw / 2 - 7, dy, 14, 3, 1);
            } else {
                let n = item.windows.len().min(3) as i32;
                let total = n * 4 + (n - 1) * 3;
                draw::set_draw_color(mix(t.text, t.background, 0.4));
                for k in 0..n {
                    draw::draw_rounded_rectf(bx + (bw - total) / 2 + k * 7, dy, 4, 3, 1);
                }
            }
        }
        bx += bw + GAP;
    }
    draw::pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> App {
        App { id: id.into(), name: id.into(), icon: id.into(), exec: id.into() }
    }
    fn win(id: u64, app_id: &str, focused: bool) -> Win {
        Win { id, app_id: app_id.into(), title: format!("{app_id} {id}"), focused, app: Some(app(app_id)), workspace: Some(id % 2) }
    }
    fn cfg(show: TaskShow, style: TaskStyle) -> Config {
        Config { show, style, pinned: vec![app("foot"), app("firefox")], max_width: 600, fixed_width: false, button_width: 180, current_workspace: false }
    }

    fn items_all(cfg: &Config, wins: &[Win]) -> Vec<Item> {
        items(cfg, wins, None)
    }

    #[test]
    fn current_workspace_only() {
        let wins = [win(1, "mpv", false), win(2, "foot", true)];
        let mut c = cfg(TaskShow::Both, TaskStyle::Icons);
        c.current_workspace = true;
        let it = items(&c, &wins, Some(0));
        let icons: Vec<&str> = it.iter().map(|i| i.icon.as_str()).collect();
        assert_eq!(icons, ["foot", "firefox"], "mpv is on workspace 1; pinned apps stay");
        assert_eq!(items(&c, &wins, None).len(), 3, "unknown workspace: everything");
    }

    #[test]
    fn grouping_and_order() {
        let wins = [win(1, "mpv", false), win(2, "foot", true), win(3, "foot", false)];
        let it = items_all(&cfg(TaskShow::Both, TaskStyle::Icons), &wins);
        let ids: Vec<&str> = it.iter().map(|i| i.icon.as_str()).collect();
        assert_eq!(ids, ["foot", "firefox", "mpv"]);
        assert_eq!(it[0].windows, [2, 3]);
        assert!(it[0].focused && it[1].windows.is_empty());

        let it = items_all(&cfg(TaskShow::Running, TaskStyle::Icons), &wins);
        assert_eq!(it.len(), 2, "no launcher for firefox");

        let it = items_all(&cfg(TaskShow::Both, TaskStyle::IconsTitles), &wins);
        let labels: Vec<&str> = it.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["", "mpv 1", "foot 2", "foot 3"], "firefox launcher, then each window");
    }

    #[test]
    fn widths_shrink_to_fit() {
        let it = items_all(&cfg(TaskShow::Running, TaskStyle::IconsTitles), &[win(1, "a", false), win(2, "b", false)]);
        assert_eq!(widths(&it, TaskStyle::IconsTitles, 18, 180, 600), [180, 180]);
        assert_eq!(widths(&it, TaskStyle::IconsTitles, 18, 180, 204), [100, 100]);
        // Never smaller than an icon button.
        assert_eq!(widths(&it, TaskStyle::IconsTitles, 18, 180, 20), [34, 34]);
    }
}
