// SPDX-License-Identifier: Apache-2.0

//! The icon the launcher leaves in the notification area.
//!
//! The window is a tool window on purpose: it is not in the taskbar and not
//! in the alt-tab list, so the launcher has no face at all while it waits
//! for the next Control. The notification area is that face — an icon the
//! user can click to open the window, and a menu to end the program.
//!
//! # The icon is drawn here, in bytes
//!
//! The tree computes what it shows and asks the platform to carry it, rather
//! than asking the platform what to show: the mark is a magnifying glass, it
//! is a handful of circles and a line, and the bytes are the tree's own.
//! The platform gets two bitmaps and an icon, and it draws them at whatever
//! size the screen has.
//!
//! The mark is a magnifying glass because the launcher is a search box, and
//! the search box is what the whole window is: the field, the answers and
//! the actions are all inside it.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rgy_launcher::text::Text;

use crate::win::*;

/// The message the shell sends when the icon is clicked.
///
/// The shell does not send a message of its own for the notification area:
/// the window declares the number, and the shell sends it back with the
/// mouse's message in `lParam` and the pointer's place in `wParam`.
///
/// This is the window's fourth private message: `WM_TOGGLE`, `WM_SHOW`
/// and `WM_WAKE` are `WM_APP+1..+3` in `window.rs`, so the tray takes the
/// next free number, `WM_APP+4`. A shared id would let a posted showing
/// be answered as a tray click, or a tray callback as a showing — the
/// right-click menu would never open.
pub const WM_TRAY: UINT = WM_APP + 4;

/// The mouse's own message, out of the word the shell sent.
///
/// The shell's fourth version of the record puts the mouse's message in the
/// LOW word of the word it sends and the icon's place in the high word. A
/// comparison with the whole word never matches — 0x0202 with a place in
/// the high word is a different number — and an icon that is never matched
/// is an icon that does not answer when it is clicked.
pub fn mouse_message(lparam: LPARAM) -> UINT {
    (lparam as u32 & 0xFFFF) as UINT
}

/// The icon's own id, in the shell's table.
///
/// The shell keeps the icons of a window under their ids, and the id is the
/// window's own: one window, one icon.
const ICON_ID: UINT = 1;

/// The mark's colour: the launcher's accent.
///
/// The accent is a blue that reads on the dark taskbar of a new Windows and
/// on the light one of an old one, and the icon is small enough that a
/// colour that reads on one background and disappears on the other would be
/// an icon that is not there half the time.
const ACCENT: (u8, u8, u8) = (0x5A, 0x96, 0xFF);

/// What the menu's items do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Open the window — the same thing the hotkey does.
    Search,
    /// Open the window with the settings panel open — the same thing
    /// `>settings` does, for the user who has forgotten the command.
    Settings,
    /// End the program.
    Quit,
}

/// The icon's pixels.
///
/// The colour bitmap is the picture and the mask says which of its pixels
/// the shell draws nothing on: a set bit is a transparent pixel. Both are
/// the platform's own format, and the tree fills them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    /// The colour bitmap: blue, green, red, alpha — top row first.
    pub colour: Vec<u8>,
    /// The mask: one bit a pixel, the most significant bit first, and a set
    /// bit is a transparent pixel.
    pub mask: Vec<u8>,
    /// The icon's width and height, in pixels.
    pub size: i32,
}

/// The mark: a magnifying glass, at the size the notification area uses.
///
/// The lens is a ring and the handle is a thick line away from it, and the
/// edge is antialiased by taking sixteen samples of every pixel — four a
/// side — so the mark is a mark at sixteen pixels and at thirty-two.
pub fn magnifier(size: i32) -> Pixels {
    let size = size.max(8);
    let side = size as f32;
    let mut colour = vec![0u8; (size as usize) * (size as usize) * 4];
    let mut mask = vec![0u8; mask_stride(size as usize) * size as usize];

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
            if alpha == 0 {
                set_mask_bit(&mut mask, x as usize, y as usize, size as usize);
            }
        }
    }

    Pixels { colour, mask, size }
}

