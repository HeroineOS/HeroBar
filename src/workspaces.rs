//! The workspaces module: a button per workspace of the bar's monitor.
//! The shown one is highlighted, ones with windows are brighter than empty
//! ones. Click to switch; the mouse wheel steps through them.
//!
//! One widget with one event handler, like the taskbar; the list comes
//! from the compositor (`windows.rs`), so nothing runs between changes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use heroui::fltk::app;
use heroui::fltk::draw;
use heroui::fltk::enums::{Align, Event, FrameType};
use heroui::fltk::frame::Frame;
use heroui::fltk::prelude::*;
use heroui::prelude::*;
use heroui::widgets::{mix, repaint};

use crate::windows::Ws;
use crate::{Bar, Msg};

const GAP: i32 = 3;
const PAD: i32 = 7;

/// The workspaces to show: all, or (`occupied_only`) those with windows
/// plus the shown one.
pub fn shown(all: &[Ws], occupied_only: bool) -> Vec<Ws> {
    all.iter().filter(|w| !occupied_only || w.occupied || w.active).cloned().collect()
}

struct View {
    list: Vec<Ws>,
    font: i32,
    hover: crate::fade::HoverFade,
    pressed: Option<usize>,
    /// Where the highlight is, as a fractional index: it slides from the
    /// old workspace to the new one.
    slide: heroui::anim::Tween,
}

impl View {
    /// Button widths: at least square, wider for long names.
    fn widths(&self, h: i32) -> Vec<i32> {
        let t = heroui::theme::current();
        draw::set_font(t.font(), self.font);
        let square = (h - 10).max(16);
        self.list.iter().map(|w| (draw::width(&w.label).ceil() as i32 + 2 * PAD).max(square)).collect()
    }

    fn at(&self, x0: i32, h: i32, px: i32) -> Option<usize> {
        let mut x = x0;
        for (i, w) in self.widths(h).iter().enumerate() {
            if px >= x && px < x + w {
                return Some(i);
            }
            x += w + GAP;
        }
        None
    }
}

