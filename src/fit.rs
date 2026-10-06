//! Sizing modules in the bar's rows.
//!
//! Every module has a width it wants (its content); [`set_width`] gives
//! it that in its row. Rows marked [`content_sized`] (the center section,
//! groups) are as wide as what's in them, so a change inside one resizes
//! it in its own row, and so on up. Widgets marked [`flexible`] (the
//! sections' edge space, expanding spacers) share what's left of a row
//! equally, which is what centers a module between two of them.
//!
//! Width changes spring (~0.3 s) once [`animate`] is on: a module that
//! appears grows from nothing, one that changes slides to its new size,
//! and its neighbors move along. Widths are remembered by module name, so
//! after the bar is rebuilt (a config change) modules continue from where
//! they were instead of jumping.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use heroui::fltk::group::Flex;
use heroui::fltk::prelude::*;
use heroui::fltk::widget::Widget;

thread_local! {
    /// Current widths, by widget pointer.
    static WIDTHS: RefCell<HashMap<usize, i32>> = RefCell::new(HashMap::new());
    /// Module names, by widget pointer, and their last widths by name.
    static NAMES: RefCell<HashMap<usize, String>> = RefCell::new(HashMap::new());
    static LAST: RefCell<HashMap<String, i32>> = RefCell::new(HashMap::new());
    /// Running width animations, by widget pointer.
    static TWEENS: RefCell<HashMap<usize, heroui::anim::Tween>> = RefCell::new(HashMap::new());
    static ANIMATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CONTENT_SIZED: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    static FLEXIBLE: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

fn key<W: WidgetExt>(w: &W) -> usize {
    w.as_widget_ptr() as usize
}

/// `f` is as wide as its visible children (plus the gaps).
pub fn content_sized(f: &Flex) {
    CONTENT_SIZED.with(|s| s.borrow_mut().insert(key(f)));
}

/// `w` shares its row's free space with the other flexible widgets.
pub fn flexible<W: WidgetExt>(w: &W) {
    FLEXIBLE.with(|s| s.borrow_mut().insert(key(w)));
}

pub fn is_flexible<W: WidgetExt>(w: &W) -> bool {
    FLEXIBLE.with(|s| s.borrow().contains(&key(w)))
}

pub fn is_content_sized<W: WidgetExt>(w: &W) -> bool {
    CONTENT_SIZED.with(|s| s.borrow().contains(&key(w)))
}

/// Width changes animate from now on (off while the bar first lays out).
pub fn animate(on: bool) {
    ANIMATE.with(|a| a.set(on));
}

/// `w` shows module `name` (its width carries over a rebuild).
pub fn name<W: WidgetExt>(w: &W, name: &str) {
    NAMES.with(|n| n.borrow_mut().insert(key(w), name.to_owned()));
}

/// The width module `name` had (0 if it wasn't shown).
pub fn last_width(name: &str) -> i32 {
    LAST.with(|l| l.borrow().get(name).copied().unwrap_or(0))
}

/// Gives `w` `width` px in its row (0 hides it), then resizes the rows
/// around it that follow their content. Animated when [`animate`] is on.
pub fn set_width(w: &mut Widget, width: i32) {
    let k = key(w);
    let current = WIDTHS.with(|m| m.borrow().get(&k).copied());
    let name = NAMES.with(|n| n.borrow().get(&k).cloned());
    // Where it starts from: its width now, or (new widget) what the module
    // had before the rebuild, or nothing (a new module grows).
    let from = current.or_else(|| name.as_deref().map(last_width)).unwrap_or(0);
    if !ANIMATE.with(|a| a.get()) || !heroui::anim::enabled() || from == width {
        TWEENS.with(|t| t.borrow_mut().remove(&k));
        apply(w, width);
        return;
    }
    if current.is_none() {
        apply(w, from);
    }
    let tween = TWEENS.with(|t| {
        t.borrow_mut().entry(k).or_insert_with(|| heroui::anim::Tween::new(from as f64)).clone()
    });
    let mut w2 = w.clone();
    let t2 = tween.clone();
    // A spring: a new width mid-move carries on from the current speed.
    tween.spring_to(width as f64, WIDTH_SPRING, move || {
        if w2.was_deleted() {
            return;
        }
        let v = t2.get().round().max(0.0) as i32;
        apply(&mut w2, v);
        if t2.velocity() == 0.0 {
            TWEENS.with(|t| t.borrow_mut().remove(&key(&w2)));
        }
    });
}

/// How widths move: quick, settling with a barely visible overshoot.
pub const WIDTH_SPRING: heroui::anim::Spring = heroui::anim::Spring { response: 0.3, damping: 0.85 };

fn apply(w: &mut Widget, width: i32) {
    WIDTHS.with(|m| m.borrow_mut().insert(key(w), width));
    if let Some(n) = NAMES.with(|n| n.borrow().get(&key(w)).cloned()) {
        LAST.with(|l| l.borrow_mut().insert(n, width));
    }
    if width == 0 {
        w.hide();
    } else {
        w.show();
    }
    let Some(parent) = w.parent() else { return };
    let Some(mut flex) = Flex::from_dyn_widget(&parent) else { return };
    flex.fixed(&*w, width);
    flex.recalc();
    // Repaint the whole row, background included: space a module gave up
    // must not keep showing it.
    heroui::widgets::repaint(&mut flex);
    if is_content_sized(&flex) {
        // Follows its content frame by frame (no animation of its own).
        let total = content_width(&flex);
        let mut fw = flex.as_base_widget();
        apply(&mut fw, total);
    }
    heroui::relayout_parent(&flex);
}

/// What a content-sized row needs: its visible children's wanted widths
/// and the gaps between them.
fn content_width(f: &Flex) -> i32 {
    let mut total = 0;
    let mut shown = 0;
    for k in 0..f.children() {
        let Some(c) = f.child(k) else { continue };
        if !c.visible() {
            continue;
        }
        shown += 1;
        total += WIDTHS.with(|m| m.borrow().get(&key(&c)).copied()).unwrap_or(0);
    }
    if shown == 0 {
        0
    } else {
        total + f.pad() * (shown - 1)
    }
}

/// Starts over for a new view (the bar is rebuilt): everything but the
/// remembered widths by name, since new widgets can reuse the old ones'
/// addresses.
pub fn forget_widgets() {
    WIDTHS.with(|m| m.borrow_mut().clear());
    NAMES.with(|n| n.borrow_mut().clear());
    TWEENS.with(|t| t.borrow_mut().clear());
    CONTENT_SIZED.with(|s| s.borrow_mut().clear());
    FLEXIBLE.with(|s| s.borrow_mut().clear());
}

/// The most `w` can have in its row: the row minus the other fixed
/// widgets and the gaps (flexible space gives way). None before the bar
/// is laid out, or in a row sized to its content.
pub fn room(w: &Widget) -> Option<i32> {
    let parent = Flex::from_dyn_widget(&w.parent()?)?;
    if parent.w() <= 0 || is_content_sized(&parent) {
        return None;
    }
    let mut used = 0;
    let mut shown = 0;
    for k in 0..parent.children() {
        let Some(c) = parent.child(k) else { continue };
        if !c.visible() {
            continue;
        }
        shown += 1;
        if key(&c) != key(w) && !is_flexible(&c) {
            used += c.w();
        }
    }
    Some((parent.w() - used - parent.pad() * (shown - 1).max(0)).max(0))
}
