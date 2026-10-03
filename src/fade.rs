//! Hover fades for widgets with several buttons of their own (taskbar,
//! workspaces): the newly hovered button fades in while the previous one
//! fades out, ~120 ms (instant with animations off).

use heroui::anim::Tween;
use heroui::fltk::prelude::*;
use heroui::fltk::widget::Widget;

pub struct HoverFade {
    pub cur: Option<usize>,
    prev: Option<usize>,
    t: Tween,
}

impl Default for HoverFade {
    fn default() -> Self {
        HoverFade { cur: None, prev: None, t: Tween::new(1.0) }
    }
}

impl HoverFade {
    /// The hovered button is now `new`; `w` is repainted while it fades.
    pub fn set(&mut self, new: Option<usize>, w: &Widget) {
        if new == self.cur {
            return;
        }
        self.prev = self.cur;
        self.cur = new;
        self.t.set(0.0);
        let mut w = w.clone();
        self.t.animate_to(1.0, std::time::Duration::from_millis(120), move || {
            if !w.was_deleted() {
                heroui::widgets::repaint(&mut w);
            }
        });
    }

    /// How hovered button `i` looks, 0.0 to 1.0.
    pub fn amount(&self, i: usize) -> f32 {
        let t = self.t.get() as f32;
        if Some(i) == self.cur {
            t
        } else if Some(i) == self.prev {
            1.0 - t
        } else {
            0.0
        }
    }

    /// Forgets the hover (the list changed).
    pub fn clear(&mut self) {
        self.cur = None;
        self.prev = None;
    }
}