pub fn view(i: usize, font_size: Option<i32>, in_group: bool) -> Element<Bar, Msg> {
    Element::new(move |ctx| {
        let v = Rc::new(RefCell::new(View { list: vec![], font: 14, hover: Default::default(), pressed: None, slide: heroui::anim::Tween::new(-1.0) }));
        let mut f = Frame::default();
        f.set_frame(FrameType::NoBox);
        {
            let v = v.clone();
            f.draw(move |f| {
                let Ok(v) = v.try_borrow() else { return };
                draw::push_clip(f.x(), f.y(), f.w(), f.h());
                paint(&v, f.x(), f.y(), f.w(), f.h(), in_group);
                draw::pop_clip();
            });
        }
        let emit = ctx.emitter();
        {
            let v = v.clone();
            f.handle(move |f, ev| {
                let px = app::event_x();
                let mut s = v.borrow_mut();
                match ev {
                    Event::Enter | Event::Move => {
                        let h = s.at(f.x(), f.h(), px);
                        s.hover.set(h, &f.as_base_widget());
                        true
                    }
                    Event::Leave => {
                        s.hover.set(None, &f.as_base_widget());
                        true
                    }
                    Event::Push => {
                        s.pressed = s.at(f.x(), f.h(), px);
                        true
                    }
                    Event::Released => {
                        if let Some(idx) = s.pressed.take() {
                            if s.at(f.x(), f.h(), px) == Some(idx) {
                                emit(Msg::Workspace(s.list[idx].id));
                            }
                        }
                        true
                    }
                    Event::MouseWheel => {
                        // Next/previous of the shown list, no wrapping.
                        let cur = s.list.iter().position(|w| w.active);
                        let step = if app::event_dy() == app::MouseWheel::Down { 1 } else { -1 };
                        if let Some(c) = cur {
                            let n = c as i32 + step;
                            if n >= 0 && (n as usize) < s.list.len() {
                                emit(Msg::Workspace(s.list[n as usize].id));
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
        ctx.bind(move |bar: &Bar| {
            let m = &bar.modules[i];
            let occupied_only = m.cfg.show.as_deref() == Some("occupied");
            let list = if bar.gone(i) { vec![] } else { shown(&bar.desktop.workspaces, occupied_only) };
            let mut s = v.borrow_mut();
            if s.list == list && last_width.get() >= 0 {
                return;
            }
            // Same workspaces, another one shown: the highlight slides.
            let ids = |l: &[Ws]| l.iter().map(|w| w.id).collect::<Vec<_>>();
            let old = s.list.iter().position(|w| w.active);
            let new = list.iter().position(|w| w.active);
            match (old, new) {
                (Some(_), Some(n)) if ids(&s.list) == ids(&list) && s.slide.get() >= 0.0 => {
                    let mut w2 = w.clone();
                    s.slide.animate_to(n as f64, std::time::Duration::from_millis(220), move || repaint(&mut w2));
                }
                (_, Some(n)) => s.slide.set(n as f64),
                (_, None) => s.slide.set(-1.0),
            }
            s.list = list;
            s.font = font_size.unwrap_or_else(|| heroui::theme::current().font_size);
            s.hover.clear();
            let ws = s.widths(w.h().max(bar.config.bar.height));
            let width = if ws.is_empty() { 0 } else { ws.iter().sum::<i32>() + GAP * (ws.len() as i32 - 1) + 2 * GAP };
            drop(s);
            if width != last_width.replace(width) {
                crate::fit::set_width(&mut w, width);
            }
            repaint(&mut w);
        });
        f.as_base_widget()
    })
}

fn paint(v: &View, x: i32, y: i32, w: i32, h: i32, in_group: bool) {
    let t = heroui::theme::current();
    if !in_group {
        crate::island(x, y, w, h);
    }
    let m = crate::margin();
    let (by, bh) = (y + m + 2, h - 2 * m - 4);
    draw::set_font(t.font(), v.font);
    let widths = v.widths(h);
    let xs: Vec<i32> = widths.iter().scan(x + GAP, |cx, bw| {
        let here = *cx;
        *cx += bw + GAP;
        Some(here)
    }).collect();
    let r = t.radius.min(bh / 2);
    let under = if in_group || crate::islands_on() { crate::island_color() } else { t.background };
    // Hover backgrounds first, then the sliding highlight over them.
    for idx in 0..v.list.len() {
        let a = v.hover.amount(idx);
        if a > 0.0 {
            draw::set_draw_color(mix(under, t.surface_alt, a));
            draw::draw_rounded_rectf(xs[idx], by, widths[idx], bh, r);
        }
    }
    let f = v.slide.get();
    if f >= 0.0 && !v.list.is_empty() {
        let i0 = (f.floor() as usize).min(v.list.len() - 1);
        let i1 = (f.ceil() as usize).min(v.list.len() - 1);
        let k = (f - f.floor()) as f32;
        let lerp = |a: i32, b: i32| a + ((b - a) as f32 * k).round() as i32;
        draw::set_draw_color(t.accent);
        draw::draw_rounded_rectf(lerp(xs[i0], xs[i1]), by, lerp(widths[i0], widths[i1]), bh, r);
    }
    for (idx, ws) in v.list.iter().enumerate() {
        let base = if ws.occupied { t.text } else { mix(t.text, t.background, 0.55) };
        // Text under the highlight takes its color, as it passes.
        let cover = if f >= 0.0 { (1.0 - (idx as f64 - f).abs()).max(0.0) as f32 } else { 0.0 };
        draw::set_draw_color(mix(base, t.accent_text, cover));
        draw::draw_text2(&ws.label, xs[idx], by, widths[idx], bh, Align::Center);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: u64, active: bool, occupied: bool) -> Ws {
        Ws { id, label: id.to_string(), active, occupied }
    }

    #[test]
    fn occupied_filter() {
        let all = [ws(1, false, true), ws(2, true, false), ws(3, false, false)];
        assert_eq!(shown(&all, false).len(), 3);
        let ids: Vec<u64> = shown(&all, true).iter().map(|w| w.id).collect();
        assert_eq!(ids, [1, 2], "the shown workspace stays even when empty");
    }
}
