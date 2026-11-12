// SPDX-License-Identifier: Apache-2.0

//! The Win32 calls the launcher makes, declared by hand.
//!
//! The tree does not link the `windows` crate: it does its platform calls
//! by hand, in the seam, once — and this is the seam for the launcher.
//! Every type here is the platform's own, whole: a struct the platform
//! writes is the platform's struct, and a field left out is a buffer the
//! platform writes past.
//!
//! Only the calls the launcher uses are declared. A declaration that is
//! not used is a declaration that can be wrong.

#![allow(non_snake_case, non_camel_case_types)]
// The names here are the **platform's** names: `BOOL`, `DWORD`, `HDC`,
// `WNDCLASSEXW`. Renaming them to satisfy a lint would make the seam
// harder to read against the documentation it implements, and a reader
// who knows Win32 would have to translate every name in their head. The
// lint is right about Rust and wrong about this file.
#![allow(clippy::upper_case_acronyms)]

use core::ffi::c_void;

pub type BOOL = i32;
pub type UINT = u32;
pub type DWORD = u32;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;

pub type HANDLE = *mut c_void;
pub type HINSTANCE = *mut c_void;
pub type HWND = *mut c_void;
pub type HDC = *mut c_void;
pub type HBRUSH = *mut c_void;
pub type HFONT = *mut c_void;
pub type HICON = *mut c_void;
pub type HBITMAP = *mut c_void;
pub type HRGN = *mut c_void;
pub type HHOOK = *mut c_void;
pub type HMENU = *mut c_void;
pub type HMONITOR = *mut c_void;

/// The process's own DPI awareness: a pointer-sized value the platform
/// takes and hands back.
pub type DPI_AWARENESS_CONTEXT = isize;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: UINT,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: DWORD,
    pub pt: POINT,
}

#[repr(C)]
pub struct PAINTSTRUCT {
    pub hdc: HDC,
    pub fErase: BOOL,
    pub rcPaint: RECT,
    pub fRestore: BOOL,
    pub fIncUpdate: BOOL,
    pub rgbReserved: [u8; 32],
}

#[repr(C)]
pub struct WNDCLASSEXW {
    pub cbSize: UINT,
    pub style: UINT,
    pub lpfnWndProc: Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HINSTANCE,
    pub hIcon: HANDLE,
    pub hCursor: HANDLE,
    pub hbrBackground: HBRUSH,
    pub lpszMenuName: *const u16,
    pub lpszClassName: *const u16,
    pub hIconSm: HANDLE,
}

#[repr(C)]
pub struct KBDLLHOOKSTRUCT {
    pub vkCode: DWORD,
    pub scanCode: DWORD,
    pub flags: DWORD,
    pub time: DWORD,
    pub dwExtraInfo: usize,
}

/// What the shell says about a file.
///
/// The struct is the shell's, whole: the names are the buffers it fills
/// in, and the icon is the handle it hands over.
#[repr(C)]
pub struct SHFILEINFOW {
    pub hIcon: HICON,
    pub iIcon: i32,
    pub dwAttributes: DWORD,
    pub szDisplayName: [u16; 260],
    pub szTypeName: [u16; 80],
}

/// What the platform says about a monitor.
///
/// The struct is the platform's, whole: the rectangles are where the
/// monitor is and where the work is, and the size is what the platform
/// writes back.
#[repr(C)]
pub struct MONITORINFO {
    pub cbSize: DWORD,
    /// The whole monitor, taskbar and bars included.
    pub rcMonitor: RECT,
    /// The part of the monitor the bars did not take.
    pub rcWork: RECT,
    pub dwFlags: DWORD,
}

/// The platform's own identifier, as the shell carries it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GUID {
    pub Data1: u32,
    pub Data2: u16,
    pub Data3: u16,
    pub Data4: [u8; 8],
}

/// The icon the shell draws, as the platform assembles it.
///
/// The struct is the platform's, whole: the mask comes first and the colour
/// second — the platform's own header says so — and the two handles sit at
/// sixteen and twenty-four, because the struct is laid out the ordinary way.
///
/// **The order is the whole story.** The platform's own `WinUser.h` names the
/// fields `hbmMask` and `hbmColor`, mask first; a tree that puts the colour
/// in the first handle has its colour bitmap read as the mask, and a
/// thirty-two bit colour bitmap read as a one bit mask is its alpha bytes
/// read as bits. Measured on 2026-10-31: the icon in the notification area
/// came out as black and white stripes instead of the magnifier, and the
/// stripes were the alpha of the mark, one bit at a time.
///
/// The sizes are asserted where the struct is declared, so a layout that
/// drifts fails the build and not the notification area.
#[repr(C)]
pub struct ICONINFO {
    pub fIcon: BOOL,
    pub xHotspot: DWORD,
    pub yHotspot: DWORD,
    /// The mask: the pixels the icon draws nothing on.
    pub hbmMask: HICON,
    /// The colour: the picture itself.
    pub hbmColor: HICON,
}

