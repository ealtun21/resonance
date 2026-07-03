//! Tray icon pixels. A single committed PNG is decoded to RGBA at startup; the
//! "bypassed" and macOS "template" variants are derived in-process, so we ship
//! one asset and carry no runtime SVG rasterizer.
//!
//! Backend wiring (calling these from `main`) lands in Task 12; until then
//! nothing but the inline tests exercises this module, so the module-level
//! allow keeps that from tripping `-D warnings`.
#![allow(dead_code)]

const ICON_PNG: &[u8] = include_bytes!("../assets/icon-64.png");

pub struct IconRgba {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

fn decode() -> IconRgba {
    let img = image::load_from_memory(ICON_PNG)
        .expect("embedded tray icon PNG must decode")
        .to_rgba8();
    let (width, height) = img.dimensions();
    IconRgba {
        width,
        height,
        rgba: img.into_raw(),
    }
}

#[must_use]
pub fn active() -> IconRgba {
    decode()
}

/// Desaturated + dimmed copy (color only; alpha preserved) for the bypassed state.
#[must_use]
pub fn bypassed() -> IconRgba {
    let mut icon = decode();
    for px in icon.rgba.chunks_exact_mut(4) {
        // Rec.601 luma, then dim to 60%.
        let luma =
            (0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2])) * 0.6;
        let v = luma.round().clamp(0.0, 255.0) as u8;
        px[0] = v;
        px[1] = v;
        px[2] = v;
    }
    icon
}

/// macOS template image: pure black, original alpha — the OS tints it for the
/// current menu-bar appearance.
#[must_use]
pub fn template() -> IconRgba {
    let mut icon = decode();
    for px in icon.rgba.chunks_exact_mut(4) {
        px[0] = 0;
        px[1] = 0;
        px[2] = 0;
    }
    icon
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_decodes_to_square_rgba() {
        let a = active();
        assert_eq!(a.width, a.height, "icon is square");
        assert_eq!(
            a.rgba.len(),
            (a.width * a.height * 4) as usize,
            "RGBA = 4 bytes/px"
        );
    }

    #[test]
    fn bypassed_is_same_size_but_different_pixels() {
        let a = active();
        let b = bypassed();
        assert_eq!((a.width, a.height), (b.width, b.height));
        assert_ne!(a.rgba, b.rgba, "bypassed must be visually distinct");
        // Alpha channel is preserved (only color is dimmed/desaturated).
        let a_alpha: Vec<u8> = a.rgba.iter().skip(3).step_by(4).copied().collect();
        let b_alpha: Vec<u8> = b.rgba.iter().skip(3).step_by(4).copied().collect();
        assert_eq!(a_alpha, b_alpha, "shape (alpha) unchanged");
    }
}
