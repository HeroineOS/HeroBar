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

/// How an open folder shows its apps.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FolderStyle {
    /// Rows of icon and name, like a file manager's list view.
    #[default]
    List,
    /// Icons with their names under them, like a phone's folders.
    Grid,
    /// Icons only (the pointed one's name below them).
    Icons,
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
    pub folder_style: FolderStyle,
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
            folder_style: match c.folder_style.as_deref() {
                Some("grid") => FolderStyle::Grid,
                Some("icons") => FolderStyle::Icons,
                _ => FolderStyle::List,
            },
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
    /// Put a pinned entry (its path) or an app (pinning it) in the folder
    /// at the path (`[]`: the top level), before its entry n (None: after
    /// the last).
    Put(Option<PinPath>, String, PinPath, Option<usize>),
    /// Make a folder of a pinned entry or app and the app at the path (or
    /// an unpinned app), where the latter was.
    Merge(Option<PinPath>, String, Option<PinPath>, String),
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

/// A button being carried (after a long press), in the taskbar or in an
/// open folder.
#[derive(Debug, Clone)]
pub struct Carry {
    pub item: Item,
    /// Its index among the buttons (None: carried in from a folder popup).
    from: Option<usize>,
    /// It lands before button `gap` (the count: at the end). The others
    /// make room there.
    gap: usize,
    /// The button it's held over (the middle of it), and whether it has
    /// been held there long enough to make a folder of the two.
    over: Option<usize>,
    merge: bool,
    /// Bumped when `over` changes: an older merge timer does nothing.
    over_id: u32,
    /// The pointer, in the widget.
    at: (i32, i32),
}

impl Carry {
    fn new(item: Item, from: Option<usize>, gap: usize, at: (i32, i32)) -> Carry {
        Carry { item, from, gap, over: None, merge: false, over_id: 0, at }
    }

    /// Aims at what's under the pointer, given where the other buttons
    /// are drawn (`placed`: index and rectangle, in order). The edges of
    /// a button move the gap next to it; its middle is "over" it. Over
    /// the gap itself, nothing changes, so the gap doesn't run away from
    /// the pointer. `vertical`: a list, edges are top and bottom. Returns
    /// true if `over` changed (a merge timer should start).
    fn aim(&mut self, placed: &[(usize, Rect)], count: usize, vertical: bool) -> bool {
        let (px, py) = self.at;
        let mut over = None;
        match placed.iter().find(|(_, (x, y, w, h))| px >= *x && px < x + w && py >= *y && py < y + h) {
            Some(&(k, (x, y, w, h))) => {
                let r = if vertical { (py - y) as f32 / h as f32 } else { (px - x) as f32 / w as f32 };
                if r < 0.3 {
                    self.gap = k;
                } else if r > 0.7 {
                    self.gap = k + 1;
                } else {
                    over = Some(k);
                }
            }
            None => {
                // Past the last button: the end; before the first: the start.
                if let Some(&(_, (x, y, w, h))) = placed.last() {
                    if py >= y + h || (py >= y && px >= x + w) {
                        self.gap = count;
                    }
                }
                if let Some(&(k, (x, y, _, h))) = placed.first() {
                    if py < y || (py < y + h && px < x) {
                        self.gap = k;
                    }
                }
            }
        }
        if over != self.over {
            self.over = over;
            self.merge = false;
            self.over_id = self.over_id.wrapping_add(1);
            return over.is_some();
        }
        false
    }

    /// True if holding it over `target` can make a folder of the two.
    fn can_merge(&self, target: &Item) -> bool {
        self.item.folder.is_none() && !self.item.app_id.is_empty() && target.folder.is_none() && !target.app_id.is_empty()
    }
}

/// How long the carried button must rest on another app to make a folder.
const MERGE_HOLD: f64 = 0.6;

/// Starts the merge timer for `carry()`'s current target; when it fires
/// and the button is still held there, the two can be merged (shown).
fn arm_merge(get: impl Fn() -> Option<(u32, bool)> + 'static, mut set: impl FnMut() + 'static) {
    let Some((id, _)) = get() else { return };
    app::add_timeout3(MERGE_HOLD, move |_| {
        if let Some((now, eligible)) = get() {
            if now == id && eligible {
                set();
            }
        }
    });
}