/// The shell's record of an icon in the notification area.
///
/// The struct is the shell's, whole — and the shell writes all of it. The
/// tip, the balloon and the balloon's title are buffers it fills in, and a
/// struct that left one out would be a shorter struct than the shell
/// writes: the stack that holds it, and the shell's own read past the end
/// of it.
///
/// The sizes are asserted where the struct is declared, so a field left out
/// fails the build and not the notification area.
#[repr(C)]
pub struct NOTIFYICONDATAW {
    pub cbSize: DWORD,
    pub hWnd: HWND,
    pub uID: UINT,
    pub uFlags: UINT,
    pub uCallbackMessage: UINT,
    pub hIcon: HICON,
    pub szTip: [u16; 128],
    pub dwState: DWORD,
    pub dwStateMask: DWORD,
    pub szInfo: [u16; 256],
    /// The version of the icon, or the balloon's timeout — the platform's
    /// own union, and both halves are a word wide.
    pub uVersion: UINT,
    pub szInfoTitle: [u16; 64],
    pub dwInfoFlags: DWORD,
    pub guidItem: GUID,
    pub hBalloonIcon: HICON,
}

/// The size of the shell's record, on this target.
///
/// The platform reads the struct by its own layout and checks the size it
/// is given, so the size that goes in `cbSize` has to be the platform's own
/// — and the platform's own is what this asserts.
const _: () = assert!(core::mem::size_of::<NOTIFYICONDATAW>() == 976);
const _: () = assert!(core::mem::offset_of!(NOTIFYICONDATAW, hWnd) == 8);
const _: () = assert!(core::mem::offset_of!(NOTIFYICONDATAW, szTip) == 40);
const _: () = assert!(core::mem::offset_of!(NOTIFYICONDATAW, szInfo) == 304);
const _: () = assert!(core::mem::offset_of!(NOTIFYICONDATAW, szInfoTitle) == 820);
const _: () = assert!(core::mem::offset_of!(NOTIFYICONDATAW, guidItem) == 952);
const _: () = assert!(core::mem::size_of::<ICONINFO>() == 32);
const _: () = assert!(core::mem::offset_of!(ICONINFO, hbmMask) == 16);
const _: () = assert!(core::mem::offset_of!(ICONINFO, hbmColor) == 24);
const _: () = assert!(core::mem::size_of::<GUID>() == 16);

/// The bitmap's own header, as the platform writes it.
///
/// A struct the platform writes is the platform's struct, whole: the
/// header is the size the platform expects, and the sizes are the
/// platform's own.
#[repr(C)]
pub struct BitmapInfoHeader {
    pub size: u32,
    pub width: i32,
    pub height: i32,
    pub planes: u16,
    pub bits: u16,
    pub compression: u32,
    pub image_size: u32,
    pub x_pixels_per_meter: i32,
    pub y_pixels_per_meter: i32,
    pub colors_used: u32,
    pub colors_important: u32,
}

/// The bitmap's information: the header, and the room for the colours.
#[repr(C)]
pub struct BitmapInfo {
    pub header: BitmapInfoHeader,
    pub colors: [u32; 1],
}

/// The size of a surface, in pixels.
#[repr(C)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

/// The blend the compositor uses for a layered window.
#[repr(C)]
pub struct BlendFunction {
    pub operation: u8,
    pub flags: u8,
    pub constant_alpha: u8,
    pub alpha_format: u8,
}

// --- the constants the launcher uses -----------------------------------------

pub const WS_POPUP: u32 = 0x8000_0000;
pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
pub const CS_VREDRAW: u32 = 0x0001;
pub const CS_HREDRAW: u32 = 0x0002;

/// The error Windows gives when a window class is registered twice.
///
/// It is not a failure of the class: the class is there, and whoever
/// asked for it has what it asked for.
pub const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;

/// The error Windows gives when a named object exists already.
///
/// It is the platform's answer to "is this launcher the only one": the
/// object was there, so another process has it.
pub const ERROR_ALREADY_EXISTS: DWORD = 183;
pub const CS_DROPSHADOW: u32 = 0x0002_0000;

pub const SW_HIDE: i32 = 0;
pub const SW_SHOWNOACTIVATE: i32 = 4;

pub const WM_DESTROY: UINT = 0x0002;
pub const WM_SIZE: UINT = 0x0005;
pub const WM_PAINT: UINT = 0x000F;
pub const WM_CLOSE: UINT = 0x0010;
pub const WM_ERASEBKGND: UINT = 0x0014;
pub const WM_TIMER: UINT = 0x0113;
/// The platform's own message for a hotkey the process registered: it
/// is **posted** to the queue, so the pump is where it arrives and the
/// window procedure never sees it.
pub const WM_HOTKEY: UINT = 0x0312;
pub const WM_KEYDOWN: UINT = 0x0100;
pub const WM_KEYUP: UINT = 0x0101;
pub const WM_CHAR: UINT = 0x0102;
pub const WM_SYSKEYDOWN: UINT = 0x0104;
pub const WM_SYSKEYUP: UINT = 0x0105;
/// The application stopped being the active one.
pub const WM_ACTIVATEAPP: UINT = 0x001C;
pub const WM_APP: UINT = 0x8000;
/// The left button went up — on the notification area's icon, this is the
/// shell telling the window that the user clicked its face.
pub const WM_LBUTTONUP: UINT = 0x0202;
/// The left button was double clicked: the same face, twice.
pub const WM_LBUTTONDBLCLK: UINT = 0x0203;
/// The right button went up.
pub const WM_RBUTTONUP: UINT = 0x0205;
/// The context menu was asked for.
pub const WM_CONTEXTMENU: UINT = 0x007B;
/// The message with no meaning, sent to make the thread look once.
pub const WM_NULL: UINT = 0x0000;

/// What the shell is told to do with the icon.
pub const NIM_ADD: DWORD = 0x0000_0000;
pub const NIM_MODIFY: DWORD = 0x0000_0001;
pub const NIM_DELETE: DWORD = 0x0000_0002;
pub const NIM_SETVERSION: DWORD = 0x0000_0004;

