// SPDX-License-Identifier: Apache-2.0

//! A software rasterizer: the frame is drawn into memory, then carried.
//!
//! Xlib's primitives are one round trip each; a frame drawn into a buffer
//! and blitted with one `XPutImage` is one call and the same picture. This
//! module is the memory half, with no FFI and no X11, so a test can decide
//! every drawing question on a host with no display.
//!
//! Blending is integer-only, so a frame is the same frame on every target;
//! only the corners are antialiased, since the straight edges land on
//! whole pixels and the 4x4 samples are spent where the curve is.

use alloc::{vec, vec::Vec};

/// A 32-bit ARGB surface: one `u32` per pixel, row-major, no padding.
///
/// Straight `0xAARRGGBB` is what the theme speaks, and the X11 image is a
/// masked copy of the same block: no padding, no conversion, no stride.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    /// Width in pixels; `pixels.len()` is `width * height`.
    pub width: u32,
    /// Height in pixels; `pixels.len()` is `width * height`.
    pub height: u32,
    /// The pixels, `0xAARRGGBB` each, top row first, no padding.
    pub pixels: Vec<u32>,
}

// Three of these signatures are eight arguments wide; the seam fixes them.
#[allow(clippy::too_many_arguments)]
impl Surface {
    /// A zeroed surface: transparent composes with anything, so an untouched
    /// pixel is the identity, not black.
    pub fn new(width: u32, height: u32) -> Surface {
        Surface {
            width,
            height,
            pixels: vec![0u32; (width as usize).saturating_mul(height as usize)],
        }
    }

