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
pub const CS_DROPSHADOW: u32 = 0x0002_0000;

pub const SW_HIDE: i32 = 0;
pub const SW_SHOWNOACTIVATE: i32 = 4;

pub const WM_DESTROY: UINT = 0x0002;
pub const WM_SIZE: UINT = 0x0005;
pub const WM_PAINT: UINT = 0x000F;
pub const WM_CLOSE: UINT = 0x0010;
pub const WM_ERASEBKGND: UINT = 0x0014;
pub const WM_TIMER: UINT = 0x0013;
pub const WM_KEYDOWN: UINT = 0x0100;
pub const WM_KEYUP: UINT = 0x0101;
pub const WM_CHAR: UINT = 0x0102;
pub const WM_SYSKEYDOWN: UINT = 0x0104;
pub const WM_SYSKEYUP: UINT = 0x0105;
/// The application stopped being the active one.
pub const WM_ACTIVATEAPP: UINT = 0x001C;
pub const WM_APP: UINT = 0x8000;

pub const VK_BACK: u32 = 0x08;
pub const VK_TAB: u32 = 0x09;
pub const VK_RETURN: u32 = 0x0D;
pub const VK_CONTROL: u32 = 0x11;
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
pub const DI_NORMAL: u32 = 0x0000;

/// The window's corner radius, in pixels, as DWM wants it.
pub const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;

/// The backdrop the desktop window manager gives the window, asked for as
/// a number: this is the question that has an answer.
pub const DWMWA_SYSTEM_BACKDROP_TYPE: u32 = 38;

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

    pub fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;

    pub fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;

    pub fn GetSystemMetrics(nIndex: i32) -> i32;

    /// Make the process aware of the monitor's own scale.
    ///
    /// The awareness is asked once, before the first window exists: an
    /// awareness set after the window is on the screen is not an
    /// awareness the window can use.
    pub fn SetProcessDpiAwarenessContext(value: DPI_AWARENESS_CONTEXT) -> BOOL;

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

    pub fn DrawIconEx(
        hdc: HDC,
        hIcon: HICON,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        istep: UINT,
        hbr: HBRUSH,
        flags: UINT,
    ) -> BOOL;

    pub fn DestroyIcon(hIcon: HICON) -> BOOL;

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
}

unsafe extern "system" {
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HINSTANCE;
    pub fn GetProcAddress(hModule: HINSTANCE, lpProcName: *const u8) -> *mut c_void;
    pub fn GetTickCount64() -> u64;
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
    ) -> usize;
}