/// What the shell is told about the icon.
pub const NIF_MESSAGE: UINT = 0x0000_0001;
pub const NIF_ICON: UINT = 0x0000_0002;
pub const NIF_TIP: UINT = 0x0000_0004;

/// The version of the icon the shell is told about.
///
/// The fourth version is the one that answers the clicks with the mouse
/// message in `lParam` and the icon's own coordinates in `wParam`, which is
/// what a menu needs: the menu opens where the pointer is.
pub const NOTIFYICON_VERSION_4: UINT = 4;

/// A menu item: a word, and the command it runs.
pub const MF_STRING: UINT = 0x0000_0000;
/// The menu is for the keyboard as well as the mouse.
pub const TPM_RIGHTBUTTON: UINT = 0x0000_0002;
/// The menu gives back the item that was chosen.
pub const TPM_RETURNCMD: UINT = 0x0100;
/// The menu takes the pointer's focus while it is up.
pub const TPM_NONOTIFY: UINT = 0x0080;

/// The system's small icon size — the notification area's own.
pub const SM_CXSMICON: i32 = 49;

pub const VK_BACK: u32 = 0x08;
pub const VK_TAB: u32 = 0x09;
pub const VK_RETURN: u32 = 0x0D;
pub const VK_SHIFT: u32 = 0x10;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_MENU: u32 = 0x12;
pub const VK_PRIOR: u32 = 0x21;
pub const VK_NEXT: u32 = 0x22;
pub const VK_END: u32 = 0x23;
pub const VK_HOME: u32 = 0x24;
pub const VK_LEFT: u32 = 0x25;
pub const VK_UP: u32 = 0x26;
pub const VK_RIGHT: u32 = 0x27;
pub const VK_DOWN: u32 = 0x28;
pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_DELETE: u32 = 0x2E;
/// The left and the right Control keys.
///
/// The hook hears the generic Control as well as the left and the right
/// one: a keyboard that does not say which side was pressed says the
/// generic one, and a press the hook does not hear breaks the gesture
/// that opens the window.
pub const VK_LCONTROL: u32 = 0xA2;
pub const VK_RCONTROL: u32 = 0xA3;
pub const VK_LSHIFT: u32 = 0xA0;
pub const VK_RSHIFT: u32 = 0xA1;
pub const VK_LMENU: u32 = 0xA4;
pub const VK_RMENU: u32 = 0xA5;

pub const SWP_NOMOVE: UINT = 0x0002;
pub const SWP_NOACTIVATE: UINT = 0x0010;

pub const HWND_TOPMOST: HWND = -1isize as HWND;

/// The window's own data: the pointer the launcher keeps there.
pub const GWL_USERDATA: i32 = -21;

pub const TRANSPARENT: i32 = 1;
pub const CLEARTYPE_QUALITY: u8 = 5;

pub const DEFAULT_CHARSET: u8 = 1;
pub const OUT_DEFAULT_PRECIS: u8 = 0;
pub const CLIP_DEFAULT_PRECIS: u8 = 0;
pub const DEFAULT_PITCH: u8 = 0;
pub const FF_DONTCARE: u8 = 0;
pub const FW_NORMAL: i32 = 400;
pub const FW_SEMIBOLD: i32 = 600;

pub const SM_CXSCREEN: i32 = 0;
pub const SM_CYSCREEN: i32 = 1;

/// Per-monitor v2: the window's own scale is the monitor's, and the
/// window is told when it moves to another monitor.
///
/// The value is the platform's own: a pointer-sized `-4`.
pub const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: DPI_AWARENESS_CONTEXT = -4;

/// The monitor that has the point, or the nearest one when it has none.
pub const MONITOR_DEFAULTTONEAREST: DWORD = 0x0000_0002;

pub const WH_KEYBOARD_LL: i32 = 13;
pub const HC_ACTION: i32 = 0;

/// The shell's icon for a file, and the small one.
pub const SHGFI_ICON: u32 = 0x0000_0100;
pub const SHGFI_SMALLICON: u32 = 0x0000_0001;
/// The shell answers from the spelling alone, and does not touch the disk.
pub const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;

/// The file is a folder: the shell's own word for it.
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// The file is an ordinary one.
pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

/// The font the context has selected.
pub const OBJ_FONT: UINT = 0;

/// The bitmap's colours are the ones the platform writes.
pub const DIB_RGB_COLORS: u32 = 0;

/// The bitmap is not compressed.
pub const BI_RGB: u32 = 0;

/// The icon is drawn at its own size.
pub const DI_NORMAL: u32 = 0x0003;

/// The window's corner radius, in pixels, as DWM wants it.
pub const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;

/// The backdrop the desktop window manager gives the window, asked for as
/// a number: this is the question that has an answer.
pub const DWMWA_SYSTEM_BACKDROP_TYPE: u32 = 38;

/// Whether the window is on the screen, or the compositor put it away.
///
/// The answer is what tells a window that exists from a window the user
/// cannot see: a cloaked window is a window the compositor is not
/// drawing, and it is not drawn for anybody.
pub const DWMWA_CLOAKED: u32 = 14;

/// The window's bounds on the glass, with the invisible frame around it.
///
/// The window's own rectangle is the one the platform gives, and it is
/// the one the compositor draws: a window whose two rectangles are not
/// the same is a window that is not where it says it is.
pub const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;