/// What dropping carried `c` does, among `items` (the taskbar's buttons,
/// or an open folder's, which is at `dest`).
fn carry_action(items: &[Item], c: &Carry, dest: &[usize]) -> Option<TaskAction> {
    let it = &c.item;
    let from_path = it.pinned.clone().or_else(|| it.folder.clone());
    if let Some(t) = c.over.and_then(|k| items.get(k)) {
        if it.folder.is_none() && !it.app_id.is_empty() {
            if let Some(f) = &t.folder {
                // Into the folder (unless it's already there).
                let here = from_path.as_ref().is_some_and(|p| p[..p.len() - 1] == f[..]);
                return (!here).then(|| TaskAction::Put(from_path, it.app_id.clone(), f.clone(), None));
            }
            if c.merge && c.can_merge(t) {
                return Some(TaskAction::Merge(from_path, it.app_id.clone(), t.pinned.clone(), t.app_id.clone()));
            }
        }
    }
    if let Some(f) = c.from {
        if c.gap == f || c.gap == f + 1 {
            return None;
        }
    }
    if from_path.is_none() && it.app_id.is_empty() {
        return None;
    }
    // The pinned entry it goes before, at this level (the end of the
    // pinned ones if it's dropped among running apps).
    let before = items[c.gap.min(items.len())..].iter().find_map(|x| {
        x.pinned.as_ref().or(x.folder.as_ref()).filter(|p| p.len() == dest.len() + 1 && p.starts_with(dest)).map(|p| p[dest.len()])
    });
    Some(TaskAction::Put(from_path, it.app_id.clone(), dest.to_vec(), before))
}

/// A button carried out of a folder popup, over the bar.
#[derive(Debug, Clone, PartialEq)]
pub struct CarryOut {
    /// The taskbar module whose folder it comes from.
    pub module: usize,
    pub item: Item,
    /// The pointer, in the bar window.
    pub at: (i32, i32),
}

thread_local! {
    /// What releasing the button carried out of a folder would do (the
    /// taskbar under the pointer works it out; the folder popup gets the
    /// release).
    static OUT_DROP: RefCell<Option<(usize, TaskAction)>> = const { RefCell::new(None) };
}

/// Takes what dropping the button carried out of a folder does, if it's
/// over a taskbar.
pub fn take_out_drop() -> Option<(usize, TaskAction)> {
    OUT_DROP.with(|d| d.borrow_mut().take())
}

struct View {
    items: Vec<Item>,
    /// Every pinned folder, for "Move to ..." in the menu.
    folders: Vec<(PinPath, String)>,
    style: TaskStyle,
    button_width: i32,
    hover: crate::fade::HoverFade,
    /// The button pressed (index, mouse button), and where.
    pressed: Option<(usize, i32, i32)>,
    /// Bumped by every press: a long-press timer of an older press does
    /// nothing.
    press_id: u32,
    /// Picked up by a long press (or carried in from a folder).
    drag: Option<Carry>,
}

/// How long a press must last to pick a button up and move it.
const HOLD: f64 = 0.45;

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

    /// The button at `px` (offset in the widget).
    fn at(&self, w: i32, px: i32) -> Option<usize> {
        let mut x = 0;
        for (i, w) in self.layout(w).iter().enumerate() {
            if px >= x && px < x + w {
                return Some(i);
            }
            x += w + GAP;
        }
        None
    }

    /// The carried button's width.
    fn carry_width(&self, w: i32, c: &Carry) -> i32 {
        c.from.and_then(|f| self.layout(w).get(f).copied()).unwrap_or(icon_px() + 2 * BTN_PAD)
    }

    /// Where the other buttons are while `c` is carried: (index, (x, 0, w,
    /// h)), with room for it at its gap.
    fn placed(&self, w: i32, h: i32, c: &Carry) -> Vec<(usize, Rect)> {
        let cw = self.carry_width(w, c);
        let mut x = 0;
        let mut out = Vec::new();
        for (k, &bw) in self.layout(w).iter().enumerate() {
            if k == c.gap {
                x += cw + GAP;
            }
            if Some(k) == c.from {
                continue;
            }
            out.push((k, (x, 0, bw, h)));
            x += bw + GAP;
        }
        out
    }

    /// Aims carried button at the pointer; true if a merge timer should
    /// start.
    fn aim(&mut self, w: i32, h: i32) -> bool {
        let Some(mut c) = self.drag.take() else { return false };
        let placed = self.placed(w, h, &c);
        let start = c.aim(&placed, self.items.len(), false);
        let eligible = c.over.and_then(|k| self.items.get(k)).is_some_and(|t| c.can_merge(t));
        self.drag = Some(c);
        start && eligible
    }
}

/// Starts the merge timer of the taskbar's carried button.
fn arm_view_merge(v: &Rc<RefCell<View>>, w: &heroui::fltk::widget::Widget) {
    let (v1, v2, mut w) = (v.clone(), v.clone(), w.clone());
    arm_merge(
        move || {
            let s = v1.try_borrow().ok()?;
            let c = s.drag.as_ref()?;
            Some((c.over_id, c.over.and_then(|k| s.items.get(k)).is_some_and(|t| c.can_merge(t))))
        },
        move || {
            if let Ok(mut s) = v2.try_borrow_mut() {
                if let Some(c) = s.drag.as_mut() {
                    c.merge = true;
                }
            }
            repaint(&mut w);
        },
    );
}

