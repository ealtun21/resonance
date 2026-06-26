//! Painter-drawn vector icons.
//!
//! The whole UI is painter-drawn (see [`crate::ui::kit`]) rather than built from
//! egui's default widgets, and the bundled symbol font carries only a handful of
//! geometric glyphs. Icons that need to read crisply at any size and re-tint with
//! the theme are therefore drawn here as vector paths in a unit box — no font, no
//! raster, sharp at 14 px or 64 px, one stroke weight everywhere.
//!
//! Each icon is defined in a 0..1 coordinate box (origin top-left, y down) and
//! mapped into the caller's `rect`, so the same path serves every call site. Pair
//! an icon with a hover tooltip (the kit's `icon_btn`) for a "glyph + name on
//! hover" control, the house style for compact actions.

use eframe::egui::{self, Color32, Pos2, Shape, Stroke, pos2};

/// The icon set. Add a variant + a `match` arm in [`paths`] to grow it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
    Menu,
    Speaker,
    Folder,
    FolderOpen,
    Save,
    Download,
    Plus,
    Close,
    Trash,
    Refresh,
    Sliders,
    Target,
    Wave,
    Wand,
    Undo,
    Redo,
    Up,
    Home,
    Check,
    Gear,
    Chevron,
    Power,
    Help,
}

/// Every icon with a label — drives the dev gallery (`RESONANCE_ICON_GALLERY=1`).
pub(crate) const ALL: &[(Icon, &str)] = &[
    (Icon::Menu, "Menu"),
    (Icon::Speaker, "Speaker"),
    (Icon::Folder, "Folder"),
    (Icon::FolderOpen, "Folder open"),
    (Icon::Save, "Save"),
    (Icon::Download, "Download"),
    (Icon::Plus, "Plus"),
    (Icon::Close, "Close"),
    (Icon::Trash, "Trash"),
    (Icon::Refresh, "Refresh"),
    (Icon::Sliders, "Sliders"),
    (Icon::Target, "Target"),
    (Icon::Wave, "Wave"),
    (Icon::Wand, "Wand"),
    (Icon::Undo, "Undo"),
    (Icon::Redo, "Redo"),
    (Icon::Up, "Up"),
    (Icon::Home, "Home"),
    (Icon::Check, "Check"),
    (Icon::Gear, "Gear"),
    (Icon::Chevron, "Chevron"),
    (Icon::Power, "Power"),
    (Icon::Help, "Help"),
];

/// Draw `icon` centred in `rect`, tinted `color`. Stroke weight scales with the
/// icon's drawn size so it reads consistently from toolbar (≈14 px) to gallery.
pub(crate) fn draw(painter: &egui::Painter, icon: Icon, rect: egui::Rect, color: Color32) {
    // Square the box (icons are designed square) and centre it in `rect`.
    let side = rect.width().min(rect.height());
    let c = rect.center();
    let r = egui::Rect::from_center_size(c, egui::vec2(side, side));
    let w = (side * 0.085).clamp(1.3, 6.0);
    let pen = Pen {
        painter,
        r,
        w,
        c: color,
    };
    paths(&pen, icon);
}

/// Maps unit-box coordinates into a rect and emits stroked paths/arcs/dots with a
/// single shared stroke. All icon geometry is expressed against this.
struct Pen<'a> {
    painter: &'a egui::Painter,
    r: egui::Rect,
    w: f32,
    c: Color32,
}