/// The backdrops that are really there.
///
/// `AUTO` is the system deciding, and "the system deciding" is not the
/// same thing as a blur: the window has to know which one it is before
/// it leaves its own background empty.
pub const DWMSBT_AUTO: i32 = 0;
pub const DWMSBT_NONE: i32 = 1;
/// Mica: the tinted, barely blurred backdrop of a main window.
pub const DWMSBT_MAIN_WINDOW: i32 = 2;
/// Acrylic: the blurred backdrop of a transient window.
pub const DWMSBT_TRANSIENT_WINDOW: i32 = 3;
/// Mica, the variant that tabs use.
pub const DWMSBT_TABBED_WINDOW: i32 = 4;
/// Round the corners the way Win11 does.
pub const DWMWCP_ROUND: i32 = 2;

/// The accent policy that asks for the blur behind the window.
#[repr(C)]
pub struct AccentPolicy {
    pub accent_state: u32,
    pub accent_flags: u32,
    pub gradient_color: u32,
    pub animation_id: u32,
}

/// No backdrop at all: the accent is off.
pub const ACCENT_ENABLE_NONE: u32 = 0;
/// The blurred backdrop: Win11's host backdrop, and the Win10 blur.
pub const ACCENT_ENABLE_BLURBEHIND: u32 = 3;
pub const ACCENT_ENABLE_HOSTBACKDROP: u32 = 5;
/// The accent is drawn over the whole window, borders included.
pub const ACCENT_FLAG_DRAW_ALL_BORDERS: u32 = 2;

#[repr(C)]
pub struct WindowCompositionAttributeData {
    pub attribute: u32,
    pub data: *mut c_void,
    pub size_of_data: usize,
}

pub const WCA_ACCENT_POLICY: u32 = 19;

/// The accent policy's call is not in the import library on every SDK, so
/// it is looked up at run time rather than linked.
pub type SetWindowCompositionAttributeFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> BOOL;

// --- the calls ---------------------------------------------------------------