    /// Resizes and zeroes every pixel: the allocation is reused because the
    /// frame loop paid for it, and no pixel of the old picture survives.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let len = (width as usize).saturating_mul(height as usize);
        self.pixels.clear();
        self.pixels.resize(len, 0);
    }

    /// Paints every pixel; a new frame starts here, so nothing trails from
    /// the previous one.
    pub fn clear(&mut self, argb: u32) {
        self.pixels.fill(argb);
    }

    /// Writes `argb` with no blending, for a pixel whose colour is decided.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn put(&mut self, x: i32, y: i32, argb: u32) {
        if let Some(p) = self.at_mut(x, y) {
            *p = argb;
        }
    }

    /// Source-over in integers: `out_a = sa + da * (255 - sa) / 255`, each
    /// channel divided by `out_a`, for the determinism the module note
    /// gives. Alpha 255 replaces and 0 leaves the pixel alone, exactly.
    pub fn blend(&mut self, x: i32, y: i32, argb: u32) {
        if let Some(p) = self.at_mut(x, y) {
            *p = blend_pixel(*p, argb);
        }
    }

    /// The pixel at `(x, y)`, or `0` off the surface; `0` is transparent, so
    /// the clip need not be spelled twice.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return 0;
        }
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    /// Fills a rectangle, blended so translucent bands share the call; a
    /// zero or negative size is a no-op at the call site.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, argb: u32) {
        let (x0, y0, x1, y1) = match self.visible(x, y, w, h) {
            Some(r) => r,
            None => return,
        };
        for py in y0..y1 {
            self.hline(x0, x1 - 1, py, argb);
        }
    }

    /// Fills a rounded rectangle with antialiased corners; the radius clamps
    /// to half the smaller side, so a theme asking for a pill gets a pill.
    pub fn fill_round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: u32, argb: u32) {
        let (x0, y0, x1, y1) = match self.visible(x, y, w, h) {
            Some(r) => r,
            None => return,
        };
        let r = radius.min((w.min(h) / 2) as u32) as i32;
        if r == 0 {
            self.fill_rect(x, y, w, h, argb);
            return;
        }
        for py in y0..y1 {
            for px in x0..x1 {
                let coverage = round_rect_coverage(px, py, x, y, w, h, r);
                self.blend_coverage(px, py, argb, coverage);
            }
        }
    }

    /// Strokes the ring between the outer and the inner shape, so the line
    /// keeps its width around the arc; a thickness that would close the
    /// centre fills the rectangle instead of leaving a hole.
    pub fn stroke_round_rect(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        radius: u32,
        thickness: u32,
        argb: u32,
    ) {
        if w <= 0 || h <= 0 || thickness == 0 {
            return;
        }
        if thickness as u64 * 2 >= w.min(h) as u64 {
            self.fill_round_rect(x, y, w, h, radius, argb);
            return;
        }
        let (x0, y0, x1, y1) = match self.visible(x, y, w, h) {
            Some(r) => r,
            None => return,
        };
        let r = radius.min((w.min(h) / 2) as u32) as i32;
        let t = thickness as i32;
        let (iw, ih) = (w - 2 * t, h - 2 * t);
        let ir = (r - t).max(0).min(iw.min(ih) / 2);
        for py in y0..y1 {
            for px in x0..x1 {
                let outer = round_rect_coverage(px, py, x, y, w, h, r);
                let inner = round_rect_coverage(px, py, x + t, y + t, iw, ih, ir);
                self.blend_coverage(px, py, argb, outer.saturating_sub(inner));
            }
        }
    }

    /// A one-pixel row from `x0` to `x1`, both included, so a border has no
    /// gap at its end; clipped and blended.
    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, argb: u32) {
        let (a, b) = (x0.min(x1).max(0), x0.max(x1).min(self.width as i32 - 1));
        for x in a..=b {
            self.blend(x, y, argb);
        }
    }

    /// The twin of [`Surface::hline`], for the same reason.
    pub fn vline(&mut self, x: i32, y0: i32, y1: i32, argb: u32) {
        let (a, b) = (y0.min(y1).max(0), y0.max(y1).min(self.height as i32 - 1));
        for y in a..=b {
            self.blend(x, y, argb);
        }
    }

    /// Blends `argb` through 8-bit coverage: text is coverage, not colour.
    /// `pitch` may exceed `w` (a cached glyph keeps its rows aligned), and a
    /// mask read past its end is zero coverage, so a short mask is safe.
    pub fn blend_mask(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        mask: &[u8],
        pitch: u32,
        argb: u32,
    ) {
        let w = w.min(i32::MAX as u32) as i32;
        let h = h.min(i32::MAX as u32) as i32;
        let (x0, y0, x1, y1) = match self.visible(x, y, w, h) {
            Some(r) => r,
            None => return,
        };
        for py in y0..y1 {
            let mut i = ((py as i64 - y as i64) as usize).saturating_mul(pitch as usize);
            for px in x0..x1 {
                let coverage = mask.get(i).copied().unwrap_or(0) as u32;
                i = i.saturating_add(1);
                self.blend_coverage(px, py, argb, coverage);
            }
        }
    }

    /// A soft dark edge: `1 - dist/depth` on `argb`'s alpha (the shadow's
    /// strength at the edge), clipped by the rounded coverage so it never
    /// paints into the transparent corners.
    pub fn inner_shadow(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        radius: u32,
        depth: u32,
        argb: u32,
    ) {
        if w <= 0 || h <= 0 || depth == 0 {
            return;
        }
        let (x0, y0, x1, y1) = match self.visible(x, y, w, h) {
            Some(r) => r,
            None => return,
        };
        let r = radius.min((w.min(h) / 2) as u32) as i32;
        let reach = depth as i64 * 2;
        for py in y0..y1 {
            for px in x0..x1 {
                let coverage = round_rect_coverage(px, py, x, y, w, h, r);
                let distance = distance_half(px, py, x, y, w, h).max(0);
                if distance < reach {
                    let falloff = (reach - distance) * 255 / reach;
                    self.blend_coverage(px, py, argb, coverage * falloff as u32 / 255);
                }
            }
        }
    }

    /// A mutable reference to `(x, y)`, or `None` off the surface: the one
    /// clip the writers share.
    fn at_mut(&mut self, x: i32, y: i32) -> Option<&mut u32> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return None;
        }
        Some(&mut self.pixels[y as usize * self.width as usize + x as usize])
    }

    /// The visible part of a rectangle as half-open bounds; `i64` because
    /// `x + w` overflows `i32` for a rectangle that starts far off-screen.
    fn visible(&self, x: i32, y: i32, w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
        if w <= 0 || h <= 0 {
            return None;
        }
        let x0 = (x as i64).clamp(0, self.width as i64);
        let y0 = (y as i64).clamp(0, self.height as i64);
        let x1 = (x as i64 + w as i64).clamp(0, self.width as i64);
        let y1 = (y as i64 + h as i64).clamp(0, self.height as i64);
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        Some((x0 as i32, y0 as i32, x1 as i32, y1 as i32))
    }

    /// Blends `argb` with its alpha scaled by `coverage`: the one path the
    /// antialiased fill, the glyph mask and the shadow share.
    fn blend_coverage(&mut self, x: i32, y: i32, argb: u32, coverage: u32) {
        let a = ((argb >> 24) & 0xFF) * coverage / 255;
        if a != 0 {
            self.blend(x, y, (argb & 0x00FF_FFFF) | (a << 24));
        }
    }
}