/// Whether the point is on the mark.
///
/// The distances are compared squared, and the diagonal is measured with
/// the axis and the across: the square root is a `std` method and this
/// crate is `no_std`, so the mark is measured the way the crate can.
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

/// The mask's rows are padded to a word, the way the platform reads them.
///
/// The mask is given to `CreateBitmap`, which makes a device-dependent
/// bitmap: a one-bit device-dependent picture pads its rows to a word — two
/// bytes — and not to the four bytes a device-independent one uses. The tree
/// wrote four-byte rows and the platform read the tree's row *n* as its own
/// row *2n*, so the icon's mask was garbage at every size where the two
/// paddings disagree: sixteen pixels, and thirty-six, forty and forty-eight.
fn mask_stride(size: usize) -> usize {
    size.div_ceil(16) * 2
}

/// Set one bit of the mask: a transparent pixel.
fn set_mask_bit(mask: &mut [u8], x: usize, y: usize, size: usize) {
    let stride = mask_stride(size);
    let row = y * stride;
    let byte = row + x / 8;
    mask[byte] |= 0x80 >> (x % 8);
}

/// The icon the launcher leaves in the notification area.
pub struct Tray {
    /// The icon the shell draws.
    icon: HICON,
    /// The shell's own record of it.
    ///
    /// The record is a box: it is the shell's struct and it is nearly a
    /// kilobyte, and the stack is not the place for it.
    data: alloc::boxed::Box<NOTIFYICONDATAW>,
    /// The message the shell broadcasts when the notification area is made
    /// again.
    ///
    /// The number is the platform's and it changes with the session, so it
    /// is asked for and never written down. The shell forgets the icons it
    /// was given when it is made again, and a launcher that does not put
    /// its icon back is a launcher that disappeared.
    taskbar_created: UINT,
}

impl Tray {
    /// Put the icon in the notification area.
    ///
    /// The answer is the shell's: an icon that is not there is a launcher
    /// with no face, and the caller is told so rather than left to guess.
    pub fn new(hwnd: HWND, text: &Text) -> Result<Tray, String> {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
        let pixels = magnifier(size);
        let icon = build_icon(&pixels)?;

        let mut data = alloc::boxed::Box::new(NOTIFYICONDATAW {
            cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as DWORD,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: icon,
            szTip: [0; 128],
            dwState: 0,
            dwStateMask: 0,
            szInfo: [0; 256],
            uVersion: NOTIFYICON_VERSION_4,
            szInfoTitle: [0; 64],
            dwInfoFlags: 0,
            guidItem: GUID {
                Data1: 0,
                Data2: 0,
                Data3: 0,
                Data4: [0; 8],
            },
            hBalloonIcon: core::ptr::null_mut(),
        });
        write_tip(&mut data.szTip, text.tray_tip);

        let taskbar_created = unsafe {
            let name: Vec<u16> = "TaskbarCreated\0".encode_utf16().collect();
            RegisterWindowMessageW(name.as_ptr())
        };

        let tray = Tray {
            icon,
            data,
            taskbar_created,
        };
        tray.add()?;
        Ok(tray)
    }