#[link(name = "user32")]
unsafe extern "system" {
    pub fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> u16;

    pub fn CreateWindowExW(
        dwExStyle: DWORD,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: DWORD,
        x: i32,
        y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HMENU,
        hInstance: HINSTANCE,
        lpParam: *mut c_void,
    ) -> HWND;

    pub fn DefWindowProcW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;

    pub fn DestroyWindow(hWnd: HWND) -> BOOL;

    pub fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;

    pub fn UpdateWindow(hWnd: HWND) -> BOOL;

    pub fn SetForegroundWindow(hWnd: HWND) -> BOOL;

    pub fn SetFocus(hWnd: HWND) -> HWND;

    /// The window the user's keyboard is going to.
    ///
    /// The answer is what the platform's foreground lock is about: a
    /// process that is not behind this window is refused
    /// `SetForegroundWindow`, and the launcher that does not know the
    /// answer is a launcher whose window opens without the keyboard.
    pub fn GetForegroundWindow() -> HWND;

    /// The thread that owns a window, and optionally its process.
    ///
    /// The thread is what `AttachThreadInput` joins to, so the answer is
    /// the key to the foreground lock. The process id may be null: the
    /// caller that only wants the thread does not read it.
    pub fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;

    /// Join two input queues, so that one thread may act for the other's
    /// window for the length of a call.
    ///
    /// The join has to be undone by the same thread that made it, and the
    /// pair is written in the order `(idAttach, idAttachTo)`.
    pub fn AttachThreadInput(idAttach: DWORD, idAttachTo: DWORD, fAttach: BOOL) -> BOOL;

    /// Raise a window to the top of its z-order without activating it.
    pub fn BringWindowToTop(hWnd: HWND) -> BOOL;

    /// The window of a class, by name.
    ///
    /// The class name is the launcher's own, registered by this crate: a
    /// second launcher finds the first one's window with this, because
    /// the class is the only name two processes of one launcher share.
    pub fn FindWindowW(lpClassName: *const u16, lpWindowName: *const u16) -> HWND;

    pub fn SetWindowPos(
        hWnd: HWND,
        hWndInsertAfter: HWND,
        X: i32,
        Y: i32,
        cx: i32,
        cy: i32,
        uFlags: UINT,
    ) -> BOOL;

    pub fn GetMessageW(
        lpMsg: *mut MSG,
        hWnd: HWND,
        wMsgFilterMin: UINT,
        wMsgFilterMax: UINT,
    ) -> BOOL;

    pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;

    pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;

    pub fn PostQuitMessage(nExitCode: i32);

    pub fn PostMessageW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> BOOL;

    /// Register a hotkey with the platform.
    ///
    /// The modifiers are the platform's own `MOD_*` bits and the key is
    /// a virtual-key code; the answer says whether the platform took
    /// it, and the last error is the difference between a combination
    /// another program already has and a machine that could not. The
    /// message it posts is `WM_HOTKEY`, to the window named here.
    pub fn RegisterHotKey(hWnd: HWND, id: i32, fsModifiers: UINT, vk: UINT) -> BOOL;

    /// Give a registered hotkey back.
    pub fn UnregisterHotKey(hWnd: HWND, id: i32) -> BOOL;

    pub fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;

    pub fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;

    pub fn GetSystemMetrics(nIndex: i32) -> i32;

    /// Make the process aware of the monitor's own scale.
    ///
    /// The awareness is asked once, before the first window exists: an
    /// awareness set after the window is on the screen is not an
    /// awareness the window can use.
    pub fn SetProcessDpiAwarenessContext(value: DPI_AWARENESS_CONTEXT) -> BOOL;

    /// The older way of saying the same thing.
    ///
    /// A platform that does not know the newer call knows this one, and a
    /// process that is aware of the screen's scale is a window that is not
    /// a stretched picture.
    pub fn SetProcessDPIAware() -> BOOL;

    /// The scale of the monitor the window is on, in dots per inch.
    pub fn GetDpiForWindow(hwnd: HWND) -> UINT;

    /// Where the pointer is.
    pub fn GetCursorPos(lpPoint: *mut POINT) -> BOOL;

    /// The monitor that has the point.
    pub fn MonitorFromPoint(pt: POINT, dwFlags: DWORD) -> HMONITOR;

    /// The monitor the window is on.
    pub fn MonitorFromWindow(hwnd: HWND, dwFlags: DWORD) -> HMONITOR;

    /// What the platform says about a monitor.
    pub fn GetMonitorInfoW(hMonitor: HMONITOR, lpmi: *mut MONITORINFO) -> BOOL;

    pub fn SetTimer(hWnd: HWND, nIDEvent: usize, uElapse: u32, lpTimerFunc: *mut c_void) -> usize;

    pub fn KillTimer(hWnd: HWND, uIDEvent: usize) -> i32;

    pub fn BeginPaint(hWnd: HWND, lpPs: *mut PAINTSTRUCT) -> HDC;

    pub fn EndPaint(hWnd: HWND, lpPs: *const PAINTSTRUCT) -> BOOL;

    pub fn FillRect(hdc: HDC, lpRect: *const RECT, hBrush: HBRUSH) -> i32;

    pub fn FrameRect(hdc: HDC, lpRect: *const RECT, hBrush: HBRUSH) -> i32;

    pub fn DrawTextW(
        hdc: HDC,
        lpchText: *const u16,
        cchText: i32,
        lpRect: *mut RECT,
        uFormat: UINT,
    ) -> i32;

    /// Draw an icon at a place.
    ///
    /// The place comes before the icon: the platform's own header says so,
    /// and an argument list with the icon in it is a list the platform
    /// reads the other way round — the icon's own handle arrives as the
    /// place, and the place arrives as the icon.
    pub fn DrawIconEx(
        hdc: HDC,
        xLeft: i32,
        yTop: i32,
        hIcon: HICON,
        cxWidth: i32,
        cyWidth: i32,
        istepIfAniCur: UINT,
        hbrFlickerFreeDraw: HBRUSH,
        diFlags: UINT,
    ) -> BOOL;

    pub fn DestroyIcon(hIcon: HICON) -> BOOL;

    /// The icon the shell draws, out of a colour bitmap and a mask.
    ///
    /// The call is in the window library and not in the graphics one: the
    /// export table of the host says so — measured on 2026-10-31, the name
    /// is in `user32.dll` and not in `gdi32.dll` — and a name in the wrong
    /// library only links because the toolchain's default set carries the
    /// right one anyway.
    pub fn CreateIconIndirect(piconinfo: *const ICONINFO) -> HICON;

    pub fn SetWindowRgn(hWnd: HWND, hRgn: HRGN, bRedraw: BOOL) -> i32;

    pub fn SetWindowsHookExW(
        idHook: i32,
        lpfn: Option<unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT>,
        hMod: HINSTANCE,
        dwThreadId: DWORD,
    ) -> HHOOK;

    pub fn CallNextHookEx(hhk: HHOOK, nCode: i32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;

    pub fn UnhookWindowsHookEx(hhk: HHOOK) -> BOOL;

    /// The state of a key, as the system's own view of the keyboard has it.
    pub fn GetKeyState(vKey: i32) -> i16;
    /// Whether the window is on the screen.
    pub fn IsWindowVisible(hWnd: HWND) -> BOOL;
    /// Mark the window as needing to be painted again.
    pub fn InvalidateRect(hWnd: HWND, lpRect: *const RECT, bErase: BOOL) -> BOOL;
    /// The window's own rectangle, in the process's own coordinates.
    pub fn GetWindowRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
    /// The context of the whole screen.
    ///
    /// The face a font really has is only known to the context that has
    /// it selected, and the window has no context of its own until it
    /// paints.
    pub fn GetDC(hWnd: HWND) -> HDC;
    /// Give the context back.
    pub fn ReleaseDC(hWnd: HWND, hDC: HDC) -> i32;
    /// Put the window's own pixels on the screen.
    pub fn UpdateLayeredWindow(
        hWnd: HWND,
        hdcDst: HDC,
        pptDst: *const POINT,
        psize: *const Size,
        hdcSrc: HDC,
        pptSrc: *const POINT,
        crKey: u32,
        pblend: *const BlendFunction,
        dwFlags: u32,
    ) -> BOOL;
    /// The clipboard, opened for the caller.
    pub fn OpenClipboard(hWndNewOwner: HWND) -> i32;
    /// Take everything the clipboard holds.
    pub fn EmptyClipboard() -> i32;
    /// Give the clipboard back.
    pub fn CloseClipboard() -> i32;
    /// Put data in the clipboard, which takes it over.
    pub fn SetClipboardData(uFormat: u32, hMem: HANDLE) -> HANDLE;
    /// What the clipboard holds.
    pub fn GetClipboardData(uFormat: u32) -> HANDLE;
    /// The window station of the process.
    pub fn GetProcessWindowStation() -> *mut c_void;
    /// What the platform says about an object of the user's session.
    pub fn GetUserObjectInformationW(
        hObj: *mut c_void,
        nIndex: i32,
        pvInfo: *mut c_void,
        nLength: u32,
        lpnLengthNeeded: *mut u32,
    ) -> i32;

    /// The message the shell broadcasts when the notification area is made
    /// again.
    ///
    /// The number is the platform's and it changes with the session, so it
    /// is asked for and not written down: a hard-coded number is a number
    /// the platform never sends.
    pub fn RegisterWindowMessageW(lpString: *const u16) -> UINT;

    /// A menu, with nothing in it.
    pub fn CreatePopupMenu() -> HMENU;
    /// Put a word in the menu.
    ///
    /// The line between the items is the same call with `MF_SEPARATOR` and
    /// no word: the platform's menu has one way to say both.
    pub fn AppendMenuW(hMenu: HMENU, uFlags: UINT, uIDNewItem: usize, lpString: *const u16)
    -> BOOL;
    /// Show the menu at the pointer and wait.
    ///
    /// With `TPM_RETURNCMD` the answer is the item that was chosen, and
    /// zero is the answer for a menu the user walked away from.
    pub fn TrackPopupMenu(
        hMenu: HMENU,
        uFlags: UINT,
        x: i32,
        y: i32,
        nReserved: i32,
        hWnd: HWND,
        lpRect: *const RECT,
    ) -> UINT;
    /// Give the menu back.
    pub fn DestroyMenu(hMenu: HMENU) -> BOOL;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    pub fn GetTextExtentPoint32W(hdc: HDC, lpString: *const u16, c: i32, pSize: *mut POINT)
    -> BOOL;

    pub fn SetBkMode(hdc: HDC, mode: i32) -> i32;

    pub fn SetTextColor(hdc: HDC, color: u32) -> u32;

    pub fn CreateSolidBrush(color: u32) -> HBRUSH;

    pub fn CreateFontW(
        nHeight: i32,
        nWidth: i32,
        nEscapement: i32,
        nOrientation: i32,
        fnWeight: i32,
        fdwItalic: DWORD,
        fdwUnderline: DWORD,
        fdwStrikeOut: DWORD,
        fdwCharSet: DWORD,
        fdwOutputPrecision: DWORD,
        fdwClipPrecision: DWORD,
        fdwQuality: DWORD,
        fdwPitchAndFamily: DWORD,
        lpszFace: *const u16,
    ) -> HFONT;

    pub fn SelectObject(hdc: HDC, h: HANDLE) -> HANDLE;

    pub fn DeleteObject(ho: HANDLE) -> BOOL;

    /// The object of a kind that the context has selected.
    pub fn GetCurrentObject(hdc: HDC, iType: UINT) -> HANDLE;

    pub fn CreateRoundRectRgn(
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        widthEllipse: i32,
        heightEllipse: i32,
    ) -> HRGN;

    /// A context that draws into a bitmap of the window's own.
    pub fn CreateCompatibleDC(hdc: HDC) -> HDC;
    /// Give the context back.
    pub fn DeleteDC(hdc: HDC) -> BOOL;
    /// A bitmap the context can draw into, with the pixels of the panel.
    pub fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BitmapInfo,
        usage: u32,
        ppvBits: *mut *mut c_void,
        hSection: HANDLE,
        offset: u32,
    ) -> HANDLE;
    /// The face of the font the context has selected.
    pub fn GetTextFaceW(hdc: HDC, lpszFace: *mut u16, cFace: i32) -> i32;

    /// A bitmap the caller fills in.
    ///
    /// The pixels are the caller's own bytes: a colour bitmap is thirty-two
    /// bits a pixel and a mask is one, and the platform reads them in the
    /// order the icon's own format says.
    pub fn CreateBitmap(
        nWidth: i32,
        nHeight: i32,
        nPlanes: u32,
        nBitsPerPixel: u32,
        lpBits: *const c_void,
    ) -> HBITMAP;
}

