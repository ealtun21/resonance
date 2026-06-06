//! Brand icon, shared by the window/taskbar icon (rasterised in `main.rs`) and
//! the in-app titlebar logo (drawn with the egui painter). Both mirror
//! `contrib/io.github.ealtun21.Resonance.svg` so the app reads the same
//! everywhere: a rounded purple tile, five teal→purple EQ bars, and a white
//! response curve overlaid.

use eframe::egui;

// Brand colours straight from the SVG gradients.
const BG_TOP: [u8; 3] = [0x2b, 0x22, 0x40];
const BG_BOT: [u8; 3] = [0x15, 0x10, 0x1f];
const BAR_TOP: [u8; 3] = [0x18, 0xe0, 0xd8]; // teal (bar top)
const BAR_BOT: [u8; 3] = [0x7c, 0x4d, 0xff]; // purple (bar bottom)

// Geometry in the SVG's 128×128 viewBox.
const BG: (f32, f32, f32, f32) = (4.0, 4.0, 120.0, 120.0);
const BG_RX: f32 = 26.0;
const BARS: [(f32, f32, f32, f32); 5] = [
    (24.0, 62.0, 12.0, 42.0),
    (42.0, 44.0, 12.0, 60.0),
    (60.0, 28.0, 12.0, 76.0),
    (78.0, 50.0, 12.0, 54.0),
    (96.0, 70.0, 12.0, 34.0),
];

fn lerp3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
    ]
}

fn cubic(p: [(f32, f32); 4], t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        a * p[0].0 + b * p[1].0 + c * p[2].0 + d * p[3].0,
        a * p[0].1 + b * p[1].1 + c * p[2].1 + d * p[3].1,
    )
}

/// Sampled points of the SVG response curve in 128-space.
/// Path: `M16 86 C40 86,50 40,64 40 S92 78,112 56`.
fn curve_pts() -> Vec<(f32, f32)> {
    let seg1 = [(16.0, 86.0), (40.0, 86.0), (50.0, 40.0), (64.0, 40.0)];
    // Smooth cubic: first control is the reflection of seg1's last control.
    let seg2 = [(64.0, 40.0), (78.0, 40.0), (92.0, 78.0), (112.0, 56.0)];
    let mut v = Vec::with_capacity(50);
    for seg in [seg1, seg2] {
        for i in 0..=24 {
            v.push(cubic(seg, i as f32 / 24.0));
        }
    }
    v
}

// ── egui painter (titlebar logo) ─────────────────────────────────────────────

/// Paint the brand logo into `rect` with the egui painter (used by the titlebar).
pub fn paint(painter: &egui::Painter, rect: egui::Rect) {
    // Square sub-rect, centred, so the icon keeps its aspect ratio.
    let s = rect.width().min(rect.height());
    let o = egui::pos2(rect.center().x - s * 0.5, rect.center().y - s * 0.5);
    let map = |x: f32, y: f32| egui::pos2(o.x + x / 128.0 * s, o.y + y / 128.0 * s);
    let col = |c: [u8; 3]| egui::Color32::from_rgb(c[0], c[1], c[2]);

    // Rounded background (solid mid-purple; the SVG's subtle gradient is lost on
    // a tiny logo anyway).
    let bg = egui::Rect::from_min_max(map(BG.0, BG.1), map(BG.0 + BG.2, BG.1 + BG.3));
    painter.rect_filled(bg, BG_RX / 128.0 * s, col(lerp3(BG_TOP, BG_BOT, 0.5)));

    // Bars with a vertical teal→purple gradient via per-vertex mesh colours.
    for (x, y, w, h) in BARS {
        let top = map(x, y);
        let bot = map(x + w, y + h);
        let mut mesh = egui::Mesh::default();
        let tl = col(BAR_TOP);
        let bl = col(BAR_BOT);
        let i = mesh.vertices.len() as u32;
        for (pos, c) in [
            (egui::pos2(top.x, top.y), tl),
            (egui::pos2(bot.x, top.y), tl),
            (egui::pos2(bot.x, bot.y), bl),
            (egui::pos2(top.x, bot.y), bl),
        ] {
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: egui::epaint::WHITE_UV,
                color: c,
            });
        }
        mesh.indices
            .extend_from_slice(&[i, i + 1, i + 2, i, i + 2, i + 3]);
        painter.add(egui::Shape::mesh(mesh));
    }

    // White response curve.
    let pts: Vec<egui::Pos2> = curve_pts().into_iter().map(|(x, y)| map(x, y)).collect();
    let stroke = egui::Stroke::new((s * 0.04).max(1.0), egui::Color32::from_white_alpha(217));
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], stroke);
    }
}

