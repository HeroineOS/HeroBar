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
use crate::config::{self, Pinned};
use crate::edit::PinPath;
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

/// A pinned entry, resolved: an app (None if it isn't installed: kept so
/// positions match the config), or a folder.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    App(String, Option<App>),
    Folder { name: String, icon: Option<String>, entries: Vec<Entry> },
}

fn resolve(list: &[Pinned]) -> Vec<Entry> {
    list.iter()
        .map(|p| match p {
            Pinned::App(id) => Entry::App(id.clone(), crate::apps::by_id(id)),
            Pinned::Folder(f) => Entry::Folder { name: f.folder.clone(), icon: f.icon.clone(), entries: resolve(&f.apps) },
        })
        .collect()
}

/// The entries of the folder at `path` (the top level for an empty path).
pub fn folder_entries<'a>(entries: &'a [Entry], path: &[usize]) -> Option<&'a [Entry]> {
    let mut cur = entries;
    for &i in path {
        cur = match cur.get(i)? {
            Entry::Folder { entries, .. } => entries,
            Entry::App(..) => return None,
        };
    }
    Some(cur)
}

/// Desktop ids of every app in `entries`, folders included.
fn all_apps(entries: &[Entry]) -> Vec<&str> {
    let mut out = Vec::new();
    for e in entries {
        match e {
            Entry::App(id, _) => out.push(id.as_str()),
            Entry::Folder { entries, .. } => out.extend(all_apps(entries)),
        }
    }
    out
}

/// Icons of the first apps in a folder (for its button).
fn minis(entries: &[Entry]) -> Vec<String> {
    let mut out = Vec::new();
    for e in entries {
        if out.len() >= 4 {
            break;
        }
        match e {
            Entry::App(_, Some(a)) if !a.icon.is_empty() => out.push(a.icon.clone()),
            Entry::App(..) => {}
            Entry::Folder { entries, .. } => out.extend(minis(entries).into_iter().take(4 - out.len())),
        }
    }
    out
}