unsafe extern "system" {
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HINSTANCE;
    pub fn GetProcAddress(hModule: HINSTANCE, lpProcName: *const u8) -> *mut c_void;
    pub fn GetTickCount64() -> u64;

    /// Wait without taking the CPU.
    ///
    /// The launcher waits for another launcher's window to exist, and a
    /// wait that spins is a wait that takes the CPU from the process it
    /// is waiting for.
    pub fn Sleep(dwMilliseconds: u32);
    /// The last error, and the way to clear it.
    ///
    /// A call that fails says so in the last error, and a call that
    /// succeeds leaves it as it was — so the way to tell a call that
    /// failed from a call that succeeded with a null answer is to clear
    /// the error first and to look at it after.
    pub fn GetLastError() -> DWORD;
    pub fn SetLastError(code: DWORD) -> DWORD;

    /// The size of a block of the global memory.
    pub fn GlobalSize(hMem: HANDLE) -> usize;
    /// A block of the global memory, moveable so that it can be given
    /// away.
    pub fn GlobalAlloc(uFlags: u32, dwBytes: usize) -> HANDLE;
    /// The block's bytes, locked for reading or writing.
    pub fn GlobalLock(hMem: HANDLE) -> *mut c_void;
    /// Give the block back, unlocked.
    pub fn GlobalUnlock(hMem: HANDLE) -> i32;
    /// Give the block back to the memory.
    pub fn GlobalFree(hMem: HANDLE) -> HANDLE;
}

/// The last error of a call that succeeded.
pub const ERROR_SUCCESS: DWORD = 0;

