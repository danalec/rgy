// SPDX-License-Identifier: Apache-2.0

//! Where the user dragged the window, kept per monitor.
//!
//! A placement is the window's top-left corner **normalized to a work
//! area**, so that it survives a change of resolution, scale or taskbar,
//! and desktop coordinates (negative ones included) are never kept. The
//! window keeps one per monitor, under a key made from the monitor's
//! identity; a monitor with none uses the configured anchor.
//!
//! The grip the window is dragged by is measured here too: the picture and
//! the hit test have to agree on where it is.
//!
//! Nothing here reads or writes a file: the host loads the placements,
//! hands them to the window and stores what the window says changed.

use alloc::collections::BTreeMap;
use alloc::string::String;

use rgy_launcher::Layout;
use rgy_launcher::layout::device_pixels;

use crate::win::*;

/// The whole of a work area's side, in the units of an [`Anchor`].
///
/// Finer than a pixel on any monitor, so a window comes back where it was
/// dropped.
pub const ANCHOR_SCALE: u32 = 10_000;

/// A saved place on a work area, each number in `0..=ANCHOR_SCALE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// The left edge, across the width the window does not take.
    pub x: u32,
    /// The top edge, down the area's own height.
    pub y: u32,
}

/// The saved places, by monitor key.
pub type Placements = BTreeMap<String, Anchor>;

/// A window's rectangle: where it starts and how big it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// `value * numerator / denominator`, to the nearest whole number.
///
/// The callers pass numbers that are zero or more.
fn ratio(value: i64, numerator: i64, denominator: i64) -> i64 {
    if denominator <= 0 {
        return 0;
    }
    (value * numerator + denominator / 2) / denominator
}

/// The anchor of a window of this size whose top-left corner is here.
///
/// A corner that would leave part of the window outside the area is
/// brought inside first.
pub(crate) fn anchor_of(work: RECT, corner: (i32, i32), size: (i32, i32)) -> Anchor {
    let area_w = (i64::from(work.right) - i64::from(work.left)).max(0);
    let area_h = (i64::from(work.bottom) - i64::from(work.top)).max(0);
    let free_w = (area_w - i64::from(size.0)).max(0);
    let free_h = (area_h - i64::from(size.1)).max(0);
    let across = (i64::from(corner.0) - i64::from(work.left)).clamp(0, free_w);
    let down = (i64::from(corner.1) - i64::from(work.top)).clamp(0, free_h);
    let scale = i64::from(ANCHOR_SCALE);
    Anchor {
        x: ratio(across, scale, free_w) as u32,
        y: ratio(down, scale, area_h) as u32,
    }
}

/// Where a window of this size goes for a saved anchor.
///
/// The whole window is inside the area: it is cut only when the area itself
/// is smaller, and one saved low is moved up until its bottom fits — so the
/// query field is always there to be typed into.
pub(crate) fn restore(work: RECT, width: i32, height: i32, anchor: Anchor) -> Bounds {
    let area_w = (work.right - work.left).max(0);
    let area_h = (work.bottom - work.top).max(0);
    let width = width.clamp(0, area_w);
    let height = height.clamp(0, area_h);
    let scale = i64::from(ANCHOR_SCALE);
    let across = ratio(
        i64::from(area_w - width),
        i64::from(anchor.x.min(ANCHOR_SCALE)),
        scale,
    ) as i32;
    let down = ratio(
        i64::from(area_h),
        i64::from(anchor.y.min(ANCHOR_SCALE)),
        scale,
    ) as i32;
    Bounds {
        x: work.left + across,
        y: work.top + down.min(area_h - height),
        width,
        height,
    }
}

// The grip, in the model's units. It lives in the padding above the query
// field, which starts fifteen units down: it covers no text, no caret and
// no action.
const GRIP_HIT_WIDTH: u32 = 80;
const GRIP_HIT_TOP: u32 = 1;
const GRIP_HIT_BOTTOM: u32 = 14;
const GRIP_BAR_WIDTH: u32 = 32;
const GRIP_BAR_TOP: u32 = 6;
const GRIP_BAR_HEIGHT: u32 = 3;

/// The width of the picture for a view of this model width, in device
/// pixels. Zero is a view that has not been sized, which the painting draws
/// at the geometry's own width.
pub(crate) fn picture_width(view_width: u32, scale: u32) -> i32 {
    let width = if view_width == 0 {
        Layout::default().width
    } else {
        view_width
    };
    device_pixels(width, scale) as i32
}