pub struct Config {
    pub show: TaskShow,
    pub style: TaskStyle,
    pub pinned: Vec<Entry>,
    /// As written in the config (for edits).
    pub raw: Vec<Pinned>,
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
            // Pinned apps that aren't installed aren't shown.
            pinned: resolve(c.pinned.as_deref().unwrap_or_default()),
            raw: c.pinned.clone().unwrap_or_default(),
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
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    pub icon: String,
    /// Shown in icons-titles style ("" = icon only).
    pub label: String,
    /// Shown on hover: the app's or folder's name, the window's title.
    pub name: String,
    /// The app's desktop id (or app id), for pinning.
    pub app_id: String,
    /// Window ids (one in icons-titles style).
    pub windows: Vec<u64>,
    pub focused: bool,
    /// What starts the app, for pinned apps.
    pub exec: Option<String>,
    /// Where it's pinned, if it is.
    pub pinned: Option<PinPath>,
    /// For a folder: its path, and small icons of its apps (when it has
    /// no icon of its own).
    pub folder: Option<PinPath>,
    pub minis: Vec<String>,
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

/// The button of pinned entry `e` at `path`, with its windows.
fn entry_item(e: &Entry, path: PinPath, windows: &[Win]) -> Option<Item> {
    match e {
        Entry::App(_, None) => None,
        Entry::App(id, Some(app)) => {
            let wins: Vec<&Win> = windows.iter().filter(|w| key(w) == id).collect();
            Some(Item {
                icon: if app.icon.is_empty() { wins.first().map(|w| icon_of(w)).unwrap_or_else(|| "app".into()) } else { app.icon.clone() },
                name: app.name.clone(),
                app_id: id.clone(),
                windows: wins.iter().map(|w| w.id).collect(),
                focused: wins.iter().any(|w| w.focused),
                exec: Some(app.exec.clone()),
                pinned: Some(path),
                ..Default::default()
            })
        }
        Entry::Folder { name, icon, entries } => {
            // Its apps' windows give it the running dots.
            let ids = all_apps(entries);
            let wins: Vec<&Win> = windows.iter().filter(|w| ids.contains(&key(w))).collect();
            Some(Item {
                icon: icon.clone().unwrap_or_default(),
                name: name.clone(),
                windows: wins.iter().map(|w| w.id).collect(),
                focused: wins.iter().any(|w| w.focused),
                minis: if icon.is_some() { vec![] } else { minis(entries) },
                folder: Some(path),
                ..Default::default()
            })
        }
    }
}

/// The buttons, in order: pinned apps and folders first, then other
/// running apps by when their first window opened. Running apps pinned in
/// a folder stay in it (the folder shows they run).
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
    let pinned_ids = all_apps(&cfg.pinned);
    let mut out: Vec<Item> = Vec::new();
    let show_pinned = cfg.show != TaskShow::Running;
    let running = cfg.show != TaskShow::Pinned;
    match cfg.style {
        TaskStyle::Icons => {
            for (i, e) in cfg.pinned.iter().enumerate() {
                if let Some(it) = entry_item(e, vec![i], windows) {
                    if show_pinned || !it.windows.is_empty() {
                        out.push(it);
                    }
                }
            }
            if running {
                let mut seen: Vec<&str> = Vec::new();
                for w in windows {
                    let k = key(w);
                    if pinned_ids.contains(&k) || seen.contains(&k) {
                        continue;
                    }
                    seen.push(k);
                    let wins: Vec<&Win> = windows.iter().filter(|x| key(x) == k).collect();
                    out.push(Item {
                        icon: icon_of(w),
                        name: w.app.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| w.app_id.clone()),
                        app_id: k.to_owned(),
                        windows: wins.iter().map(|x| x.id).collect(),
                        focused: wins.iter().any(|x| x.focused),
                        exec: w.app.as_ref().map(|a| a.exec.clone()),
                        ..Default::default()
                    });
                }
            }
        }
        TaskStyle::IconsTitles => {
            for (i, e) in cfg.pinned.iter().enumerate() {
                if let Some(mut it) = entry_item(e, vec![i], windows) {
                    // Open pinned apps show as their windows below.
                    let open_app = it.folder.is_none() && !it.windows.is_empty();
                    if !open_app && (show_pinned || !it.windows.is_empty()) {
                        if it.folder.is_none() {
                            it.windows.clear();
                        }
                        out.push(it);
                    }
                }
            }
            for w in windows {
                let k = key(w);
                let top_pinned = cfg.pinned.iter().any(|e| matches!(e, Entry::App(id, _) if id == k));
                let in_folder = pinned_ids.contains(&k) && !top_pinned;
                if in_folder || (!running && !top_pinned) {
                    continue;
                }
                let label = if w.title.is_empty() { w.app.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| w.app_id.clone()) } else { w.title.clone() };
                out.push(Item {
                    icon: icon_of(w),
                    name: label.clone(),
                    label,
                    app_id: k.to_owned(),
                    windows: vec![w.id],
                    focused: w.focused,
                    exec: w.app.as_ref().map(|a| a.exec.clone()),
                    pinned: crate::edit::find(&cfg.raw, k),
                    ..Default::default()
                });
            }
        }
    }
    out
}

/// What the right-click menu can do.
#[derive(Debug, Clone)]
pub enum TaskAction {
    Pin(String),
    Unpin(PinPath),
    MoveTo(String, PinPath),
    NewFolder(String),
    MoveOut(PinPath),
    /// Remove a folder, keeping its apps.
    Dissolve(PinPath),
    Close(Vec<u64>),
    Launch(String),
}