impl Pen<'_> {
    fn stroke(&self) -> Stroke {
        Stroke::new(self.w, self.c)
    }
    /// Unit (0..1) → screen.
    fn at(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.r.left() + x * self.r.width(),
            self.r.top() + y * self.r.height(),
        )
    }
    /// A single straight segment.
    fn seg(&self, a: (f32, f32), b: (f32, f32)) {
        self.painter
            .line_segment([self.at(a.0, a.1), self.at(b.0, b.1)], self.stroke());
    }
    /// An open polyline through `pts`.
    fn line(&self, pts: &[(f32, f32)]) {
        let p: Vec<Pos2> = pts.iter().map(|&(x, y)| self.at(x, y)).collect();
        self.painter.add(Shape::line(p, self.stroke()));
    }
    /// A closed polygon outline (stroked, not filled).
    fn poly(&self, pts: &[(f32, f32)]) {
        let mut p: Vec<Pos2> = pts.iter().map(|&(x, y)| self.at(x, y)).collect();
        if let Some(&first) = p.first() {
            p.push(first);
        }
        self.painter.add(Shape::line(p, self.stroke()));
    }
    /// A stroked circle (unit centre + unit radius, radius scaled by box width).
    fn ring(&self, cx: f32, cy: f32, rad: f32) {
        self.painter
            .circle_stroke(self.at(cx, cy), rad * self.r.width(), self.stroke());
    }
    /// A filled dot.
    fn dot(&self, cx: f32, cy: f32, rad: f32) {
        self.painter
            .circle_filled(self.at(cx, cy), rad * self.r.width(), self.c);
    }
    /// A stroked arc, `a0`..`a1` degrees (0° = +x, clockwise in screen space since
    /// y points down), centre + unit radius.
    fn arc(&self, cx: f32, cy: f32, rad: f32, a0: f32, a1: f32) {
        const N: usize = 24;
        let pts: Vec<(f32, f32)> = (0..=N)
            .map(|i| {
                let t = i as f32 / N as f32;
                let a = (a0 + (a1 - a0) * t).to_radians();
                (cx + rad * a.cos(), cy + rad * a.sin())
            })
            .collect();
        self.line(&pts);
    }
    /// A filled triangular arrowhead with its tip at unit point `tip`, pointing
    /// along `dir` (any length), `len` units long. Filled reads far cleaner than
    /// an open chevron at icon sizes.
    fn head(&self, tip: (f32, f32), dir: (f32, f32), len: f32) {
        let m = (dir.0 * dir.0 + dir.1 * dir.1).sqrt().max(1e-4);
        let (dx, dy) = (dir.0 / m, dir.1 / m);
        let (px, py) = (-dy, dx); // perpendicular
        let base = (tip.0 - dx * len, tip.1 - dy * len);
        let w = len * 0.62;
        self.painter.add(Shape::convex_polygon(
            vec![
                self.at(tip.0, tip.1),
                self.at(base.0 + px * w, base.1 + py * w),
                self.at(base.0 - px * w, base.1 - py * w),
            ],
            self.c,
            Stroke::NONE,
        ));
    }
    /// Tip + tangent direction of an arc end at `ang` degrees (radius `rad`,
    /// centre `cx,cy`). `cw` = the arc was drawn clockwise (increasing angle).
    fn arc_end(&self, cx: f32, cy: f32, rad: f32, ang: f32, cw: bool) -> ((f32, f32), (f32, f32)) {
        let a = ang.to_radians();
        let tip = (cx + rad * a.cos(), cy + rad * a.sin());
        // d/dθ(cos,sin) = (-sin, cos); reverse it for a counter-clockwise sweep.
        let dir = if cw {
            (-a.sin(), a.cos())
        } else {
            (a.sin(), -a.cos())
        };
        (tip, dir)
    }
}