/// A strip of the grip, centred in the picture.
fn centred(picture: i32, scale: u32, width: u32, top: u32, bottom: u32) -> RECT {
    let width = device_pixels(width, scale) as i32;
    let left = (picture - width) / 2;
    RECT {
        left,
        top: device_pixels(top, scale) as i32,
        right: left + width,
        bottom: device_pixels(bottom, scale) as i32,
    }
}

/// The mark the user sees.
pub(crate) fn grip_bar(picture: i32, scale: u32) -> RECT {
    centred(
        picture,
        scale,
        GRIP_BAR_WIDTH,
        GRIP_BAR_TOP,
        GRIP_BAR_TOP + GRIP_BAR_HEIGHT,
    )
}

/// Whether a point of the window is on the grip. It is larger than the mark.
pub(crate) fn on_grip(picture: i32, scale: u32, x: i32, y: i32) -> bool {
    let hit = centred(
        picture,
        scale,
        GRIP_HIT_WIDTH,
        GRIP_HIT_TOP,
        GRIP_HIT_BOTTOM,
    );
    x >= hit.left && x < hit.right && y >= hit.top && y < hit.bottom
}

/// The screen point a mouse message carries: two **signed** words, because
/// a monitor left of or above the primary one has negative coordinates.
pub(crate) fn point_of(lparam: LPARAM) -> POINT {
    POINT {
        x: i32::from(lparam as u16 as i16),
        y: i32::from((lparam >> 16) as u16 as i16),
    }
}

/// The text of a fixed buffer, up to its first NUL.
fn text_of(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// The key for a display, from what the platform says about it.
///
/// `monitors` are the monitors of the display as `(active, interface
/// path)`. The path of an active one is preferred, then any path; between
/// several (a mirrored display) the smallest wins, so that the order of the
/// enumeration decides nothing. With no path at all the key is the
/// display's device name, which Windows may give to another monitor after a
/// reconnection or a restart.
fn choose_key<'a>(device: &str, monitors: impl Iterator<Item = (bool, &'a str)>) -> Option<String> {
    let best = monitors
        .filter(|(_, path)| !path.is_empty())
        .map(|(active, path)| (!active, path.to_ascii_lowercase()))
        .min();
    match best {
        Some((_, path)) => Some(path),
        None if device.is_empty() => None,
        None => Some(alloc::format!("device:{device}")),
    }
}