// ── raster (window / taskbar icon) ───────────────────────────────────────────

/// Rasterise the brand icon to RGBA at `size`×`size` for the OS window icon.
pub fn rgba(size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size * size * 4];
    let sc = size as f32 / 128.0;
    let put = |buf: &mut [u8], x: usize, y: usize, c: [u8; 3], a: u8| {
        let i = (y * size + x) * 4;
        buf[i] = c[0];
        buf[i + 1] = c[1];
        buf[i + 2] = c[2];
        buf[i + 3] = a;
    };

    // Background (rounded rect) with a vertical gradient + the bars on top.
    for py in 0..size {
        for px in 0..size {
            // Pixel centre in 128-space.
            let x = (px as f32 + 0.5) / sc;
            let y = (py as f32 + 0.5) / sc;
            if !in_rounded(x, y) {
                continue;
            }
            // Default: background gradient.
            let bt = ((y - BG.1) / BG.3).clamp(0.0, 1.0);
            let mut color = lerp3(BG_TOP, BG_BOT, bt);
            // Bars override where they cover.
            for (bx, by, bw, bh) in BARS {
                if x >= bx && x < bx + bw && y >= by && y < by + bh {
                    let t = (y - by) / bh; // 0 top → teal, 1 bottom → purple
                    color = lerp3(BAR_TOP, BAR_BOT, t);
                }
            }
            put(&mut buf, px, py, color, 255);
        }
    }

    // White curve: stamp discs along the sampled path.
    let r = (3.0 * sc).max(1.0);
    let r2 = r * r;
    let pts = curve_pts();
    for w in pts.windows(2) {
        // Walk the segment in small steps so the stroke is continuous.
        let steps = 6;
        for k in 0..=steps {
            let f = k as f32 / steps as f32;
            let cx = (w[0].0 + (w[1].0 - w[0].0) * f) * sc;
            let cy = (w[0].1 + (w[1].1 - w[0].1) * f) * sc;
            let (x0, x1) = ((cx - r) as i32, (cx + r) as i32);
            let (y0, y1) = ((cy - r) as i32, (cy + r) as i32);
            for py in y0..=y1 {
                for px in x0..=x1 {
                    if px < 0 || py < 0 || px as usize >= size || py as usize >= size {
                        continue;
                    }
                    let dx = px as f32 + 0.5 - cx;
                    let dy = py as f32 + 0.5 - cy;
                    if dx * dx + dy * dy <= r2 {
                        put(&mut buf, px as usize, py as usize, [255, 255, 255], 255);
                    }
                }
            }
        }
    }

    buf
}

/// Rounded-rect membership test in 128-space (matches `BG` + `BG_RX`).
fn in_rounded(x: f32, y: f32) -> bool {
    let (rx0, ry0, rx1, ry1) = (BG.0, BG.1, BG.0 + BG.2, BG.1 + BG.3);
    if x < rx0 || x > rx1 || y < ry0 || y > ry1 {
        return false;
    }
    let r = BG_RX;
    // Nearest corner centre.
    let cx = if x < rx0 + r {
        rx0 + r
    } else if x > rx1 - r {
        rx1 - r
    } else {
        x
    };
    let cy = if y < ry0 + r {
        ry0 + r
    } else if y > ry1 - r {
        ry1 - r
    } else {
        y
    };
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

#[cfg(test)]
mod tests {
    /// Set `ICON_DUMP=1` to write the raster to `/tmp/icon.rgba` for eyeballing.
    #[test]
    fn dump() {
        if std::env::var("ICON_DUMP").is_ok() {
            std::fs::write("/tmp/icon.rgba", super::rgba(256)).unwrap();
        }
    }
}