/// The geometry of each icon, in the unit box.
fn paths(p: &Pen, icon: Icon) {
    match icon {
        Icon::Menu => {
            for y in [0.30, 0.50, 0.70] {
                p.seg((0.18, y), (0.82, y));
            }
        }
        Icon::Speaker => {
            // Cone (closed) + two sound arcs to the right.
            p.poly(&[
                (0.16, 0.40),
                (0.30, 0.40),
                (0.46, 0.24),
                (0.46, 0.76),
                (0.30, 0.60),
                (0.16, 0.60),
            ]);
            p.arc(0.46, 0.50, 0.16, -48.0, 48.0);
            p.arc(0.46, 0.50, 0.30, -42.0, 42.0);
        }
        Icon::Folder => {
            // Closed folder (a pressable "load" target): body + tab.
            p.poly(&[
                (0.16, 0.74),
                (0.16, 0.36),
                (0.40, 0.36),
                (0.46, 0.44),
                (0.84, 0.44),
                (0.84, 0.74),
            ]);
        }
        Icon::FolderOpen => {
            // Open folder (the currently-loaded one): back panel + an angled front
            // flap tilting open.
            p.line(&[
                (0.16, 0.72),
                (0.16, 0.34),
                (0.38, 0.34),
                (0.45, 0.43),
                (0.82, 0.43),
                (0.82, 0.52),
            ]);
            p.poly(&[(0.16, 0.72), (0.27, 0.52), (0.92, 0.52), (0.81, 0.72)]);
        }
        Icon::Save => {
            // Floppy: body with a cut top-right corner, shutter + label slots.
            p.poly(&[
                (0.20, 0.20),
                (0.66, 0.20),
                (0.80, 0.34),
                (0.80, 0.80),
                (0.20, 0.80),
            ]);
            p.poly(&[(0.34, 0.20), (0.34, 0.36), (0.60, 0.36), (0.60, 0.20)]);
            p.poly(&[(0.32, 0.80), (0.32, 0.56), (0.68, 0.56), (0.68, 0.80)]);
        }
        Icon::Download => {
            p.seg((0.50, 0.20), (0.50, 0.62));
            p.line(&[(0.34, 0.46), (0.50, 0.64), (0.66, 0.46)]);
            p.seg((0.22, 0.80), (0.78, 0.80));
        }
        Icon::Plus => {
            p.seg((0.50, 0.22), (0.50, 0.78));
            p.seg((0.22, 0.50), (0.78, 0.50));
        }
        Icon::Close => {
            p.seg((0.27, 0.27), (0.73, 0.73));
            p.seg((0.73, 0.27), (0.27, 0.73));
        }
        Icon::Trash => {
            p.seg((0.20, 0.30), (0.80, 0.30));
            p.line(&[(0.40, 0.30), (0.42, 0.22), (0.58, 0.22), (0.60, 0.30)]);
            p.line(&[(0.28, 0.30), (0.32, 0.80), (0.68, 0.80), (0.72, 0.30)]);
            p.seg((0.43, 0.40), (0.44, 0.71));
            p.seg((0.57, 0.40), (0.56, 0.71));
        }
        Icon::Refresh => {
            // A near-full circular arrow (small gap top-right) with one filled
            // head — the universal "reload".
            let (cx, cy, r) = (0.50, 0.50, 0.30);
            p.arc(cx, cy, r, 300.0, 600.0); // 300°→240° (wraps), ~300° clockwise
            let (tip, dir) = p.arc_end(cx, cy, r, 600.0, true);
            p.head(tip, dir, 0.20);
        }
        Icon::Sliders => {
            // Mixer: three tracks, each with a knob at a different position.
            let rows = [(0.30, 0.64), (0.50, 0.36), (0.70, 0.56)];
            for (y, kx) in rows {
                p.seg((0.18, y), (0.82, y));
                p.dot(kx, y, 0.085);
            }
        }
        Icon::Target => {
            p.ring(0.50, 0.50, 0.30);
            p.ring(0.50, 0.50, 0.15);
            p.dot(0.50, 0.50, 0.05);
        }
        Icon::Wave => {
            let pts: Vec<(f32, f32)> = (0..=28)
                .map(|i| {
                    let t = i as f32 / 28.0;
                    let x = 0.16 + 0.68 * t;
                    let y = 0.50 - 0.22 * (t * std::f32::consts::TAU * 1.15).sin();
                    (x, y)
                })
                .collect();
            p.line(&pts);
        }
        Icon::Wand => {
            // A magic wand: a stick bottom-left → top-right, with a four-point
            // sparkle (long axes + short diagonals) at the tip → reads as "auto".
            p.seg((0.22, 0.82), (0.58, 0.46));
            let (sx, sy) = (0.70, 0.28);
            p.seg((sx, sy - 0.16), (sx, sy + 0.16));
            p.seg((sx - 0.16, sy), (sx + 0.16, sy));
            p.seg((sx - 0.08, sy - 0.08), (sx + 0.08, sy + 0.08));
            p.seg((sx - 0.08, sy + 0.08), (sx + 0.08, sy - 0.08));
        }
        Icon::Undo => {
            // ↶ — arc over the TOP (y is down, so the top is 180°→360°), sweeping
            // right→top→left, with the head at the left end pointing down.
            let (cx, cy, r) = (0.50, 0.55, 0.27);
            p.arc(cx, cy, r, 380.0, 180.0); // decreasing = counter-clockwise
            let (tip, dir) = p.arc_end(cx, cy, r, 180.0, false);
            p.head(tip, dir, 0.18);
        }
        Icon::Redo => {
            // ↷ — mirror of Undo: left→top→right, head at the right end.
            let (cx, cy, r) = (0.50, 0.55, 0.27);
            p.arc(cx, cy, r, 160.0, 360.0); // increasing = clockwise
            let (tip, dir) = p.arc_end(cx, cy, r, 360.0, true);
            p.head(tip, dir, 0.18);
        }
        Icon::Up => {
            p.seg((0.50, 0.80), (0.50, 0.24));
            p.line(&[(0.30, 0.46), (0.50, 0.24), (0.70, 0.46)]);
        }
        Icon::Home => {
            p.line(&[(0.16, 0.50), (0.50, 0.22), (0.84, 0.50)]);
            p.line(&[(0.26, 0.46), (0.26, 0.80), (0.74, 0.80), (0.74, 0.46)]);
            p.poly(&[(0.43, 0.80), (0.43, 0.60), (0.57, 0.60), (0.57, 0.80)]);
        }
        Icon::Check => p.line(&[(0.24, 0.52), (0.43, 0.70), (0.76, 0.32)]),
        Icon::Gear => {
            // Eight teeth radiating from a ring, plus a hub.
            for k in 0..8 {
                let a = (k as f32 * 45.0).to_radians();
                let (dx, dy) = (a.cos(), a.sin());
                p.seg(
                    (0.50 + dx * 0.24, 0.50 + dy * 0.24),
                    (0.50 + dx * 0.36, 0.50 + dy * 0.36),
                );
            }
            p.ring(0.50, 0.50, 0.22);
            p.dot(0.50, 0.50, 0.07);
        }
        Icon::Chevron => p.line(&[(0.32, 0.42), (0.50, 0.60), (0.68, 0.42)]),
        Icon::Power => {
            p.seg((0.50, 0.18), (0.50, 0.46));
            p.arc(0.50, 0.52, 0.28, -60.0, 240.0);
        }
        Icon::Help => {
            // A "?" inside a ring.
            p.ring(0.50, 0.50, 0.36);
            p.line(&[
                (0.38, 0.40),
                (0.43, 0.32),
                (0.56, 0.32),
                (0.62, 0.40),
                (0.58, 0.49),
                (0.50, 0.54),
                (0.50, 0.61),
            ]);
            p.dot(0.50, 0.71, 0.045);
        }
    }
}

/// Dev gallery: paint every icon in a labelled grid. Gated behind
/// `RESONANCE_ICON_GALLERY=1` in `main` so the shapes can be eyeballed via the
/// screenshot harness without wiring each into the real UI first.
pub(crate) fn gallery(ui: &mut egui::Ui) {
    let text = ui.visuals().text_color();
    ui.heading("Icon gallery");
    ui.add_space(8.0);
    let cell = 84.0;
    let glyph = 40.0;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (icon, label) in ALL {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(cell, cell), egui::Sense::hover());
                let g = egui::Rect::from_center_size(
                    pos2(rect.center().x, rect.top() + glyph * 0.5 + 8.0),
                    egui::vec2(glyph, glyph),
                );
                ui.painter()
                    .rect_filled(g.expand(8.0), 6.0, ui.visuals().faint_bg_color);
                draw(ui.painter(), *icon, g, text);
                ui.painter().text(
                    pos2(rect.center().x, rect.bottom() - 12.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(11.0),
                    text,
                );
            }
        });
    });
}