/// The right-click menu of `item`: labels ("-" first: a line above) and
/// what each does.
pub fn menu(item: &Item, folders: &[(PinPath, String)]) -> (Vec<String>, Vec<TaskAction>) {
    let mut m: Vec<(String, TaskAction)> = Vec::new();
    if let Some(path) = &item.folder {
        m.push(("Remove folder (keep its apps)".into(), TaskAction::Dissolve(path.clone())));
        m.push(("Rename or change icon...".into(), TaskAction::Launch("heroappearance".into())));
        return m.into_iter().unzip();
    }
    match &item.pinned {
        Some(p) => m.push(("Unpin".into(), TaskAction::Unpin(p.clone()))),
        None if !item.app_id.is_empty() => m.push(("Pin to taskbar".into(), TaskAction::Pin(item.app_id.clone()))),
        None => {}
    }
    if !item.app_id.is_empty() {
        let here = item.pinned.as_ref().map(|p| p[..p.len() - 1].to_vec());
        for (path, name) in folders {
            if Some(path) != here.as_ref() {
                m.push((format!("Move to {name}"), TaskAction::MoveTo(item.app_id.clone(), path.clone())));
            }
        }
        m.push(("Move to a new folder".into(), TaskAction::NewFolder(item.app_id.clone())));
        if let Some(p) = item.pinned.as_ref().filter(|p| p.len() > 1) {
            m.push(("Move out of the folder".into(), TaskAction::MoveOut(p.clone())));
        }
    }
    let first = m.len();
    if let Some(e) = &item.exec {
        m.push(("New window".into(), TaskAction::Launch(e.clone())));
    }
    match item.windows.len() {
        0 => {}
        1 => m.push(("Close window".into(), TaskAction::Close(item.windows.clone()))),
        n => m.push((format!("Close {n} windows"), TaskAction::Close(item.windows.clone()))),
    }
    // A line between pinning and window actions.
    if first > 0 && first < m.len() {
        m[first].0 = format!("-{}", m[first].0);
    }
    m.into_iter().unzip()
}

/// Tooltip texts live as long as FLTK may show them: each distinct one is
/// kept once (a bounded number).
fn tip(text: &str) -> Option<&'static std::ffi::CStr> {
    thread_local! {
        static TIPS: RefCell<std::collections::HashMap<String, &'static std::ffi::CStr>> = RefCell::new(Default::default());
    }
    TIPS.with(|t| {
        let mut t = t.borrow_mut();
        if let Some(c) = t.get(text) {
            return Some(*c);
        }
        if t.len() >= 256 {
            return None;
        }
        let c: &'static std::ffi::CStr = Box::leak(std::ffi::CString::new(text).ok()?.into_boxed_c_str());
        t.insert(text.to_owned(), c);
        Some(c)
    })
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
    /// Every pinned folder, for "Move to ..." in the menu.
    folders: Vec<(PinPath, String)>,
    style: TaskStyle,
    button_width: i32,
    hover: crate::fade::HoverFade,
    pressed: Option<(usize, i32)>,
}

impl View {
    /// Button widths in `w` px (the width the taskbar actually got).
    fn layout(&self, w: i32) -> Vec<i32> {
        widths(&self.items, self.style, icon_px(), self.button_width, w)
    }