    /// Put the icon in the notification area.
    ///
    /// The shell forgets the icons it was given when it is made again, and
    /// this is how the icon comes back.
    fn add(&self) -> Result<(), String> {
        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &*self.data) };
        if ok == 0 {
            return Err("the notification area refused the icon".to_string());
        }
        // The version is told after the icon is there: the shell reads the
        // version out of the same record, and an icon that was added
        // without it answers the clicks with the old numbers.
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &*self.data) };
        Ok(())
    }

    /// The message the shell broadcasts when the notification area is made
    /// again.
    pub fn taskbar_created(&self) -> UINT {
        self.taskbar_created
    }

    /// Change the tip the shell shows when the pointer rests on the icon.
    ///
    /// The tip is a word the user reads, and the words are the product's —
    /// one column per language, like every other word the launcher says.
    pub fn set_tip(&mut self, tip: &str) {
        write_tip(&mut self.data.szTip, tip);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &*self.data) };
    }

    /// Put the icon back, after the shell was made again.
    pub fn restore(&self) -> Result<(), String> {
        self.add()
    }

    /// Open the menu at the pointer and say what was chosen.
    ///
    /// The menu is the platform's own, and the answer is the item the user
    /// picked — or nothing, when the user walked away from it.
    pub fn menu(&self, hwnd: HWND, text: &Text) -> Option<Command> {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return None;
            }
            let search: Vec<u16> = text.tray_search.encode_utf16().chain(Some(0)).collect();
            let settings: Vec<u16> = text.tray_settings.encode_utf16().chain(Some(0)).collect();
            let quit: Vec<u16> = text.tray_quit.encode_utf16().chain(Some(0)).collect();
            AppendMenuW(menu, MF_STRING, 1, search.as_ptr());
            AppendMenuW(menu, MF_STRING, 2, settings.as_ptr());
            AppendMenuW(menu, MF_STRING, 3, quit.as_ptr());

            let mut at = POINT { x: 0, y: 0 };
            GetCursorPos(&mut at);
            // The window is brought to the front first, or the menu does not
            // close when the user clicks away from it: the platform dismisses
            // a menu whose owner is the foreground window.
            SetForegroundWindow(hwnd);
            let chosen = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                at.x,
                at.y,
                0,
                hwnd,
                core::ptr::null(),
            );
            DestroyMenu(menu);
            // The menu asks the thread to look once more before it closes,
            // so that the window it belongs to is the foreground one and the
            // menu goes away when the user clicks somewhere else. Without
            // this the menu stays on the screen after the user walked away
            // from it.
            PostMessageW(hwnd, WM_NULL, 0, 0);

            match chosen {
                1 => Some(Command::Search),
                2 => Some(Command::Settings),
                3 => Some(Command::Quit),
                _ => None,
            }
        }
    }
}

/// The icon goes away with the tray.
///
/// The shell forgets an icon when the window behind it is gone, and the
/// icon's own handle is the tree's to give back: a GDI object that is
/// never destroyed is a GDI object the process is holding for nothing.
impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &*self.data);
            DestroyIcon(self.icon);
        }
    }
}

/// The icon, out of the tree's own pixels.
///
/// The platform wants a mask and a colour, and it draws them as one icon:
/// the mask says which of the pixels the icon draws nothing on, and the
/// colour carries the picture and its own alpha. The answer is the icon, and
/// the two bitmaps are given back: the platform copied them into the icon it
/// made.
///
/// The colour is a section and not a device-dependent bitmap: the alpha of
/// a thirty-two bit picture is a channel of its own, and a device-dependent
/// bitmap has no channel of its own — the platform would draw the shape and
/// lose the soft edges.
fn build_icon(pixels: &Pixels) -> Result<HICON, String> {
    unsafe {
        let screen = GetDC(core::ptr::null_mut());
        if screen.is_null() {
            return Err("the screen has no context".to_string());
        }
        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let colour = CreateDIBSection(
            screen,
            &colour_info(pixels.size),
            DIB_RGB_COLORS,
            &mut bits,
            core::ptr::null_mut(),
            0,
        );
        ReleaseDC(core::ptr::null_mut(), screen);
        if colour.is_null() || bits.is_null() {
            return Err("the colour bitmap could not be made".to_string());
        }
        core::ptr::copy_nonoverlapping(
            pixels.colour.as_ptr(),
            bits.cast::<u8>(),
            pixels.colour.len(),
        );

        let mask = CreateBitmap(
            pixels.size,
            pixels.size,
            1,
            1,
            pixels.mask.as_ptr() as *const core::ffi::c_void,
        );
        if mask.is_null() {
            DeleteObject(colour);
            return Err("the mask could not be made".to_string());
        }

        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            // The mask first and the colour second: the platform's own
            // header names them in that order. The other way round is the
            // one that reads the colour's alpha as a mask, and the icon
            // comes out as black and white stripes.
            hbmMask: mask,
            hbmColor: colour,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(colour);
        DeleteObject(mask);
        if icon.is_null() {
            return Err("the icon could not be made".to_string());
        }
        Ok(icon)
    }
}

