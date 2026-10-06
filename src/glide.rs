//! Buttons that glide: a widget drawing a row of its own buttons (the
//! taskbar, a folder) asks [`Glides::place`] where each one is drawn this
//! frame. When its spot moves (reordering, a drag making room, the bar
//! resizing), the button springs there instead of jumping; a new one pops
//! in; one that's gone leaves a ghost that shrinks and fades while the
//! others close the gap. Nothing runs while nothing moves.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use heroui::anim::{Spring, Tween};
use heroui::fltk::prelude::WidgetExt;
use heroui::fltk::widget::Widget;
use heroui::widgets::repaint;

/// Moves: quick, with a hint of bounce.
const MOVE: Spring = Spring { response: 0.3, damping: 0.8 };

struct Slot<T> {
    x: Tween,
    y: Tween,
    w: Tween,
    /// Where it's heading, to notice a new spot.
    target: Cell<(i32, i32, i32)>,
    /// 0 → 1 as it pops in.
    appear: Tween,
    /// Placed in the current frame.
    seen: Cell<bool>,
    item: RefCell<T>,
}

pub struct Ghost<T> {
    pub item: T,
    /// Where it was: (x, y, w).
    pub at: (i32, i32, i32),
    /// 1 → 0 as it goes.
    pub fade: Tween,
}

pub struct Glides<T> {
    slots: RefCell<HashMap<String, Slot<T>>>,
    ghosts: RefCell<Vec<Ghost<T>>>,
    /// Anything placed yet: the first layout doesn't animate.
    started: Cell<bool>,
    widget: RefCell<Option<Widget>>,
}

impl<T: Clone + 'static> Default for Glides<T> {
    fn default() -> Self {
        Glides { slots: Default::default(), ghosts: Default::default(), started: Cell::new(false), widget: RefCell::new(None) }
    }
}

impl<T: Clone + 'static> Glides<T> {
    /// Starts a frame (before the `place` calls) for widget `w`.
    pub fn begin(&self, w: &Widget) {
        if self.widget.borrow().is_none() {
            *self.widget.borrow_mut() = Some(w.clone());
        }
        for s in self.slots.borrow().values() {
            s.seen.set(false);
        }
    }

    fn redraw(&self) -> impl FnMut() + 'static {
        let w = self.widget.borrow().clone();
        move || {
            if let Some(mut w) = w.clone() {
                if !w.was_deleted() {
                    repaint(&mut w);
                }
            }
        }
    }

    /// Where button `key` is drawn now (x, y, w) on its way to `target`,
    /// and how far it has popped in (0..1). `item` is what a ghost of it
    /// would draw.
    pub fn place(&self, key: &str, target: (i32, i32, i32), item: &T) -> ((i32, i32, i32), f64) {
        let mut slots = self.slots.borrow_mut();
        let animate = self.started.get() && heroui::anim::enabled();
        let slot = slots.entry(key.to_owned()).or_insert_with(|| {
            let appear = Tween::new(if animate { 0.0 } else { 1.0 });
            if animate {
                appear.animate_ease(1.0, std::time::Duration::from_millis(280), heroui::anim::snappy, self.redraw());
            }
            Slot {
                x: Tween::new(target.0 as f64),
                y: Tween::new(target.1 as f64),
                w: Tween::new(target.2 as f64),
                target: Cell::new(target),
                appear,
                seen: Cell::new(false),
                item: RefCell::new(item.clone()),
            }
        });
        slot.seen.set(true);
        *slot.item.borrow_mut() = item.clone();
        if slot.target.replace(target) != target {
            if animate {
                slot.x.spring_to(target.0 as f64, MOVE, self.redraw());
                slot.y.spring_to(target.1 as f64, MOVE, self.redraw());
                slot.w.spring_to(target.2 as f64, MOVE, self.redraw());
            } else {
                slot.x.set(target.0 as f64);
                slot.y.set(target.1 as f64);
                slot.w.set(target.2 as f64);
            }
        }
        let r = |t: &Tween| t.get().round() as i32;
        ((r(&slot.x), r(&slot.y), r(&slot.w).max(1)), slot.appear.get())
    }

    /// Button `key` isn't drawn this frame but isn't gone (it's being
    /// carried).
    pub fn keep(&self, key: &str) {
        if let Some(s) = self.slots.borrow().get(key) {
            s.seen.set(true);
        }
    }

    /// Ends a frame: buttons not placed in it are gone; they become ghosts
    /// (shrinking, fading). Then `paint(item, (x, y, w), amount)` draws
    /// each ghost still fading.
    pub fn end(&self, mut paint: impl FnMut(&T, (i32, i32, i32), f64)) {
        let animate = self.started.replace(true) && heroui::anim::enabled();
        let gone: Vec<(String, Slot<T>)> = {
            let mut slots = self.slots.borrow_mut();
            let keys: Vec<String> = slots.iter().filter(|(_, s)| !s.seen.get()).map(|(k, _)| k.clone()).collect();
            keys.into_iter().filter_map(|k| slots.remove(&k).map(|s| (k, s))).collect()
        };
        for (_, s) in gone {
            if !animate {
                continue;
            }
            let fade = Tween::new(1.0);
            fade.animate_ease(0.0, std::time::Duration::from_millis(200), heroui::anim::ease_in, self.redraw());
            let r = |t: &Tween| t.get().round() as i32;
            self.ghosts.borrow_mut().push(Ghost { item: s.item.into_inner(), at: (r(&s.x), r(&s.y), r(&s.w)), fade });
        }
        self.ghosts.borrow_mut().retain(|g| g.fade.get() > 0.0);
        for g in self.ghosts.borrow().iter() {
            paint(&g.item, g.at, g.fade.get());
        }
    }
}
