//! Sizing modules in the bar's rows.
//!
//! Every module has a width it wants (its content); [`set_width`] gives
//! it that in its row. Rows marked [`content_sized`] (the center section,
//! groups) are as wide as what's in them, so a change inside one resizes
//! it in its own row, and so on up. Widgets marked [`flexible`] (the
//! sections' edge space, expanding spacers) share what's left of a row
//! equally, which is what centers a module between two of them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use heroui::fltk::group::Flex;
use heroui::fltk::prelude::*;
use heroui::fltk::widget::Widget;

thread_local! {
    /// Wanted widths, by widget pointer.
    static WIDTHS: RefCell<HashMap<usize, i32>> = RefCell::new(HashMap::new());
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

/// Gives `w` `width` px in its row (0 hides it), then resizes the rows
/// around it that follow their content.
pub fn set_width(w: &mut Widget, width: i32) {
    WIDTHS.with(|m| m.borrow_mut().insert(key(w), width));
    if width == 0 {
        w.hide();
    } else {
        w.show();
    }
    let Some(parent) = w.parent() else { return };
    let Some(mut flex) = Flex::from_dyn_widget(&parent) else { return };
    flex.fixed(&*w, width);
    flex.recalc();
    if is_content_sized(&flex) {
        let total = content_width(&flex);
        let mut fw = flex.as_base_widget();
        set_width(&mut fw, total);
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