/// The colour bitmap's own header: thirty-two bits a pixel, top row first.
///
/// The height is negative so that the first row of the bitmap is the top of
/// the picture: a picture that grows upwards would come out upside down.
fn colour_info(size: i32) -> BitmapInfo {
    BitmapInfo {
        header: BitmapInfoHeader {
            size: core::mem::size_of::<BitmapInfoHeader>() as u32,
            width: size,
            height: -size,
            planes: 1,
            bits: 32,
            compression: BI_RGB,
            image_size: 0,
            x_pixels_per_meter: 0,
            y_pixels_per_meter: 0,
            colors_used: 0,
            colors_important: 0,
        },
        colors: [0; 1],
    }
}

/// Write the tip the shell shows when the pointer rests on the icon.
///
/// The buffer is cleared first: the tip is written over the one that was
/// there before — the tree writes the English tip when the icon is made and
/// the language's own over it — and a shorter tip would otherwise keep the
/// tail of the longer one, and the shell would show the user two sentences
/// at once.
///
/// The last slot is the terminator's: the shell reads the tip as a string,
/// and a tip that filled the whole field would have no terminator of its
/// own.
fn write_tip(tip: &mut [u16; 128], text: &str) {
    tip.fill(0);
    let last = tip.len() - 1;
    for (slot, ch) in tip[..last].iter_mut().zip(text.encode_utf16()) {
        *slot = ch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tray's callback has a number of its own.
    ///
    /// It used to be `WM_APP+2`, the same id the window gives `WM_SHOW`:
    /// a second launcher posts that id, and the shell sends this one, so
    /// one message could be answered as the other. The next free number
    /// is `WM_APP+4`, and none of the window's own messages use it.
    #[test]
    fn the_tray_callback_has_its_own_message() {
        assert_eq!(WM_TRAY, WM_APP + 4);
        assert_ne!(WM_TRAY, crate::window::WM_TOGGLE);
        assert_ne!(WM_TRAY, crate::window::WM_SHOW);
        assert_ne!(WM_TRAY, crate::window::WM_WAKE);
    }

    /// What the platform drew.
    ///
    /// The platform is the judge of an icon: it takes the mask and the
    /// colour and draws them as one picture, and what it draws is the truth
    /// about the tree's bytes. The pixels are kept so that the picture can
    /// be compared with the mark the tree drew, and the text is the picture
    /// in characters, so that a failure says what the icon looked like and
    /// not only that it was wrong.
    struct Picture {
        /// The pixels, blue, green, red, alpha — top row first.
        pixels: alloc::vec::Vec<u8>,
        /// The picture in characters.
        text: alloc::string::String,
    }

    /// Draw an icon into a bitmap of our own and read the pixels back.
    fn draw(icon: HICON) -> Picture {
        let mut text = alloc::string::String::new();
        let mut pixels: alloc::vec::Vec<u8> = alloc::vec![0; 16 * 16 * 4];
        unsafe {
            let screen = GetDC(core::ptr::null_mut());
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(core::ptr::null_mut(), screen);
            assert!(!dc.is_null(), "the picture's own context could not be made");
            let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
            let info = BitmapInfo {
                header: BitmapInfoHeader {
                    size: core::mem::size_of::<BitmapInfoHeader>() as u32,
                    width: 16,
                    height: -16,
                    planes: 1,
                    bits: 32,
                    compression: BI_RGB,
                    image_size: 0,
                    x_pixels_per_meter: 0,
                    y_pixels_per_meter: 0,
                    colors_used: 0,
                    colors_important: 0,
                },
                colors: [0; 1],
            };
            let bitmap = CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                core::ptr::null_mut(),
                0,
            );
            assert!(
                !bitmap.is_null() && !bits.is_null(),
                "the picture's own bitmap could not be made"
            );
            // The bitmap has to be the one the context draws into: a
            // context that draws into its own one-pixel bitmap leaves this
            // one untouched, and the reading is of nothing.
            let previous = SelectObject(dc, bitmap);
            DrawIconEx(dc, 0, 0, icon, 16, 16, 0, core::ptr::null_mut(), DI_NORMAL);
            SelectObject(dc, previous);

            let read = core::slice::from_raw_parts(bits.cast::<u8>(), 16 * 16 * 4);
            pixels.copy_from_slice(read);
            for y in 0..16 {
                for x in 0..16 {
                    let at = (y * 16 + x) * 4;
                    let alpha = pixels[at + 3];
                    let b = pixels[at];
                    let g = pixels[at + 1];
                    let r = pixels[at + 2];
                    let ch = if alpha == 0 {
                        '.'
                    } else if b as i32 > r as i32 + 40 {
                        'B'
                    } else if r > 200 && g > 200 && b > 200 {
                        'W'
                    } else {
                        'k'
                    };
                    text.push(ch);
                }
                text.push('\n');
            }
            DeleteObject(bitmap);
            DeleteDC(dc);
        }
        Picture { pixels, text }
    }

    /// The icon the platform makes is the mark, in the accent's blue.
    ///
    /// The tree's bytes are not the proof: the platform assembles the icon
    /// out of them, and what it draws is what the user sees in the
    /// notification area. Measured on 2026-10-31: with the two handles the
    /// wrong way round the platform read the colour as the mask, and the
    /// icon came out as black and white stripes — the alpha of the mark,
    /// one bit at a time. This is the gate that says the icon is the mark
    /// and that it is blue.
    #[test]
    fn the_icon_is_the_mark_in_colour() {
        let pixels = magnifier(16);
        let icon = build_icon(&pixels).expect("the icon could not be made");
        let drawn = draw(icon);
        unsafe {
            DestroyIcon(icon);
        }

        let mut missing = 0u32;
        let mut extra = 0u32;
        let mut not_blue = 0u32;
        for y in 0..16 {
            for x in 0..16 {
                let at = (y * 16 + x) * 4;
                let ours = pixels.colour[at + 3];
                let theirs = drawn.pixels[at + 3];
                if ours > 0 && theirs == 0 {
                    missing += 1;
                }
                if ours == 0 && theirs > 0 {
                    extra += 1;
                }
                if ours > 0 && theirs > 0 {
                    // The accent is blue with green under it and red under
                    // that, and the platform's drawing is premultiplied by
                    // the alpha — so the hue is the test, and the margin
                    // between the channels is not: a faint edge pixel is
                    // small in every channel and blue in all of them.
                    let b = drawn.pixels[at];
                    let g = drawn.pixels[at + 1];
                    let r = drawn.pixels[at + 2];
                    if b <= r || b <= g {
                        not_blue += 1;
                    }
                }
            }
        }

        assert!(
            missing == 0,
            "{missing} pixels of the mark are missing from the icon\n{}",
            drawn.text
        );
        assert!(
            extra == 0,
            "{extra} pixels that are not the mark are in the icon\n{}",
            drawn.text
        );
        assert!(
            not_blue == 0,
            "{not_blue} pixels of the icon are not the accent's blue\n{}",
            drawn.text
        );
    }

    /// The mark is a mark.
    ///
    /// An icon that is empty is an icon that is not there, and an icon that
    /// is a solid square is a square with a magnifying glass's name on it.
    #[test]
    fn the_mark_is_drawn_and_not_a_square() {
        let pixels = magnifier(16);
        assert_eq!(pixels.colour.len(), 16 * 16 * 4);
        let opaque = pixels.colour.chunks(4).filter(|p| p[3] > 0).count();
        assert!(opaque > 20, "the mark is too small: {opaque} pixels");
        assert!(opaque < 16 * 16, "the mark fills the whole icon");
    }

    /// The middle of the lens is see-through.
    ///
    /// The lens is a ring, and a ring that is filled is a disc: the mark
    /// would read as a lollipop.
    #[test]
    fn the_lens_is_a_ring() {
        let pixels = magnifier(16);
        let at = |x: usize, y: usize| -> u8 { pixels.colour[(y * 16 + x) * 4 + 3] };
        assert_eq!(at(6, 6), 0, "the middle of the lens is not see-through");
        assert!(at(4, 4) > 0, "the ring is not on the mark");
    }

    /// The mask says where the shell draws nothing.
    ///
    /// A mask that is empty makes the icon a rectangle of the taskbar's own
    /// colour, and a mask that is full makes it invisible. The bits are what
    /// the platform reads, so the bits are what is counted: the padding of a
    /// row is not a pixel of the mask, and a count of bytes would have
    /// called a mask that hides everything a mask that hides most of it.
    #[test]
    fn the_mask_covers_what_the_picture_does_not() {
        let pixels = magnifier(16);
        let set: usize = pixels.mask.iter().map(|b| b.count_ones() as usize).sum();
        let whole = (pixels.size * pixels.size) as usize;
        assert!(set > 0, "the mask says nothing");
        assert!(set < whole, "the mask hides everything");
    }

    /// The rows of the mask are padded the way the platform reads them.
    ///
    /// The mask is a device-dependent bitmap, and a one-bit device-dependent
    /// picture pads its rows to a word — two bytes — and not to the four
    /// bytes a device-independent one uses. These are the platform's
    /// numbers: sixteen pixels is two bytes a row, twenty is four, and
    /// thirty-two is four.
    #[test]
    fn the_mask_rows_are_padded_to_a_word() {
        assert_eq!(mask_stride(16), 2);
        assert_eq!(mask_stride(20), 4);
        assert_eq!(mask_stride(32), 4);
    }

    /// A shorter tip leaves no tail of the longer one.
    ///
    /// The tip is written over the one that was there before, and the buffer
    /// is the shell's: the tree writes the English tip when the icon is made
    /// and the language's own over it, and a tip that kept the tail of the
    /// last one would show the user two sentences at once.
    #[test]
    fn a_shorter_tip_leaves_no_tail() {
        let mut tip = [0u16; 128];
        write_tip(&mut tip, "rgxy — search files, apps or commands");
        write_tip(&mut tip, "rgxy — buscar arquivos");

        let said: alloc::vec::Vec<u16> = "rgxy — buscar arquivos".encode_utf16().collect();
        for (slot, ch) in tip.iter().zip(said.iter()) {
            assert_eq!(*slot, *ch, "the tip does not say what it should");
        }
        // Everything past the text is the terminator's: the shell reads the
        // tip as a string, and a tail of the last one would be part of it.
        for slot in tip.iter().skip(said.len()) {
            assert_eq!(*slot, 0, "the tip keeps the tail of the last one");
        }
    }

    /// A tip that fills the buffer still ends where the buffer ends.
    ///
    /// The field is a hundred and twenty-eight units and the shell reads the
    /// tip as a string: a tip that filled the whole field would have no
    /// terminator of its own, and the shell would read past the field for
    /// the rest of it.
    #[test]
    fn a_tip_that_fills_the_buffer_is_terminated() {
        let mut tip = [0u16; 128];
        write_tip(&mut tip, &"x".repeat(200));

        assert_eq!(tip[127], 0, "the tip has no terminator of its own");
        for slot in tip.iter().take(127) {
            assert_eq!(*slot, u16::from(b'x'), "the tip is not what was written");
        }
    }

    /// The icon is drawn at the size the shell asked for.
    #[test]
    fn the_icon_is_the_size_it_was_asked_for() {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
        let pixels = magnifier(size);
        assert!(pixels.size >= 8, "the icon is too small to draw");
        assert_eq!(
            pixels.colour.len(),
            (pixels.size * pixels.size * 4) as usize
        );
    }

    /// The mouse's message is read out of the low word.
    ///
    /// The shell sends the mouse's own message together with the icon's
    /// place, and the place is in the high word: the word as it comes is
    /// 0x0202 with a place on top of it, and a comparison with the whole
    /// word never matches. An icon whose clicks are never matched is an
    /// icon that does not answer.
    #[test]
    fn the_mouses_message_comes_out_of_the_low_word() {
        // The shell's own example: the left button, with the icon at
        // x = 0x0123.
        let lparam = 0x0123_0202isize;
        assert_eq!(mouse_message(lparam), WM_LBUTTONUP);

        // The right button, and the context menu the shell sends instead of
        // it.
        assert_eq!(mouse_message(0x0400_0205isize), WM_RBUTTONUP);
        assert_eq!(mouse_message(0x0500_007Bisize), WM_CONTEXTMENU);

        // And the whole word is NOT the message — which is the defect this
        // rule is here to keep from coming back.
        assert_ne!(0x0123_0202u32 as UINT, WM_LBUTTONUP);
    }
}