/// The key a monitor's placement is kept under. `None` when the platform
/// will not say which monitor it is.
pub(crate) fn monitor_key(monitor: HMONITOR) -> Option<String> {
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFOEXW {
        info: MONITORINFO {
            cbSize: core::mem::size_of::<MONITORINFOEXW>() as u32,
            rcMonitor: RECT::default(),
            rcWork: RECT::default(),
            dwFlags: 0,
        },
        szDevice: [0; 32],
    };
    // SAFETY: the size says the buffer is the extended struct, whose first
    // field is the plain one.
    let known = unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) } != 0;
    if !known {
        return None;
    }
    // A display has a monitor or a few: the bound only ends a loop the
    // platform would not.
    let mut found: alloc::vec::Vec<(bool, String)> = alloc::vec::Vec::new();
    for index in 0..16 {
        let mut display = DISPLAY_DEVICEW {
            cb: core::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            DeviceName: [0; 32],
            DeviceString: [0; 128],
            StateFlags: 0,
            DeviceID: [0; 128],
            DeviceKey: [0; 128],
        };
        // SAFETY: the device name is NUL-terminated by the platform, and
        // `cb` is the struct's own size.
        let more = unsafe {
            EnumDisplayDevicesW(
                info.szDevice.as_ptr(),
                index,
                &mut display,
                EDD_GET_DEVICE_INTERFACE_NAME,
            )
        } != 0;
        if !more {
            break;
        }
        let active = display.StateFlags & DISPLAY_DEVICE_ACTIVE != 0;
        found.push((active, text_of(&display.DeviceID)));
    }
    choose_key(
        &text_of(&info.szDevice),
        found.iter().map(|(active, path)| (*active, path.as_str())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn a_dropped_window_comes_back_where_it_was() {
        for work in [
            area(0, 0, 1920, 1040),
            area(-2560, -300, 0, 1100),
            area(1920, 200, 5760, 2320),
        ] {
            for corner in [
                (work.left, work.top),
                (work.left + 137, work.top + 411),
                (work.right - 600, work.bottom - 89),
            ] {
                let anchor = anchor_of(work, corner, (600, 89));
                let back = restore(work, 600, 89, anchor);
                assert_eq!((back.x, back.y), corner, "{anchor:?}");
            }
        }
    }

    #[test]
    fn a_corner_outside_the_area_is_brought_inside() {
        let work = area(-1920, 0, 0, 1040);
        let anchor = anchor_of(work, (-4000, 5000), (600, 89));
        let back = restore(work, 600, 89, anchor);
        assert_eq!((back.x, back.y), (-1920, 1040 - 89));
    }

    #[test]
    fn a_saved_place_stays_whole_on_a_smaller_area_and_a_taller_window() {
        let low = Anchor {
            x: ANCHOR_SCALE,
            y: ANCHOR_SCALE,
        };
        // A smaller screen, a taskbar at the top, and results under the
        // field.
        for work in [area(0, 48, 1280, 720), area(-800, -600, 0, 0)] {
            for height in [89, 500, 2000] {
                let spot = restore(work, 600, height, low);
                assert!(spot.x >= work.left && spot.x + spot.width <= work.right);
                assert!(spot.y >= work.top && spot.y + spot.height <= work.bottom);
                assert_eq!(spot.height, height.min(work.bottom - work.top));
            }
        }
        let narrow = restore(area(0, 0, 400, 300), 600, 500, low);
        assert_eq!(
            narrow,
            Bounds {
                x: 0,
                y: 0,
                width: 400,
                height: 300
            }
        );
    }

    #[test]
    fn an_anchor_beyond_the_scale_is_the_far_edge() {
        let work = area(0, 0, 1920, 1040);
        let wild = Anchor {
            x: u32::MAX,
            y: u32::MAX,
        };
        let spot = restore(work, 600, 89, wild);
        assert_eq!((spot.x, spot.y), (1320, 951));
    }

    #[test]
    fn the_grip_is_above_the_query_field() {
        let layout = Layout::default();
        for scale in [100, 125, 150, 175, 200, 300] {
            let picture = picture_width(0, scale);
            let field = layout.scaled(scale).input_top() as i32;
            let middle = picture / 2;
            let bar = grip_bar(picture, scale);
            assert!(on_grip(picture, scale, middle, bar.top));
            assert!(on_grip(picture, scale, bar.left, bar.bottom - 1));
            assert!(bar.bottom <= field);
            assert!(!on_grip(picture, scale, middle, field), "{scale}");
            assert!(!on_grip(picture, scale, 20, bar.top), "{scale}");
            assert!(!on_grip(picture, scale, picture - 20, bar.top), "{scale}");
        }
    }

    #[test]
    fn a_mouse_point_keeps_its_sign() {
        let packed = |x: i16, y: i16| ((u32::from(y as u16) << 16) | u32::from(x as u16)) as LPARAM;
        for (x, y) in [(10, 20), (-1900, 5), (7, -300), (-1, -1)] {
            let point = point_of(packed(x, y));
            assert_eq!((point.x, point.y), (i32::from(x), i32::from(y)));
        }
    }

    #[test]
    fn the_monitor_key_does_not_depend_on_the_enumeration_order() {
        let a = (true, r"\\?\DISPLAY#AAA#1");
        let b = (true, r"\\?\DISPLAY#BBB#2");
        let idle = (false, r"\\?\DISPLAY#000#3");
        let wanted = Some(r"\\?\display#aaa#1".into());
        for order in [[a, b, idle], [idle, b, a], [b, idle, a]] {
            assert_eq!(
                choose_key(r"\\.\DISPLAY1", order.into_iter()),
                wanted,
                "{order:?}"
            );
        }
        // The device name is not part of a key that has a path.
        assert_eq!(choose_key(r"\\.\DISPLAY7", [a].into_iter()), wanted);
    }

    #[test]
    fn a_monitor_with_no_path_is_kept_by_its_device_name() {
        assert_eq!(
            choose_key(r"\\.\DISPLAY2", [(true, "")].into_iter()),
            Some(r"device:\\.\DISPLAY2".into())
        );
        assert_eq!(choose_key("", core::iter::empty()), None);
    }
}
