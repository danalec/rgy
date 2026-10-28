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
pub type HRGN = *mut c_void;
pub type HHOOK = *mut c_void;
pub type HMENU = *mut c_void;

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
pub const WM_CHAR: UINT = 0x0102;
pub const WM_SYSKEYDOWN: UINT = 0x0104;
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

pub const SWP_NOSIZE: UINT = 0x0001;
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

pub const WH_KEYBOARD_LL: i32 = 13;
pub const HC_ACTION: i32 = 0;

/// The window's corner radius, in pixels, as DWM wants it.
pub const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
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
    pub fn GetMessageW(lpMsg: *mut MSG, hWnd: HWND, wMsgFilterMin: UINT, wMsgFilterMax: UINT)
        -> BOOL;
    pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    pub fn PostQuitMessage(nExitCode: i32);
    pub fn PostMessageW(hWnd: HWND, Msg: UINT, wParam: WPARAM, lParam: LPARAM) -> BOOL;
    pub fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;
    pub fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;
    pub fn GetSystemMetrics(nIndex: i32) -> i32;
    pub fn SetTimer(
        hWnd: HWND,
        nIDEvent: usize,
        uElapse: u32,
        lpTimerFunc: *mut c_void,
    ) -> usize;
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
    pub fn GetTextExtentPoint32W(
        hdc: HDC,
        lpString: *const u16,
        c: i32,
        pSize: *mut POINT,
    ) -> BOOL;
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

    pub fn SetWindowRgn(hWnd: HWND, hRgn: HRGN, bRedraw: BOOL) -> i32;
    pub fn CreateRoundRectRgn(
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        widthEllipse: i32,
        heightEllipse: i32,
    ) -> HRGN;

    pub fn SetWindowsHookExW(
        idHook: i32,
        lpfn: Option<
            unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT,
        >,
        hMod: HINSTANCE,
        dwThreadId: DWORD,
    ) -> HHOOK;
    pub fn CallNextHookEx(hhk: HHOOK, nCode: i32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn UnhookWindowsHookEx(hhk: HHOOK) -> BOOL;
}

unsafe extern "system" {
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HINSTANCE;
    pub fn GetProcAddress(hModule: HINSTANCE, lpProcName: *const u8) -> *mut c_void;
    pub fn GetTickCount64() -> u64;
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    pub fn DwmSetWindowAttribute(
        hwnd: HWND,
        dwAttribute: u32,
        pvAttribute: *const c_void,
        cbAttribute: u32,
    ) -> i32;
}