/// Packs a colour into `0xAARRGGBB`; a function because themes build
/// colours at runtime, and the argument order is otherwise a comment.
pub fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Mixes per channel: `t == 0` gives `a` and `t == 255` gives `b`, exactly,
/// because a theme's endpoints must come back unchanged; the middle is the
/// integer average, and alpha is mixed like any other channel.
pub fn lerp(a: u32, b: u32, t: u8) -> u32 {
    let t = t as u32;
    let inv = 255 - t;
    let mut out = 0u32;
    for shift in [24u32, 16, 8, 0] {
        let (ca, cb) = ((a >> shift) & 0xFF, (b >> shift) & 0xFF);
        out |= ((ca * inv + cb * t) / 255) << shift;
    }
    out
}

/// Drops alpha: a 24-bit `ZPixmap` wants `0x00RRGGBB`, so an alpha left in
/// the high byte would be a colour shift. The name is historical — the
/// surface is straight ARGB, so there is no premultiplication to undo.
pub fn premultiplied_to_x11(p: u32) -> u32 {
    p & 0x00FF_FFFF
}

/// Source-over of one straight-ARGB pixel on another, in integers; alpha 0
/// returns the destination and alpha 255 the source, exactly.
fn blend_pixel(dst: u32, src: u32) -> u32 {
    let sa = (src >> 24) & 0xFF;
    if sa == 0 {
        return dst;
    }
    if sa == 255 {
        return src;
    }
    let da = (dst >> 24) & 0xFF;
    let inv = 255 - sa;
    let out_a = sa + da * inv / 255;
    let channel = |shift: u32| -> u32 {
        let sc = (src >> shift) & 0xFF;
        let dc = (dst >> shift) & 0xFF;
        (sc * sa + dc * da * inv / 255) / out_a
    };
    (out_a << 24) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// The fraction of the pixel at `(px, py)` a rounded rectangle covers, in
/// `0..=255`: full for the straight edges and the interior, sixteen integer
/// samples of the arc for a corner pixel.
fn round_rect_coverage(px: i32, py: i32, x: i32, y: i32, w: i32, h: i32, r: i32) -> u32 {
    if r <= 0 {
        return 255;
    }
    let (x, y, w, h, r) = (x as i64, y as i64, w as i64, h as i64, r as i64);
    let (qx, qy) = (px as i64, py as i64);
    if qx < x || qy < y || qx >= x + w || qy >= y + h {
        return 0;
    }
    let corner = (qx < x + r || qx >= x + w - r) && (qy < y + r || qy >= y + h - r);
    if !corner {
        return 255;
    }
    let (px8, py8, r8) = (qx * 8, qy * 8, r * 8);
    let (left, right) = ((x + r) * 8, (x + w - r) * 8);
    let (top, bottom) = ((y + r) * 8, (y + h - r) * 8);
    let mut hits = 0u32;
    for sy in 0..4i64 {
        for sx in 0..4i64 {
            let fx = px8 + 2 * sx + 1;
            let fy = py8 + 2 * sy + 1;
            let (ix, iy) = (fx.clamp(left, right), fy.clamp(top, bottom));
            if (fx - ix) * (fx - ix) + (fy - iy) * (fy - iy) <= r8 * r8 {
                hits += 1;
            }
        }
    }
    hits * 255 / 16
}

/// The signed distance from a pixel's centre to the nearest side of the
/// rectangle, in half pixels: positive inside. The rounded corners are the
/// coverage's job, so this measures the box the shadow is drawn inside.
fn distance_half(px: i32, py: i32, x: i32, y: i32, w: i32, h: i32) -> i64 {
    let (cx, cy) = (2 * px as i64 + 1, 2 * py as i64 + 1);
    let (x, y, w, h) = (x as i64, y as i64, w as i64, h as i64);
    (cx - 2 * x)
        .min(2 * (x + w) - cx)
        .min(cy - 2 * y)
        .min(2 * (y + h) - cy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(p: u32) -> u32 {
        p >> 24
    }

    #[test]
    fn a_new_surface_is_zeroed_and_sized() {
        let s = Surface::new(7, 3);
        assert_eq!(s.pixels.len(), 21);
        assert!(s.pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn blending_half_over_black_is_half() {
        let mut s = Surface::new(1, 1);
        s.clear(argb(255, 0, 0, 0));
        s.blend(0, 0, argb(128, 255, 255, 255));
        assert_eq!(s.get(0, 0), 0xFF80_8080);
    }

    #[test]
    fn blending_full_alpha_replaces() {
        let mut s = Surface::new(1, 1);
        s.clear(argb(128, 0xAB, 0xCD, 0xEF));
        s.blend(0, 0, argb(255, 10, 20, 30));
        assert_eq!(s.get(0, 0), 0xFF0A_141E);
    }

    #[test]
    fn drawing_outside_is_clipped_and_harmless() {
        let mut s = Surface::new(4, 4);
        s.clear(argb(255, 40, 80, 120));
        let before = s.pixels.clone();
        let red = argb(255, 255, 0, 0);
        for (x, y, w, h) in [(-8, -8, 4, 4), (4, 2, 4, 4), (0, 0, -3, 4)] {
            s.fill_rect(x, y, w, h, red);
        }
        for (x, y) in [(-1, 2), (4, 2), (2, -1), (2, 4)] {
            s.put(x, y, red);
            s.blend(x, y, red);
        }
        s.hline(-10, -5, 1, red);
        s.hline(10, 20, 1, red);
        s.vline(-1, 0, 3, red);
        s.vline(4, 0, 3, red);
        assert_eq!(s.pixels, before, "a clipped call touched the surface");
    }

    #[test]
    fn a_round_rect_corner_is_not_square() {
        let mut s = Surface::new(64, 64);
        s.fill_round_rect(8, 8, 40, 40, 10, argb(255, 200, 20, 40));
        assert!(alpha(s.get(8, 8)) < 255, "the corner pixel is square");
        assert_eq!(alpha(s.get(28, 8)), 255, "the edge centre is empty");
    }

    #[test]
    fn a_round_rect_leaves_transparent_corners() {
        let mut s = Surface::new(48, 48);
        s.fill_round_rect(4, 4, 40, 40, 12, argb(255, 10, 20, 30));
        assert_eq!(s.get(4, 4), 0, "the exact corner is not transparent");
        assert_eq!(alpha(s.get(20, 4)), 255);
    }

    #[test]
    fn a_stroke_draws_only_the_edge() {
        let mut s = Surface::new(64, 64);
        s.stroke_round_rect(4, 4, 40, 40, 8, 2, argb(255, 240, 240, 240));
        assert_eq!(s.get(24, 24), 0, "the middle of the stroke is filled");
        assert_eq!(s.get(24, 4), argb(255, 240, 240, 240));
        assert_eq!(s.get(24, 6), 0, "the stroke is wider than it says");
    }

    #[test]
    fn blend_mask_respects_coverage() {
        let mut s = Surface::new(3, 1);
        let paint = argb(255, 10, 20, 30);
        s.blend_mask(0, 0, 3, 1, &[0, 128, 255], 3, paint);
        assert_eq!(s.get(0, 0), 0, "a zero mask changed the pixel");
        assert_eq!(s.get(1, 0), argb(128, 10, 20, 30));
        assert_eq!(s.get(2, 0), paint);
    }

    #[test]
    fn lerp_hits_both_ends() {
        let (a, b) = (argb(255, 10, 20, 30), argb(0, 200, 100, 50));
        assert_eq!(lerp(a, b, 0), a);
        assert_eq!(lerp(a, b, 255), b);
    }

    #[test]
    fn premultiplied_to_x11_drops_alpha() {
        assert_eq!(premultiplied_to_x11(0xFF11_2233), 0x0011_2233);
    }
}
