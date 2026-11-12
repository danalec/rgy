// SPDX-License-Identifier: Apache-2.0

//! The icon's pixels, drawn here in bytes.
//!
//! The tree computes what it shows and asks the platform to carry it,
//! rather than asking the platform what to show: the mark is a magnifying
//! glass, a handful of circles and a line, and the bytes are the tree's
//! own. The Windows tray hands two GDI bitmaps to the shell; the Linux
//! tray draws the same bytes into an XEmbed window. The mark is the same
//! because the launcher is the same search box.
//!
//! The square root of two is a constant: this crate is `no_std`, and the
//! diagonal is measured squared.

use alloc::vec;
use alloc::vec::Vec;

/// The mark's colour: the launcher's accent.
///
/// A blue that reads on a dark panel and on a light one, and the icon is
/// small enough that a colour which disappears on one of them is an icon
/// that is not there half the time.
pub const ACCENT: (u8, u8, u8) = (0x5A, 0x96, 0xFF);

/// The icon's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    /// The picture: blue, green, red, alpha — top row first.
    pub colour: Vec<u8>,
    /// The size of the square icon, in pixels.
    pub size: i32,
}

/// The mark: a magnifying glass, at the size the tray uses.
///
/// The lens is a ring and the handle is a thick line away from it, and the
/// edge is antialiased by taking sixteen samples of every pixel — four a
/// side — so the mark is a mark at sixteen pixels and at thirty-two.
pub fn magnifier(size: i32) -> Pixels {
    let size = size.max(8);
    let side = size as f32;
    let mut colour = vec![0u8; (size as usize) * (size as usize) * 4];

    // The lens's own place, in the icon's own units: the mark sits in the
    // upper left, and the handle runs to the lower right.
    let centre = (0.40 * side, 0.40 * side);
    let outer = 0.30 * side;
    let inner = 0.185 * side;
    let handle_from = 0.27 * side;
    let handle_to = 0.45 * side;
    let handle_half = 0.075 * side;

    for y in 0..size {
        for x in 0..size {
            let mut covered = 0u32;
            for sy in 0..4u32 {
                for sx in 0..4u32 {
                    let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                    if on_the_mark(
                        px,
                        py,
                        centre,
                        (inner, outer),
                        (handle_from, handle_to, handle_half),
                    ) {
                        covered += 1;
                    }
                }
            }
            let alpha = (covered * 255) / 16;
            let at = (y as usize * size as usize + x as usize) * 4;
            colour[at] = ACCENT.2;
            colour[at + 1] = ACCENT.1;
            colour[at + 2] = ACCENT.0;
            colour[at + 3] = alpha as u8;
        }
    }

    Pixels { colour, size }
}

/// Whether the point is on the mark.
fn on_the_mark(
    x: f32,
    y: f32,
    centre: (f32, f32),
    lens: (f32, f32),
    handle: (f32, f32, f32),
) -> bool {
    let dx = x - centre.0;
    let dy = y - centre.1;
    let squared = dx * dx + dy * dy;
    if squared >= lens.0 * lens.0 && squared <= lens.1 * lens.1 {
        return true;
    }
    // The handle runs along the diagonal, away from the lens.
    let along = (dx + dy) / SQRT_2;
    let across = if dx - dy < 0.0 { dy - dx } else { dx - dy } / SQRT_2;
    along >= handle.0 && along <= handle.1 && across <= handle.2
}

/// The square root of two, in a crate with no `std` to ask.
const SQRT_2: f32 = core::f32::consts::SQRT_2;

#[cfg(test)]
mod tests {
    use super::*;

    /// The mark is a mark: an icon that is empty is an icon that is not
    /// there, and an icon that is a solid square is a square with a
    /// magnifying glass's name on it.
    #[test]
    fn the_mark_is_drawn_and_not_a_square() {
        let pixels = magnifier(16);
        assert_eq!(pixels.colour.len(), 16 * 16 * 4);
        let opaque = pixels.colour.chunks(4).filter(|p| p[3] > 0).count();
        assert!(opaque > 20, "the mark is too small: {opaque} pixels");
        assert!(opaque < 16 * 16, "the mark fills the whole icon");
    }

    /// The middle of the lens is see-through: a ring that is filled is a
    /// disc, and the mark would read as a lollipop.
    #[test]
    fn the_lens_is_a_ring() {
        let pixels = magnifier(16);
        let at = |x: usize, y: usize| -> u8 { pixels.colour[(y * 16 + x) * 4 + 3] };
        assert_eq!(at(6, 6), 0, "the middle of the lens is not see-through");
        assert!(at(4, 4) > 0, "the ring is not on the mark");
    }

    /// The mark is drawn in the accent's blue, with green under it and
    /// red under that.
    #[test]
    fn the_mark_is_the_accent_blue() {
        let pixels = magnifier(16);
        for p in pixels.colour.chunks(4) {
            if p[3] > 0 {
                assert!(p[0] > p[2], "blue leads: {p:?}");
                assert_eq!(p[0], ACCENT.2);
                assert_eq!(p[1], ACCENT.1);
                assert_eq!(p[2], ACCENT.0);
            }
        }
    }

    /// A size below the smallest a mark can be is drawn at the smallest.
    #[test]
    fn a_tiny_icon_is_still_a_mark() {
        assert_eq!(magnifier(2).size, 8);
        assert_eq!(magnifier(32).colour.len(), 32 * 32 * 4);
    }
}