#[link(name = "dwmapi")]
unsafe extern "system" {
    /// Ask the desktop window manager what a window is.
    ///
    /// The answer is the attribute's own: an `HRESULT` of zero and the
    /// value written where the caller asked for it.
    pub fn DwmGetWindowAttribute(
        hwnd: HWND,
        dwAttribute: u32,
        pvAttribute: *mut c_void,
        cbAttribute: u32,
    ) -> i32;
    pub fn DwmSetWindowAttribute(
        hwnd: HWND,
        dwAttribute: u32,
        pvAttribute: *const c_void,
        cbAttribute: u32,
    ) -> i32;
    /// Extend the window's frame into its client area.
    ///
    /// The margins of `-1` on every side are the whole trick: the frame
    /// covers the client area, and the desktop window manager composes
    /// the backdrop into it. Without this the backdrop has no surface to
    /// be drawn on, the window's own background is what shows, and a
    /// window that painted nothing over it is a window that shows the
    /// screen it was opened on.
    pub fn DwmExtendFrameIntoClientArea(hwnd: HWND, pMargrins: *const MARGINS) -> i32;
}

/// The margins the frame is extended by, in pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MARGINS {
    pub cx_left_width: i32,
    pub cy_top_height: i32,
    pub cx_right_width: i32,
    pub cy_bottom_height: i32,
}

// The shell's library is not one the toolchain links by default: a
// launch that cannot open a file is not a launch.
#[link(name = "shell32")]
unsafe extern "system" {
    /// Open a path with the platform's default program.
    ///
    /// The answer is a value above thirty-two on success: anything at
    /// or below it is an error code.
    pub fn ShellExecuteW(
        hwnd: HWND,
        lpOperation: *const u16,
        lpFile: *const u16,
        lpParameters: *const u16,
        lpDirectory: *const u16,
        nShowCmd: i32,
    ) -> isize;
    /// The shell's icon for a path.
    ///
    /// The answer is a copy the caller owns: with `SHGFI_ICON` the
    /// handle has to be given back with `DestroyIcon`.
    pub fn SHGetFileInfoW(
        pszPath: *const u16,
        dwFileAttributes: DWORD,
        psfi: *mut SHFILEINFOW,
        cbFileInfo: UINT,
        uFlags: UINT,
    ) -> u32;

    /// Put an icon in the notification area, or take it away.
    ///
    /// The answer says whether the shell did it: an icon that is not there
    /// is a launcher with no face, and the caller has to be able to say so.
    pub fn Shell_NotifyIconW(dwMessage: DWORD, pnid: *const NOTIFYICONDATAW) -> BOOL;
}

// The console's calls are the kernel's own, and the seam is where the
// platform's calls are written by hand.
#[link(name = "kernel32")]
unsafe extern "system" {
    /// A kernel object that exists once, by name, for the session.
    ///
    /// The answer is the object's handle — and the last error says
    /// whether the object was already there, which is the whole question
    /// a launcher asks: another launcher's mutex means another launcher.
    pub fn CreateMutexW(
        lpMutexAttributes: *const c_void,
        bInitialOwner: BOOL,
        lpName: *const u16,
    ) -> HANDLE;

    /// The thread the call is made on.
    ///
    /// It is what `AttachThreadInput` attaches, and the answer is this
    /// thread's own id: a thread that attached to another input queue is a
    /// thread that must detach from it with the same number.
    pub fn GetCurrentThreadId() -> DWORD;

    /// The window of the console the process is attached to.
    ///
    /// The answer is null when there is no console — a launch that was
    /// given pipes has none — and a window that belongs to the console
    /// host, not to this process: the handle is for asking about the
    /// console, and a console the process is attached to is not the same
    /// thing as a console the process was given.
    pub fn GetConsoleWindow() -> HWND;

    /// The processes attached to this console, written into the buffer.
    ///
    /// The answer is how many processes are attached. A buffer too small
    /// to hold them all answers the number the list needs, which is still
    /// more than one — the question the caller asks is whether the
    /// process is alone on the console, and a list that had to be longer
    /// still answers it. Zero is the call's own refusal: there is no
    /// console to ask about.
    pub fn GetConsoleProcessList(lpdwProcessList: *mut DWORD, dwProcessCount: DWORD) -> DWORD;

    /// Open the null device, the writer nobody has to read.
    pub fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;

    /// Point one of the process's standard streams at another handle.
    ///
    /// The answer says whether it did. The streams have to be pointed
    /// somewhere before the console they came from is given back: the
    /// library reads each standard handle once and keeps it, and a write
    /// to a handle whose console is gone is a panic.
    pub fn SetStdHandle(nStdHandle: DWORD, hHandle: HANDLE) -> BOOL;

    /// Give the console back.
    ///
    /// The answer says whether the process left it. The console's window
    /// — or its tab, when the host is a terminal with more of them —
    /// closes with the last process on it.
    pub fn FreeConsole() -> BOOL;

    /// Give a handle back.
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
}

/// The processes attached to the console.
///
/// The buffer is a **slice**: the platform takes a pointer and a length,
/// and the slice carries both — a pointer that can be null is a pointer
/// that can be wrong, and a length that does not belong to the pointer is
/// a length that does not. The answer is what the platform says about
/// the buffer: how many processes are attached, or how many the list
/// needs when the buffer was too small.
pub fn console_process_list(attached: &mut [DWORD]) -> DWORD {
    unsafe { GetConsoleProcessList(attached.as_mut_ptr(), attached.len() as DWORD) }
}

