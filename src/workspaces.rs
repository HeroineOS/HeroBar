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
    hover: Option<usize>,
    pressed: Option<usize>,
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
        let v = Rc::new(RefCell::new(View { list: vec![], font: 14, hover: None, pressed: None }));
        let mut f = Frame::default();
        f.set_frame(FrameType::NoBox);
        {
            let v = v.clone();
            f.draw(move |f| paint(&v.borrow(), f.x(), f.y(), f.w(), f.h(), in_group));
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
            let list = if bar.hidden(i) { vec![] } else { shown(&bar.desktop.workspaces, occupied_only) };
            let mut s = v.borrow_mut();
            if s.list == list && last_width.get() >= 0 {
                return;
            }
            s.list = list;
            s.font = font_size.unwrap_or_else(|| heroui::theme::current().font_size);
            s.hover = None;
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
    let mut bx = x + GAP;
    for (idx, (ws, bw)) in v.list.iter().zip(v.widths(h)).enumerate() {
        let r = t.radius.min(bh / 2);
        if ws.active {
            draw::set_draw_color(t.accent);
            draw::draw_rounded_rectf(bx, by, bw, bh, r);
        } else if v.hover == Some(idx) {
            draw::set_draw_color(t.surface_alt);
            draw::draw_rounded_rectf(bx, by, bw, bh, r);
        }
        draw::set_draw_color(if ws.active {
            t.accent_text
        } else if ws.occupied {
            t.text
        } else {
            mix(t.text, t.background, 0.55)
        });
        draw::draw_text2(&ws.label, bx, by, bw, bh, Align::Center);
        bx += bw + GAP;
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