    /// Button `i`'s x offset and width.
    fn span(&self, w: i32, i: usize) -> (i32, i32) {
        let ws = self.layout(w);
        let x = ws.iter().take(i).map(|w| w + GAP).sum();
        (x, ws.get(i).copied().unwrap_or(0))
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
        let v = Rc::new(RefCell::new(View { items: vec![], folders: vec![], style: TaskStyle::Icons, button_width: 180, hover: Default::default(), pressed: None }));
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
                        if h != s.hover.cur {
                            // Its name on hover (folders, apps).
                            if let Some(t) = h.and_then(|k| tip(&s.items[k].name)) {
                                let (bx, bw) = s.span(f.w(), h.unwrap_or(0));
                                heroui::fltk::misc::Tooltip::enter_area(f, f.x() + bx, f.y(), bw, f.h(), t);
                            }
                        }
                        s.hover.set(h, &f.as_base_widget());
                        true
                    }
                    Event::Leave => {
                        s.hover.set(None, &f.as_base_widget());
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
                        let Some((idx, button)) = s.pressed.take() else { return true };
                        if s.at(f.x(), f.w(), px) != Some(idx) {
                            return true;
                        }
                        let Some(item) = s.items.get(idx).cloned() else { return true };
                        if button == 3 {
                            // The menu runs its own event loop: let go of
                            // the state first.
                            let (labels, acts) = menu(&item, &s.folders);
                            drop(s);
                            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                            if let Some(k) = heroui::popup::context_menu(&refs) {
                                emit(Msg::TaskAction(i, acts[k].clone()));
                            }
                        } else if let (Some(path), 1) = (&item.folder, button) {
                            let (bx, bw) = s.span(f.w(), idx);
                            emit(Msg::OpenFolder(i, path.clone(), (bx, 0, bw, f.h())));
                        } else {
                            emit(Msg::Task(item, button));
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
            let items = if bar.gone(i) { vec![] } else { items(cfg, &bar.desktop.windows, bar.desktop.current_workspace()) };
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
            s.folders = crate::edit::folders(&cfg.raw);
            s.style = cfg.style;
            s.button_width = cfg.button_width;
            s.hover.clear();
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
        let under = if crate::islands_on() { crate::island_color() } else { t.background };
        let hover = mix(under, t.surface_alt, v.hover.amount(idx));
        let bg = if item.focused {
            Some(mix(t.surface_alt, t.accent, 0.18))
        } else if v.hover.amount(idx) > 0.0 {
            Some(hover)
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
        if item.folder.is_some() && item.icon.is_empty() {
            paint_minis(&item.minis, ix, iy, icon);
        } else if !heroui::icons::draw(&item.icon, ix, iy, icon, t.text) {
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

/// A folder's button: up to four of its apps, small, on a rounded tile.
fn paint_minis(minis: &[String], x: i32, y: i32, size: i32) {
    let t = heroui::theme::current();
    draw::set_draw_color(mix(t.surface_alt, t.text, 0.08));
    draw::draw_rounded_rectf(x - 2, y - 2, size + 4, size + 4, (size / 4).min(t.radius));
    if minis.is_empty() {
        heroui::icons::draw("folder", x + 2, y + 2, size - 4, t.text);
        return;
    }
    let half = (size - 2) / 2;
    for (k, icon) in minis.iter().take(4).enumerate() {
        let (cx, cy) = (x + (k as i32 % 2) * (half + 2), y + (k as i32 / 2) * (half + 2));
        heroui::icons::draw(icon, cx, cy, half, t.text);
    }
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
        let raw = vec![Pinned::App("foot".into()), Pinned::App("firefox".into())];
        let pinned = vec![Entry::App("foot".into(), Some(app("foot"))), Entry::App("firefox".into(), Some(app("firefox")))];
        Config { show, style, pinned, raw, max_width: 600, fixed_width: false, button_width: 180, current_workspace: false }
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
    fn folders_hold_running_apps() {
        let mut c = cfg(TaskShow::Both, TaskStyle::Icons);
        c.pinned.push(Entry::Folder { name: "Media".into(), icon: None, entries: vec![Entry::App("mpv".into(), Some(app("mpv")))] });
        let it = items(&c, &[win(1, "mpv", true)], None);
        let names: Vec<&str> = it.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["foot", "firefox", "Media"], "mpv stays in its folder");
        assert_eq!((it[2].windows.as_slice(), it[2].focused, it[2].minis.as_slice()), (&[1u64][..], true, &["mpv".to_string()][..]));
        let (labels, _) = menu(&it[0], &[(vec![2], "Media".into())]);
        assert_eq!(labels, ["Unpin", "Move to Media", "Move to a new folder", "-New window"]);
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

// --- Folder popup ---------------------------------------------------------

/// The open folder popup: which taskbar module, which folder, and where
/// its button is (relative to the taskbar).
#[derive(Debug, Clone, PartialEq)]
pub struct OpenFolder {
    pub module: usize,
    pub path: PinPath,
    pub rect: (i32, i32, i32, i32),
}

fn open_entries(bar: &Bar) -> Vec<Entry> {
    let Some(f) = &bar.folder else { return vec![] };
    let Some(cfg) = bar.modules.get(f.module).and_then(|m| m.taskbar.as_ref()) else { return vec![] };
    folder_entries(&cfg.pinned, &f.path).map(<[Entry]>::to_vec).unwrap_or_default()
}

/// The button of entry `k` of the open folder (for its menu and clicks).
pub fn open_item(bar: &Bar, k: usize) -> Option<Item> {
    let f = bar.folder.as_ref()?;
    let e = open_entries(bar).get(k)?.clone();
    let mut path = f.path.clone();
    path.push(k);
    entry_item(&e, path, &bar.desktop.windows)
}

fn folder_name(bar: &Bar) -> String {
    let Some(f) = &bar.folder else { return String::new() };
    let Some(cfg) = bar.modules.get(f.module).and_then(|m| m.taskbar.as_ref()) else { return String::new() };
    let (Some((&last, parent)),) = (f.path.split_last(),) else { return String::new() };
    match folder_entries(&cfg.pinned, parent).and_then(|e| e.get(last)) {
        Some(Entry::Folder { name, .. }) => name.clone(),
        _ => String::new(),
    }
}

const ROW: i32 = 38;

fn folder_row(k: usize) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let cur: Rc<RefCell<Option<Item>>> = Rc::default();
        let folders: Rc<RefCell<Vec<(PinPath, String)>>> = Rc::default();
        let mut b = custom_button({
            let cur = cur.clone();
            move |b| {
                let t = heroui::theme::current();
                let Some(it) = cur.borrow().clone() else { return };
                let a = if b.value() { 1.0 } else { heroui::hover::hover_amount(b) };
                if a > 0.0 {
                    draw::set_draw_color(mix(t.background, t.surface_alt, a));
                    draw::draw_rounded_rectf(b.x(), b.y(), b.w(), b.h(), t.radius.min(10));
                }
                let s = 22;
                let (ix, iy) = (b.x() + 8, b.y() + (b.h() - s) / 2);
                if it.folder.is_some() && it.icon.is_empty() {
                    paint_minis(&it.minis, ix, iy, s);
                } else if !heroui::icons::draw(&it.icon, ix, iy, s, t.text) {
                    heroui::icons::draw("app", ix, iy, s, t.text);
                }
                draw::set_font(if it.focused { t.bold_font() } else { t.font() }, t.font_size);
                draw::set_draw_color(t.text);
                let tx = ix + s + 10;
                draw::draw_text2(&it.name, tx, b.y(), b.x() + b.w() - tx - 80, b.h(), Align::Left | Align::Inside | Align::Clip);
                let note = match (it.folder.is_some(), it.windows.len()) {
                    (true, 0) => "Folder".to_string(),
                    (_, 0) => String::new(),
                    (_, 1) => "Open".to_string(),
                    (_, n) => format!("{n} windows"),
                };
                draw::set_font(t.font(), t.font_size - 2);
                draw::set_draw_color(t.text_dim);
                draw::draw_text2(&note, b.x(), b.y(), b.w() - 10, b.h(), Align::Right | Align::Inside);
            }
        });
        let emit = ctx.emitter();
        {
            let cur = cur.clone();
            let folders = folders.clone();
            b.set_callback(move |_| {
                let Some(it) = cur.borrow().clone() else { return };
                if app::event_mouse_button() == MouseButton::Right {
                    let (labels, acts) = menu(&it, &folders.borrow());
                    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                    if let Some(c) = heroui::popup::context_menu(&refs) {
                        emit(Msg::TaskAction(usize::MAX, acts[c].clone()));
                    }
                } else {
                    emit(Msg::FolderEntry(k));
                }
            });
        }
        let mut w = b.clone();
        ctx.bind(move |bar: &Bar| {
            let it = open_item(bar, k);
            if *cur.borrow() != it {
                *cur.borrow_mut() = it;
                if let Some(cfg) = bar.folder.as_ref().and_then(|f| bar.modules[f.module].taskbar.as_ref()) {
                    *folders.borrow_mut() = crate::edit::folders(&cfg.raw);
                }
                heroui::widgets::repaint(&mut w);
            }
        });
        b.as_base_widget()
    })
    .fixed(ROW)
}

pub fn folder_view() -> Element<Bar, Msg> {
    column(vec![
        row(vec![
            button("<", Msg::FolderBack).fixed(40).visible(|b: &Bar| b.folder.as_ref().is_some_and(|f| f.path.len() > 1)),
            canvas(folder_name, |name: &String, x, y, w, h, t: &Theme| {
                draw::set_font(t.bold_font(), t.font_size + 1);
                draw::set_draw_color(t.text);
                draw::draw_text2(name, x + 4, y, w - 4, h, Align::Left | Align::Inside);
            }),
        ])
        .fixed(34),
        scroll(vec![list(|b: &Bar| open_entries(b).len(), folder_row)]),
    ])
    .padding(12)
    .spacing(6)
}

pub fn folder_size(b: &Bar) -> (i32, i32) {
    let n = open_entries(b).len().clamp(1, 8) as i32;
    let gap = heroui::theme::current().spacing;
    (300, 12 + 34 + 6 + n * (ROW + gap) - gap + 12)
}
