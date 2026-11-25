// SPDX-License-Identifier: Apache-2.0

//! Where the window goes, and how many rows fit under it.
//!
//! The numbers are the model's: the launcher is drawn in the units of a
//! 96-DPI screen, and X11 answers in pixels. On X11 the two are the same
//! number — there is no per-monitor scale to follow — but the geometry is
//! still the model's geometry (`rgy_launcher::layout`), and the seat on
//! the screen is still the same anchor: `x` across the room the window
//! does not take, `y` down the work area.
//!
//! These are decisions about numbers, not about a display connection,
//! which is why they are testable on the host that cannot open one.

use rgy_launcher::layout::{BASE_DPI, SCALE_PERCENT, model_pixels};

/// Where the window sits on its work area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    /// The left edge, in device pixels.
    pub x: i32,
    /// The top edge, in device pixels.
    pub y: i32,
    /// The width, clipped to the work area.
    pub width: i32,
    /// The height, clipped to the room below the anchor.
    pub height: i32,
}

/// A work area as `(left, top, right, bottom)`, in device pixels.
pub type WorkArea = (i32, i32, i32, i32);

/// An anchor outside the work area cannot be honoured.
pub fn clamp_anchor(x: u32, y: u32) -> (u32, u32) {
    (x.clamp(0, 1000), y.clamp(0, 1000))
}

/// Where the window goes on a work area.
///
/// The window never falls outside the area. One that is wider than the
/// area is cut to the area, one taller than the area is pinned to the top
/// and cut to the area's height, and one taller than the room below the
/// anchor is cut to that room.
pub fn spot_on(work: WorkArea, width: i32, height: i32, anchor: (u32, u32)) -> Spot {
    let (left, top, right, bottom) = work;
    let area_w = right - left;
    let area_h = bottom - top;
    let width = width.min(area_w);
    // The permille is of the room the window does not take: the far end
    // of that room is the area's own right edge.
    let x = left + (area_w - width).max(0) * anchor.0 as i32 / 1000;
    // The offset is capped so that there is always a row of room: a
    // window with no height is a window that is not there.
    let offset = (area_h * anchor.1 as i32 / 1000).min((area_h - 1).max(0));
    if height >= area_h {
        return Spot {
            x,
            y: top,
            width,
            height: area_h,
        };
    }
    let room = (area_h - offset).max(0);
    Spot {
        x,
        y: top + offset,
        width,
        height: height.min(room),
    }
}

/// How tall the room below the anchor is, in the model's units.
///
/// `rows_that_fit` counts rows in that room, and the host sizes the
/// window from them — a screen that is twice as big in pixels is not
/// twice as many rows on a scaled monitor.
pub fn rows_height(area_device: u32, scale: u32, anchor_y: u32) -> u32 {
    rgy_launcher::app::room_below_anchor(model_pixels(area_device, scale), anchor_y)
}

/// The scale of a monitor, in per cent, from its DPI.
pub fn scale_of(dpi: u32) -> u32 {
    if dpi == 0 {
        return SCALE_PERCENT;
    }
    dpi * SCALE_PERCENT / BASE_DPI
}

/// The part of a surface the last picture covers, in device pixels.
///
/// Only what the picture painted may be handed to the compositor. A
/// picture smaller than the surface it was drawn on would leave an
/// unpainted strip, and an unpainted strip is a solid bar.
pub fn covered_layer(layer: (i32, i32), picture: (i32, i32)) -> (i32, i32) {
    (
        picture.0.clamp(0, layer.0.max(0)),
        picture.1.clamp(0, layer.1.max(0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window is centred on the work area at the default anchor.
    #[test]
    fn the_window_is_centred_on_the_work_area() {
        let spot = spot_on((0, 0, 1920, 1080), 600, 400, (500, 180));
        assert_eq!(spot.x, (1920 - 600) / 2, "{spot:?}");
        assert!(spot.y > 0 && spot.y < 400, "{spot:?}");
        assert_eq!((spot.width, spot.height), (600, 400));
    }

    /// A window wider than the work area starts at its left edge and is
    /// cut to the area.
    #[test]
    fn a_wide_window_is_cut_to_the_area() {
        let spot = spot_on((0, 0, 320, 200), 600, 80, (500, 180));
        assert_eq!(spot.x, 0);
        assert_eq!(spot.width, 320);
    }

    /// A window taller than the work area is pinned to the top.
    #[test]
    fn a_tall_window_is_pinned_to_the_top() {
        let spot = spot_on((0, 0, 800, 200), 600, 900, (500, 900));
        assert_eq!(spot.y, 0);
        assert_eq!(spot.height, 200);
    }

    /// The window never falls outside the work area below its anchor.
    #[test]
    fn a_window_never_falls_outside_the_work_area() {
        let spot = spot_on((0, 0, 800, 200), 600, 400, (500, 900));
        assert!(spot.y + spot.height <= 200, "{spot:?}");
    }

    /// An anchor of zero is the corner, and a thousand the far edge.
    #[test]
    fn an_anchor_of_zero_is_the_corner_and_a_thousand_the_edge() {
        let left = spot_on((0, 0, 1000, 500), 200, 100, (0, 0));
        assert_eq!((left.x, left.y), (0, 0));
        let right = spot_on((0, 0, 1000, 500), 200, 100, (1000, 0));
        assert_eq!(right.x, 800);
    }

    /// An anchor outside the range is clamped, not honoured off-screen.
    #[test]
    fn an_anchor_outside_the_range_is_clamped() {
        assert_eq!(clamp_anchor(2000, 30), (1000, 30));
        assert_eq!(clamp_anchor(30, 2000), (30, 1000));
    }

    /// A screen whose DPI is twice the baseline is a 200% scale.
    #[test]
    fn a_screen_at_twice_the_dpi_is_a_double_scale() {
        assert_eq!(scale_of(0), 100);
        assert_eq!(scale_of(96), 100);
        assert_eq!(scale_of(192), 200);
    }

    /// The picture is normally the larger of the two; the clamp is what
    /// catches one that came out smaller.
    #[test]
    fn the_picture_covers_no_more_than_the_layer() {
        assert_eq!(covered_layer((600, 400), (600, 400)), (600, 400));
        assert_eq!(covered_layer((600, 400), (700, 500)), (600, 400));
        assert_eq!(covered_layer((600, 400), (300, 200)), (300, 200));
    }
}