/// The null device is opened for writing, by every one.
pub const GENERIC_WRITE: DWORD = 0x8000_0000;
/// The null device is opened for sharing, with every one.
pub const FILE_SHARE_ALL: DWORD = 0x3;
/// The null device is opened as the thing it already is.
pub const OPEN_EXISTING: DWORD = 3;
/// The standard output, as the platform numbers it.
pub const STD_OUTPUT_HANDLE: DWORD = 0xFFFF_FFF5;
/// The standard error, as the platform numbers it.
pub const STD_ERROR_HANDLE: DWORD = 0xFFFF_FFF4;
/// The handle no call answers with on success.
pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform's message numbers, pinned.
    ///
    /// A message number is a number the platform chose, and a number the
    /// platform chose is a fact: the window compares what arrives against
    /// what it declared, and a digit lost in the declaration is a message
    /// the window never hears.
    ///
    /// The timer is the one that matters most. It is the clock that asks
    /// the daemon, and a window whose clock never ticks is a window that
    /// never answers: the field takes the query, the list stays empty,
    /// and nothing says why. The number was `0x0013` — a digit short of
    /// the platform's `0x0113` — and the window was silent for as long as
    /// it was there.
    #[test]
    fn the_messages_are_the_platform_s_own() {
        assert_eq!(WM_TIMER, 0x0113, "the clock is the platform's");
        assert_eq!(WM_SIZE, 0x0005);
        assert_eq!(WM_PAINT, 0x000F);
        assert_eq!(WM_CLOSE, 0x0010);
        assert_eq!(WM_ERASEBKGND, 0x0014);
        assert_eq!(WM_DESTROY, 0x0002);
        assert_eq!(WM_KEYDOWN, 0x0100);
        assert_eq!(WM_KEYUP, 0x0101);
        assert_eq!(WM_CHAR, 0x0102);
        assert_eq!(WM_SYSKEYDOWN, 0x0104);
        assert_eq!(WM_SYSKEYUP, 0x0105);
        assert_eq!(WM_ACTIVATEAPP, 0x001C);
        assert_eq!(WM_APP, 0x8000);
    }

    /// The platform's keys, pinned.
    ///
    /// A key that is not the platform's is a key the window never hears,
    /// and the arrow keys are the ones the user reaches for without
    /// looking.
    #[test]
    fn the_keys_are_the_platform_s_own() {
        assert_eq!(VK_BACK, 0x08);
        assert_eq!(VK_TAB, 0x09);
        assert_eq!(VK_RETURN, 0x0D);
        assert_eq!(VK_CONTROL, 0x11);
        assert_eq!(VK_PRIOR, 0x21);
        assert_eq!(VK_NEXT, 0x22);
        assert_eq!(VK_END, 0x23);
        assert_eq!(VK_HOME, 0x24);
        assert_eq!(VK_LEFT, 0x25);
        assert_eq!(VK_UP, 0x26);
        assert_eq!(VK_RIGHT, 0x27);
        assert_eq!(VK_DOWN, 0x28);
        assert_eq!(VK_ESCAPE, 0x1B);
        assert_eq!(VK_DELETE, 0x2E);
        assert_eq!(VK_SHIFT, 0x10);
        assert_eq!(VK_MENU, 0x12);
        assert_eq!(VK_LSHIFT, 0xA0);
        assert_eq!(VK_RSHIFT, 0xA1);
        assert_eq!(VK_LCONTROL, 0xA2);
        assert_eq!(VK_RCONTROL, 0xA3);
        assert_eq!(VK_LMENU, 0xA4);
        assert_eq!(VK_RMENU, 0xA5);
        // The combo's own message, posted and not sent: the platform
        // puts it in the queue, and the pump is where the answer is
        // given.
        assert_eq!(WM_HOTKEY, 0x0312);
    }

    /// The platform's window styles, pinned.
    ///
    /// The window is a tool window on top of everything else: the styles
    /// are what make it a tool window, and a style that is not the
    /// platform's is a window that behaves like another window.
    #[test]
    fn the_styles_are_the_platform_s_own() {
        assert_eq!(WS_POPUP, 0x8000_0000);
        assert_eq!(WS_EX_TOOLWINDOW, 0x0000_0080);
        assert_eq!(WS_EX_TOPMOST, 0x0000_0008);
        assert_eq!(CS_VREDRAW, 0x0001);
        assert_eq!(CS_HREDRAW, 0x0002);
        assert_eq!(CS_DROPSHADOW, 0x0002_0000);
        assert_eq!(SW_HIDE, 0);
        assert_eq!(SW_SHOWNOACTIVATE, 4);
        assert_eq!(SWP_NOMOVE, 0x0002);
        assert_eq!(SWP_NOACTIVATE, 0x0010);
        assert_eq!(GWL_USERDATA, -21);
    }

    /// The platform's console numbers, pinned.
    ///
    /// The standard streams are numbered by the platform, and the null
    /// device is opened with the platform's own constants: a number that
    /// is only ever compared against itself cannot catch a typo.
    #[test]
    fn the_console_numbers_are_the_platform_s_own() {
        assert_eq!(STD_OUTPUT_HANDLE as i32, -11);
        assert_eq!(STD_ERROR_HANDLE as i32, -12);
        assert_eq!(OPEN_EXISTING, 3);
        assert_eq!(GENERIC_WRITE, 0x8000_0000);
        assert_eq!(FILE_SHARE_ALL, 0x1 | 0x2);
        assert!(INVALID_HANDLE_VALUE as isize == -1);
    }
}