pub fn view(i: usize) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let v = Rc::new(RefCell::new(View {
            items: vec![],
            folders: vec![],
            style: TaskStyle::Icons,
            button_width: 180,
            hover: Default::default(),
            pressed: None,
            press_id: 0,
            drag: None,
        }));
        let mut f = Frame::default();
        f.set_frame(FrameType::NoBox);
        {
            let v = v.clone();
            f.draw(move |f| {
                // Never fails in practice: the handler doesn't hold the
                // state while FLTK could draw.
                if let Ok(v) = v.try_borrow() {
                    paint(&v, f.x(), f.y(), f.w(), f.h());
                }
            });
        }
        let emit = ctx.emitter();
        {
            let v = v.clone();
            // Rule here: borrow the state only between FLTK calls (a
            // tooltip or a menu can run the event loop, which comes back
            // into this handler).
            f.handle(move |f, ev| {
                let px = app::event_x() - f.x();
                let me = f.as_base_widget();
                match ev {
                    Event::Enter | Event::Move => {
                        let (h, tipped) = {
                            let s = v.borrow();
                            let h = s.at(f.w(), px);
                            let tipped = (h != s.hover.cur).then(|| h.and_then(|k| tip(&s.items[k].name).map(|t| (s.span(f.w(), k), t)))).flatten();
                            (h, tipped)
                        };
                        // Its name on hover (folders, apps), centered under it.
                        if let Some(((bx, bw), t)) = tipped {
                            heroui::fltk::misc::Tooltip::enter_area(f, bx, 0, bw, f.h(), t);
                        }
                        v.borrow_mut().hover.set(h, &me);
                        true
                    }
                    Event::Leave => {
                        v.borrow_mut().hover.set(None, &me);
                        true
                    }
                    Event::Push => {
                        let button = match app::event_mouse_button() {
                            MouseButton::Middle => 2,
                            MouseButton::Right => 3,
                            _ => 1,
                        };
                        let (hit, menu_args, id) = {
                            let mut s = v.borrow_mut();
                            let hit = s.at(f.w(), px);
                            s.press_id = s.press_id.wrapping_add(1);
                            s.pressed = hit.map(|k| (k, button, px));
                            let menu_args = (button == 3).then(|| hit.and_then(|k| s.items.get(k).map(|it| (k, menu(it, &s.folders), s.span(f.w(), k))))).flatten();
                            (hit, menu_args, s.press_id)
                        };
                        // Right button: its menu, opened on the press like
                        // the other popups.
                        if let Some((_, (labels, acts), (bx, bw))) = menu_args {
                            emit(Msg::OpenMenu(crate::TaskMenu { module: i, row: None, labels, acts, rect: (bx, 0, bw, f.h()) }));
                            v.borrow_mut().pressed = None;
                            return true;
                        }
                        // Held long enough without moving: picked up.
                        if hit.is_some() && button == 1 {
                            let (v, mut w) = (v.clone(), me.clone());
                            app::add_timeout3(HOLD, move |_| {
                                let mut s = v.borrow_mut();
                                if s.press_id == id {
                                    if let Some((k, 1, x)) = s.pressed {
                                        if let Some(item) = s.items.get(k).cloned() {
                                            s.drag = Some(Carry::new(item, Some(k), k, (x, 0)));
                                            s.hover.clear();
                                            drop(s);
                                            repaint(&mut w);
                                        }
                                    }
                                }
                            });
                        }
                        true
                    }
                    Event::Drag => {
                        let mut s = v.borrow_mut();
                        if let Some(c) = s.drag.as_mut() {
                            c.at = (px, app::event_y() - f.y());
                            let arm = s.aim(f.w(), f.h());
                            drop(s);
                            if arm {
                                arm_view_merge(&v, &me);
                            }
                            let mut w = me.clone();
                            repaint(&mut w);
                        } else if let Some((_, _, x0)) = s.pressed {
                            // Moved before the hold: not a pick-up (nor a click).
                            if (px - x0).abs() > 8 {
                                s.pressed = None;
                            }
                        }
                        true
                    }
                    Event::Released => {
                        let (drag, act, item) = {
                            let mut s = v.borrow_mut();
                            let drag = s.drag.take();
                            let pressed = s.pressed.take();
                            s.press_id = s.press_id.wrapping_add(1);
                            let act = drag.as_ref().and_then(|c| carry_action(&s.items, c, &[]));
                            let item = pressed.and_then(|(k, b, _)| (s.at(f.w(), px) == Some(k)).then(|| (k, b, s.items.get(k).cloned(), s.span(f.w(), k))));
                            (drag, act, item)
                        };
                        if drag.is_some() {
                            if let Some(a) = act {
                                emit(Msg::TaskAction(i, a));
                            }
                            let mut w = me.clone();
                            repaint(&mut w);
                            return true;
                        }
                        if let Some((_, button, Some(item), (bx, bw))) = item {
                            if let (Some(path), 1) = (&item.folder, button) {
                                emit(Msg::OpenFolder(i, path.clone(), (bx, 0, bw, f.h())));
                            } else {
                                emit(Msg::Task(item, button));
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
            let items = if bar.gone(i) { vec![] } else { items(cfg, &bar.desktop.windows, bar.desktop.current_workspace()) };
            carry_in(&v, &w, bar.carry.as_ref().filter(|c| c.module == i), i);
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
            s.drag = None;
            drop(s);
            if width != last_width.replace(width) {
                crate::fit::set_width(&mut w, width);
            }
            repaint(&mut w);
        });
        f.as_base_widget()
    })
}

/// Follows a button carried out of a folder (`out`) over taskbar `module`
/// (widget `w`): it makes room for it like for its own, and works out what
/// dropping it would do.
fn carry_in(v: &Rc<RefCell<View>>, w: &heroui::fltk::widget::Widget, out: Option<&CarryOut>, module: usize) {
    let inside = out.filter(|o| o.at.0 >= w.x() && o.at.0 < w.x() + w.w() && o.at.1 >= w.y() - 4 && o.at.1 < w.y() + w.h() + 8);
    let mut s = v.borrow_mut();
    let external = s.drag.as_ref().is_some_and(|d| d.from.is_none());
    let Some(o) = inside else {
        if external {
            s.drag = None;
            drop(s);
            OUT_DROP.with(|d| d.borrow_mut().take());
            repaint(&mut w.clone());
        }
        return;
    };
    if !external {
        let n = s.items.len();
        s.drag = Some(Carry::new(o.item.clone(), None, n, (0, 0)));
    }
    if let Some(c) = s.drag.as_mut() {
        c.at = (o.at.0 - w.x(), (o.at.1 - w.y()).clamp(0, w.h() - 1));
    }
    let arm = s.aim(w.w(), w.h());
    let act = s.drag.as_ref().and_then(|c| carry_action(&s.items, c, &[]));
    drop(s);
    OUT_DROP.with(|d| *d.borrow_mut() = act.map(|a| (module, a)));
    if arm {
        arm_view_merge(v, w);
    }
    repaint(&mut w.clone());
}

/// Draws one button at (bx, by).
fn paint_item(v: &View, idx: usize, item: &Item, (bx, by, bw, bh): (i32, i32, i32, i32), lifted: bool) {
    let t = heroui::theme::current();
    let icon = icon_px();
    let running = !item.windows.is_empty();
    let under = if crate::islands_on() { crate::island_color() } else { t.background };
    let bg = if lifted {
        Some(mix(t.surface_alt, t.accent, 0.35))
    } else if item.focused {
        Some(mix(t.surface_alt, t.accent, 0.18))
    } else if v.hover.amount(idx) > 0.0 {
        Some(mix(under, t.surface_alt, v.hover.amount(idx)))
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
    if running && !lifted {
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
}

/// The target under a carried button: a ring (it goes into the folder, or
/// a folder will be made of the two), and a tile behind a merge.
fn paint_target(c: &Carry, target: &Item, (x, y, w, h): (i32, i32, i32, i32), r: i32) {
    let t = heroui::theme::current();
    let into = target.folder.is_some() && c.item.folder.is_none() && !c.item.app_id.is_empty();
    if c.merge && c.can_merge(target) {
        draw::set_draw_color(mix(t.surface_alt, t.accent, 0.3));
        draw::draw_rounded_rectf(x - 2, y - 2, w + 4, h + 4, r + 2);
    }
    if into || (c.merge && c.can_merge(target)) {
        draw::set_draw_color(t.accent);
        draw::draw_rounded_rect(x - 2, y - 2, w + 4, h + 4, r + 2);
    }
}

fn paint(v: &View, x: i32, y: i32, w: i32, h: i32) {
    let t = heroui::theme::current();
    crate::island(x, y, w, h);
    let bh = h - 8;
    let by = y + 4;
    draw::push_clip(x, y, w, h);
    draw::set_font(t.font(), t.font_size - 1);
    let r = t.radius.min(bh / 2).min(8);
    match &v.drag {
        None => {
            let mut bx = x;
            for (idx, (item, bw)) in v.items.iter().zip(v.layout(w)).enumerate() {
                paint_item(v, idx, item, (bx, by, bw, bh), false);
                bx += bw + GAP;
            }
        }
        Some(c) => {
            // The others make room where it would land.
            for (idx, (bx, _, bw, _)) in v.placed(w, h, c) {
                let item = &v.items[idx];
                if c.over == Some(idx) {
                    paint_target(c, item, (x + bx, by, bw, bh), r);
                }
                paint_item(v, idx, item, (x + bx, by, bw, bh), false);
            }
            // The carried button follows the pointer, a bit raised.
            let cw = v.carry_width(w, c);
            let cx = (x + c.at.0 - cw / 2).clamp(x, x + w - cw);
            paint_item(v, usize::MAX, &c.item, (cx, by - 2, cw, bh), true);
        }
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
        Config { show, style, pinned, raw, max_width: 600, fixed_width: false, button_width: 180, current_workspace: false, folder_style: FolderStyle::List }
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

/// The open folder's buttons (its installed apps and folders).
pub fn open_items(bar: &Bar) -> Vec<Item> {
    let Some(f) = &bar.folder else { return vec![] };
    open_entries(bar)
        .iter()
        .enumerate()
        .filter_map(|(k, e)| {
            let mut path = f.path.clone();
            path.push(k);
            entry_item(e, path, &bar.desktop.windows)
        })
        .collect()
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

fn folder_style(bar: &Bar) -> FolderStyle {
    bar.folder.as_ref().and_then(|f| bar.modules.get(f.module)).and_then(|m| m.taskbar.as_ref()).map_or_else(FolderStyle::default, |c| c.folder_style)
}

/// List style: row height and gap.
const ROW: i32 = 38;
const ROW_GAP: i32 = 4;
/// Grid and icons styles: cell sizes.
const GRID: (i32, i32) = (80, 82);
const ICONS: (i32, i32) = (52, 52);
/// Icons style: room under the icons for the hovered app's name.
const NAME_H: i32 = 24;

type Rect = (i32, i32, i32, i32);

/// The open folder's content: one widget drawing every button, in the
/// folder style.
struct FView {
    items: Vec<Item>,
    style: FolderStyle,
    folders: Vec<(PinPath, String)>,
    /// The folder shown, and its taskbar module.
    path: PinPath,
    module: usize,
    hover: crate::fade::HoverFade,
    /// The button pressed (index, mouse button, where).
    pressed: Option<(usize, i32, (i32, i32))>,
    press_id: u32,
    /// Where the button went down, while it can still become a scroll.
    down: Option<(i32, i32)>,
    scrolling: Option<i32>,
    drag: Option<Carry>,
    /// The carried button is outside the popup (over the bar).
    out: bool,
    scroll: i32,
}

impl FView {
    fn cell(&self, w: i32) -> (i32, i32) {
        match self.style {
            FolderStyle::List => (w, ROW),
            FolderStyle::Grid => GRID,
            FolderStyle::Icons => ICONS,
        }
    }

    fn cols(&self, w: i32) -> usize {
        match self.style {
            FolderStyle::List => 1,
            _ => (w / self.cell(w).0).max(1) as usize,
        }
    }

    fn row_gap(&self) -> i32 {
        if self.style == FolderStyle::List {
            ROW_GAP
        } else {
            0
        }
    }

    /// The height the buttons are shown in (icons style: above the name).
    fn view_h(&self, h: i32) -> i32 {
        if self.style == FolderStyle::Icons {
            h - NAME_H
        } else {
            h
        }
    }

    /// Slot `n` (in the widget, scrolled).
    fn slot(&self, w: i32, n: usize) -> Rect {
        let (cw, ch) = self.cell(w);
        let cols = self.cols(w);
        let used = cols.min(self.items.len().max(1)) as i32 * cw;
        let x0 = if self.style == FolderStyle::List { 0 } else { (w - used) / 2 };
        let (c, r) = ((n % cols) as i32, (n / cols) as i32);
        (x0 + c * cw, r * (ch + self.row_gap()) - self.scroll, cw, ch)
    }

    /// Where the buttons are (index, rectangle): while one is carried, the
    /// others close up behind it and make room at its gap.
    fn placed(&self, w: i32) -> Vec<(usize, Rect)> {
        let mut n = 0;
        let mut out = Vec::new();
        for k in 0..self.items.len() {
            if let Some(c) = &self.drag {
                if k == c.gap && !self.out {
                    n += 1;
                }
                if Some(k) == c.from {
                    continue;
                }
            }
            out.push((k, self.slot(w, n)));
            n += 1;
        }
        out
    }

    fn at(&self, w: i32, (px, py): (i32, i32)) -> Option<usize> {
        self.placed(w).into_iter().find(|(_, (x, y, cw, ch))| px >= *x && px < x + cw && py >= *y && py < y + ch).map(|(k, _)| k)
    }

    fn content_h(&self, w: i32) -> i32 {
        let rows = self.items.len().div_ceil(self.cols(w)) as i32;
        (rows * (self.cell(w).1 + self.row_gap()) - self.row_gap()).max(0)
    }

    fn scroll_by(&mut self, w: i32, h: i32, dy: i32) {
        self.scroll = (self.scroll + dy).clamp(0, (self.content_h(w) - self.view_h(h)).max(0));
    }

    /// Aims the carried button; true if a merge timer should start.
    fn aim(&mut self, w: i32) -> bool {
        let Some(mut c) = self.drag.take() else { return false };
        let placed = self.placed(w);
        let start = c.aim(&placed, self.items.len(), self.style == FolderStyle::List);
        let eligible = c.over.and_then(|k| self.items.get(k)).is_some_and(|t| c.can_merge(t));
        self.drag = Some(c);
        start && eligible
    }
}

/// An app or folder icon (a folder without one: its apps' icons).
fn entry_icon(it: &Item, x: i32, y: i32, s: i32) {
    let t = heroui::theme::current();
    if it.folder.is_some() && it.icon.is_empty() {
        paint_minis(&it.minis, x, y, s);
    } else if !heroui::icons::draw(&it.icon, x, y, s, t.text) {
        heroui::icons::draw("app", x, y, s, t.text);
    }
}

/// One button of the open folder, in its style.
fn paint_entry(v: &FView, idx: usize, it: &Item, (x, y, w, h): Rect, lifted: bool) {
    let t = heroui::theme::current();
    let r = t.radius.min(10);
    let a = v.hover.amount(idx);
    if lifted {
        draw::set_draw_color(mix(t.surface_alt, t.accent, 0.3));
        draw::draw_rounded_rectf(x, y, w, h, r);
    } else if a > 0.0 {
        draw::set_draw_color(mix(t.background, t.surface_alt, a));
        draw::draw_rounded_rectf(x, y, w, h, r);
    }
    // Running: a dot under it (a bar for the focused app's).
    let dot = |cx: i32, dy: i32| {
        if it.windows.is_empty() || lifted {
            return;
        }
        if it.focused {
            draw::set_draw_color(t.accent);
            draw::draw_rounded_rectf(cx - 6, dy, 12, 3, 1);
        } else {
            draw::set_draw_color(mix(t.text, t.background, 0.4));
            draw::draw_rounded_rectf(cx - 2, dy, 4, 3, 1);
        }
    };
    match v.style {
        FolderStyle::List => {
            let s = 22;
            let (ix, iy) = (x + 8, y + (h - s) / 2);
            entry_icon(it, ix, iy, s);
            draw::set_font(if it.focused { t.bold_font() } else { t.font() }, t.font_size);
            draw::set_draw_color(t.text);
            let tx = ix + s + 10;
            draw::draw_text2(&it.name, tx, y, x + w - tx - 80, h, Align::Left | Align::Inside | Align::Clip);
            let note = match (it.folder.is_some(), it.windows.len()) {
                (true, 0) => "Folder".to_string(),
                (_, 0) => String::new(),
                (_, 1) => "Open".to_string(),
                (_, n) => format!("{n} windows"),
            };
            draw::set_font(t.font(), t.font_size - 2);
            draw::set_draw_color(t.text_dim);
            draw::draw_text2(&note, x, y, w - 10, h, Align::Right | Align::Inside);
        }
        FolderStyle::Grid => {
            let s = 36;
            entry_icon(it, x + (w - s) / 2, y + 8, s);
            draw::set_font(t.font(), (t.font_size - 3).max(9));
            draw::set_draw_color(t.text);
            draw::draw_text2(&fit(&it.name, w - 8), x, y + 8 + s + 4, w, t.font_size, Align::Center | Align::Inside);
            dot(x + w / 2, y + h - 6);
        }
        FolderStyle::Icons => {
            let s = 32;
            entry_icon(it, x + (w - s) / 2, y + (h - s) / 2 - 2, s);
            dot(x + w / 2, y + h - 6);
        }
    }
}

fn paint_folder(v: &FView, x: i32, y: i32, w: i32, h: i32) {
    let t = heroui::theme::current();
    let vh = v.view_h(h);
    draw::push_clip(x, y, w, vh);
    let r = t.radius.min(10);
    for (k, (bx, by, bw, bh)) in v.placed(w) {
        let rect = (x + bx, y + by, bw, bh);
        if let Some(c) = v.drag.as_ref().filter(|c| c.over == Some(k) && !v.out) {
            paint_target(c, &v.items[k], rect, r);
        }
        paint_entry(v, k, &v.items[k], rect, false);
    }
    // The carried button follows the pointer, while it's in here.
    if let Some(c) = v.drag.as_ref().filter(|_| !v.out) {
        let (cw, ch) = v.cell(w);
        let cx = (x + c.at.0 - cw / 2).clamp(x, x + w - cw);
        let cy = (y + c.at.1 - ch / 2).clamp(y, y + vh - ch);
        paint_entry(v, usize::MAX, &c.item, (cx, cy, cw, ch), true);
    }
    draw::pop_clip();
    // Icons only: the name of the one pointed at (or carried).
    if v.style == FolderStyle::Icons {
        let name = match &v.drag {
            Some(c) => c.item.name.as_str(),
            None => v.hover.cur.and_then(|k| v.items.get(k)).map_or("", |i| i.name.as_str()),
        };
        draw::set_font(t.font(), t.font_size - 1);
        draw::set_draw_color(t.text_dim);
        draw::draw_text2(&fit(name, w - 8), x, y + vh, w, NAME_H, Align::Center | Align::Inside);
    }
}

/// Starts the merge timer of the open folder's carried button.
fn arm_folder_merge(v: &Rc<RefCell<FView>>, w: &heroui::fltk::widget::Widget) {
    let (v1, v2, mut w) = (v.clone(), v.clone(), w.clone());
    arm_merge(
        move || {
            let s = v1.try_borrow().ok()?;
            let c = s.drag.as_ref().filter(|_| !s.out)?;
            Some((c.over_id, c.over.and_then(|k| s.items.get(k)).is_some_and(|t| c.can_merge(t))))
        },
        move || {
            if let Ok(mut s) = v2.try_borrow_mut() {
                if let Some(c) = s.drag.as_mut() {
                    c.merge = true;
                }
            }
            repaint(&mut w);
        },
    );
}

/// The open folder's buttons. Click: open (a folder: go into it); right
/// click: its menu; press and hold: carry it, to another place in the
/// folder, onto a folder or app, or out onto the taskbar.
fn folder_grid() -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let v = Rc::new(RefCell::new(FView {
            items: vec![],
            style: FolderStyle::List,
            folders: vec![],
            path: vec![],
            module: 0,
            hover: Default::default(),
            pressed: None,
            press_id: 0,
            down: None,
            scrolling: None,
            drag: None,
            out: false,
            scroll: 0,
        }));
        let mut f = Frame::default();
        f.set_frame(FrameType::NoBox);
        {
            let v = v.clone();
            f.draw(move |f| {
                if let Ok(v) = v.try_borrow() {
                    paint_folder(&v, f.x(), f.y(), f.w(), f.h());
                }
            });
        }
        let emit = ctx.emitter();
        {
            let v = v.clone();
            // As in the taskbar: no borrow across FLTK calls.
            f.handle(move |f, ev| {
                let p = (app::event_x() - f.x(), app::event_y() - f.y());
                let me = f.as_base_widget();
                let mut w = me.clone();
                match ev {
                    Event::Enter | Event::Move => {
                        let h = v.borrow().at(f.w(), p);
                        v.borrow_mut().hover.set(h, &me);
                        true
                    }
                    Event::Leave => {
                        v.borrow_mut().hover.set(None, &me);
                        true
                    }
                    Event::MouseWheel => {
                        let dy = match app::event_dy() {
                            app::MouseWheel::Down => 40,
                            app::MouseWheel::Up => -40,
                            _ => 0,
                        };
                        v.borrow_mut().scroll_by(f.w(), f.h(), dy);
                        repaint(&mut w);
                        true
                    }
                    Event::Push => {
                        let button = match app::event_mouse_button() {
                            MouseButton::Right => 3,
                            _ => 1,
                        };
                        let (menu_args, hold) = {
                            let mut s = v.borrow_mut();
                            let hit = s.at(f.w(), p);
                            s.press_id = s.press_id.wrapping_add(1);
                            s.pressed = hit.map(|k| (k, button, p));
                            s.down = Some(p);
                            s.scrolling = None;
                            let placed = s.placed(f.w());
                            let menu_args = (button == 3)
                                .then(|| {
                                    let k = hit?;
                                    let (_, (x, y, cw, ch)) = placed.into_iter().find(|(i, _)| *i == k)?;
                                    Some((menu(&s.items[k], &s.folders), (x, y, cw, ch)))
                                })
                                .flatten();
                            (menu_args, (button == 1 && hit.is_some()).then_some(s.press_id))
                        };
                        if let Some(((labels, acts), rect)) = menu_args {
                            v.borrow_mut().pressed = None;
                            emit(Msg::OpenMenu(crate::TaskMenu { module: usize::MAX, row: Some(0), labels, acts, rect }));
                            return true;
                        }
                        if let Some(id) = hold {
                            let (v, mut w) = (v.clone(), me.clone());
                            app::add_timeout3(HOLD, move |_| {
                                let mut s = v.borrow_mut();
                                if s.press_id != id {
                                    return;
                                }
                                if let Some((k, 1, at)) = s.pressed {
                                    if let Some(item) = s.items.get(k).cloned() {
                                        s.drag = Some(Carry::new(item, Some(k), k, at));
                                        s.out = false;
                                        s.hover.clear();
                                        drop(s);
                                        repaint(&mut w);
                                    }
                                }
                            });
                        }
                        true
                    }
                    Event::Drag => {
                        let mut s = v.borrow_mut();
                        if s.drag.is_some() {
                            // Outside the popup: over the bar, maybe.
                            let (ex, ey) = (app::event_x(), app::event_y());
                            let win = f.window();
                            let out = win.as_ref().is_some_and(|win| ex < 0 || ey < 0 || ex >= win.w() || ey >= win.h());
                            let was_out = std::mem::replace(&mut s.out, out);
                            if let Some(c) = s.drag.as_mut() {
                                c.at = p;
                            }
                            let arm = !out && s.aim(f.w());
                            let carried = s.drag.as_ref().map(|c| c.item.clone());
                            let module = s.module;
                            drop(s);
                            if out {
                                if let (Some((ox, oy)), Some(item)) = (heroui::widgets::popover_offset(f), carried) {
                                    emit(Msg::CarryOut(Some(CarryOut { module, item, at: (ox + ex, oy + ey) })));
                                }
                            } else if was_out {
                                emit(Msg::CarryOut(None));
                            }
                            if arm {
                                arm_folder_merge(&v, &me);
                            }
                            repaint(&mut w);
                        } else if let Some(y0) = s.scrolling {
                            s.scroll_by(f.w(), f.h(), y0 - p.1);
                            s.scrolling = Some(p.1);
                            drop(s);
                            repaint(&mut w);
                        } else if let Some((x0, y0)) = s.down {
                            // Moved before the hold: a scroll (touch), not a pick-up.
                            if (p.0 - x0).abs() > 8 || (p.1 - y0).abs() > 8 {
                                s.pressed = None;
                                s.down = None;
                                s.press_id = s.press_id.wrapping_add(1);
                                s.scrolling = Some(p.1);
                            }
                        }
                        true
                    }
                    Event::Released => {
                        let (drag, out, act, click, module, path) = {
                            let mut s = v.borrow_mut();
                            let drag = s.drag.take();
                            let out = std::mem::take(&mut s.out);
                            let pressed = s.pressed.take();
                            s.down = None;
                            s.scrolling = None;
                            s.press_id = s.press_id.wrapping_add(1);
                            let act = drag.as_ref().filter(|_| !out).and_then(|c| carry_action(&s.items, c, &s.path));
                            let click = pressed.and_then(|(k, b, _)| (b == 1 && s.at(f.w(), p) == Some(k)).then(|| s.items.get(k).cloned()).flatten());
                            (drag, out, act, click, s.module, s.path.clone())
                        };
                        if let Some(c) = drag {
                            if out {
                                if heroui::widgets::popover_offset(f).is_some() {
                                    emit(Msg::CarryDrop);
                                } else if let Some(p) = c.item.pinned.clone().or(c.item.folder.clone()) {
                                    // Where the bar is isn't known: out
                                    // of the folder, next to it.
                                    emit(Msg::TaskAction(module, TaskAction::MoveOut(p)));
                                }
                            } else if let Some(a) = act {
                                emit(Msg::TaskAction(usize::MAX, a));
                            }
                            let _ = path;
                            repaint(&mut w);
                        } else if let Some(item) = click {
                            emit(Msg::FolderEntry(item));
                        }
                        true
                    }
                    _ => false,
                }
            });
        }
        let mut w = f.as_base_widget();
        ctx.bind(move |bar: &Bar| {
            let items = open_items(bar);
            let style = folder_style(bar);
            let (path, module) = bar.folder.as_ref().map_or((vec![], 0), |f| (f.path.clone(), f.module));
            let mut s = v.borrow_mut();
            if s.items == items && s.style == style && s.path == path {
                return;
            }
            if s.path != path {
                s.scroll = 0;
            }
            if let Some(cfg) = bar.modules.get(module).and_then(|m| m.taskbar.as_ref()) {
                s.folders = crate::edit::folders(&cfg.raw);
            }
            s.items = items;
            s.style = style;
            s.path = path;
            s.module = module;
            s.hover.clear();
            s.drag = None;
            s.out = false;
            drop(s);
            repaint(&mut w);
        });
        f.as_base_widget()
    })
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
        // Right-click menus drop down from the button.
        popover_at(
            folder_grid(),
            |b: &Bar| b.menu.as_ref().filter(|m| m.row.is_some()).map(|m| m.rect),
            |b: &Bar| b.menu.as_ref().is_some_and(|m| m.row.is_some()),
            Msg::CloseMenu,
            crate::menu_size,
            crate::menu_view(),
        ),
    ])
    .padding(12)
    .spacing(6)
}

pub fn folder_size(b: &Bar) -> (i32, i32) {
    let n = open_items(b).len().max(1) as i32;
    let around = 12 + 34 + 6 + 12;
    match folder_style(b) {
        FolderStyle::List => (300, around + n.min(8) * (ROW + ROW_GAP) - ROW_GAP),
        FolderStyle::Grid => {
            let cols = n.min(4);
            let rows = ((n + cols - 1) / cols).min(4);
            ((cols * GRID.0 + 24).max(220), around + rows * GRID.1)
        }
        FolderStyle::Icons => {
            let cols = n.min(5);
            let rows = ((n + cols - 1) / cols).min(5);
            ((cols * ICONS.0 + 24).max(200), around + rows * ICONS.1 + NAME_H)
        }
    }
}
