//! Connection drawings: the 48-pin ZIF socket (lever at the top left) with
//! the part or adapter in place, or flying leads from the socket to a part
//! on its board.

use eframe::egui::{
    self, pos2, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2,
};
use rt809h_core::connection::{Diagram, Pad, Placement, Wiring};

use crate::theme::{bold, tint, Palette};

const ROWS: usize = 24;

struct Socket {
    body: Rect,
    pitch: f32,
    pad: Vec2,
    col_x: [f32; 2],
}

impl Socket {
    fn new(body: Rect) -> Self {
        let pitch = (body.height() - 16.0) / ROWS as f32;
        Socket {
            body,
            pitch,
            pad: Vec2::new(body.width() * 0.30, (pitch * 0.55).max(2.0)),
            col_x: [
                body.left() + body.width() * 0.28,
                body.left() + body.width() * 0.72,
            ],
        }
    }

    fn center(&self, p: Pad) -> Pos2 {
        pos2(
            self.col_x[p.right as usize],
            self.body.top() + 8.0 + self.pitch * (p.row as f32 - 0.5),
        )
    }

    fn rect(&self, p: Pad) -> Rect {
        Rect::from_center_size(self.center(p), self.pad)
    }

    fn draw(&self, ui: &Ui, p: &Palette, lit: &dyn Fn(Pad) -> bool) {
        let painter = ui.painter();
        let lx = self.body.left() + 7.0;
        painter.line_segment(
            [pos2(lx, self.body.top()), pos2(lx, self.body.top() - 16.0)],
            Stroke::new(2.0, p.dim),
        );
        painter.circle_filled(pos2(lx, self.body.top() - 17.0), 3.0, p.dim);
        painter.rect(
            self.body,
            4,
            tint(p.surface2, p.text, 0.10),
            Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        for right in [false, true] {
            for row in 1..=ROWS as u8 {
                let pad = Pad { right, row };
                let fill = if lit(pad) { p.accent } else { p.surface };
                painter.rect(
                    self.rect(pad),
                    1,
                    fill,
                    Stroke::new(1.0, tint(p.border, p.text, 0.15)),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }
}

/// Draw `d` across the available width.
pub fn draw(ui: &mut Ui, p: &Palette, d: &Diagram) {
    match d {
        Diagram::Socket(pl) => placement(ui, p, pl),
        Diagram::Wires(w) => wiring(ui, p, w),
        Diagram::Text(t) => {
            ui.label(egui::RichText::new(t).color(p.text));
        }
    }
}

fn placement(ui: &mut Ui, p: &Palette, pl: &Placement) {
    let w = ui.available_width();
    let h = (w * 1.05).min(420.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
    let bw = (h * 0.30).min(w * 0.4);
    let body = Rect::from_min_size(
        pos2(rect.center().x - bw / 2.0, rect.top() + 20.0),
        Vec2::new(bw, h - 24.0),
    );
    let s = Socket::new(body);
    let used: Vec<Pad> = (1..=pl.pins).map(|n| pl.pad(n)).collect();
    s.draw(ui, p, &|pad| used.contains(&pad));

    let painter = ui.painter();
    let top = s.center(pl.pad(pl.pins / 2)).y - s.pitch * 0.5;
    let bottom = s.center(pl.pad(1)).y + s.pitch * 0.5;
    let mut chip = Rect::from_min_max(
        pos2(s.col_x[0] + s.pad.x * 0.15, top + 1.0),
        pos2(s.col_x[1] - s.pad.x * 0.15, bottom - 1.0),
    );
    if let Some(adapter) = &pl.adapter {
        let a = Rect::from_min_max(
            pos2(s.body.left() + 2.0, top - 3.0),
            pos2(s.body.right() - 2.0, bottom + 3.0),
        );
        painter.rect(
            a,
            3,
            Color32::from_rgba_unmultiplied(40, 140, 80, 70),
            Stroke::new(1.5, p.ok),
            egui::StrokeKind::Inside,
        );
        // the part on the adapter: square for PLCC, wide for TSOP
        let c = a.center();
        let side = a.width() * 0.62;
        chip = if adapter.starts_with("PLCC") {
            Rect::from_center_size(pos2(c.x, a.bottom() - side * 0.5 - 8.0), Vec2::splat(side))
        } else if adapter.starts_with("TSOP") {
            Rect::from_center_size(
                pos2(c.x, a.bottom() - side * 0.4 - 10.0),
                Vec2::new(side, side * 0.75),
            )
        } else {
            Rect::from_center_size(
                pos2(c.x, a.top() + a.height() * 0.68),
                Vec2::new(side * 0.55, (a.height() * 0.4).min(side * 0.8)),
            )
        };
    }
    painter.rect_filled(chip, 2, Color32::from_gray(38));
    if pl.adapter.is_none() {
        // notch at the bottom
        painter.circle_filled(
            pos2(chip.center().x, chip.bottom()),
            (chip.width() * 0.12).min(6.0),
            tint(p.surface2, p.text, 0.10),
        );
    }
    // pin 1 dot: bottom right
    painter.circle_filled(
        chip.right_bottom() + Vec2::new(-6.0, -7.0),
        2.5,
        Color32::from_gray(200),
    );
    let p1 = s.rect(pl.pad(1)).right_center();
    let tip = pos2(s.body.right() + 26.0, p1.y);
    painter.line_segment([p1 + Vec2::new(2.0, 0.0), tip], Stroke::new(1.5, p.accent));
    painter.text(
        tip + Vec2::new(3.0, 0.0),
        Align2::LEFT_CENTER,
        "pin 1",
        FontId::new(12.0, bold()),
        p.accent,
    );
    let mut t =
        String::from("Bottom of the socket, notch down: pin 1 on the bottom-right contact.");
    if let Some(a) = &pl.adapter {
        t.push_str(&format!(" Use a {a}, its pin 1 at the bottom right too."));
    }
    ui.label(egui::RichText::new(t).size(12.0).color(p.dim));
}

fn wiring(ui: &mut Ui, p: &Palette, wi: &Wiring) {
    let w = ui.available_width();
    let h = (w * 1.0).min(400.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
    let bw = (h * 0.26).min(w * 0.3);
    let body = Rect::from_min_size(
        pos2(rect.left() + 22.0, rect.top() + 20.0),
        Vec2::new(bw, h - 52.0),
    );
    let s = Socket::new(body);
    let used: Vec<Pad> = wi.wires.iter().map(|w| w.0).collect();
    s.draw(ui, p, &|pad| used.contains(&pad));

    // right column first (top contact to top label), then the left column
    // (bottom contact first); this order keeps the leads from crossing
    let mut right: Vec<_> = wi.wires.iter().filter(|w| w.0.right).collect();
    right.sort_by_key(|w| w.0.row);
    let mut left: Vec<_> = wi.wires.iter().filter(|w| !w.0.right).collect();
    left.sort_by_key(|w| std::cmp::Reverse(w.0.row));
    let n = wi.wires.len();

    let box_left = rect.left() + w * 0.62;
    let row_h = 20.0;
    let box_top = rect.top() + 8.0;
    let target = Rect::from_min_max(
        pos2(box_left, box_top),
        pos2(rect.right() - 4.0, box_top + 26.0 + row_h * n as f32 + 6.0),
    );
    let painter = ui.painter();
    painter.rect(
        target,
        6,
        p.surface2,
        Stroke::new(1.5, p.text),
        egui::StrokeKind::Inside,
    );
    painter.text(
        pos2(target.center().x, target.top() + 13.0),
        Align2::CENTER_CENTER,
        wi.target,
        FontId::new(12.0, bold()),
        p.text,
    );
    let lane_lo = s.body.right() + 14.0;
    let lane_hi = box_left - 10.0;
    let lane = |i: usize| lane_lo + (lane_hi - lane_lo) * (i as f32 + 0.5) / n.max(1) as f32;
    let stroke = Stroke::new(2.0, p.accent);
    let box_pin = |i: usize| target.top() + 30.0 + row_h * (i as f32 + 0.5);
    let label = |i: usize, sig: &str| {
        painter.text(
            pos2(box_left + 8.0, box_pin(i)),
            Align2::LEFT_CENTER,
            sig,
            FontId::new(12.0, bold()),
            p.text,
        );
    };
    for (i, (pad, sig)) in right.iter().enumerate() {
        let a = s.rect(*pad).right_center();
        let (x, y) = (lane(i), box_pin(i));
        painter.add(Shape::line(
            vec![a, pos2(x, a.y), pos2(x, y), pos2(box_left, y)],
            stroke,
        ));
        label(i, sig);
    }
    for (j, (pad, sig)) in left.iter().enumerate() {
        let i = right.len() + j;
        let a = s.rect(*pad).left_center();
        let out = s.body.left() - 5.0 - 5.0 * j as f32;
        let under = s.body.bottom() + 6.0 + 6.0 * j as f32;
        let (x, y) = (lane(i), box_pin(i));
        painter.add(Shape::line(
            vec![
                a,
                pos2(out, a.y),
                pos2(out, under),
                pos2(x, under),
                pos2(x, y),
                pos2(box_left, y),
            ],
            stroke,
        ));
        label(i, sig);
    }
    for note in &wi.notes {
        ui.label(egui::RichText::new(*note).size(12.0).color(p.dim));
    }
}
