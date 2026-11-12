// SPDX-License-Identifier: Apache-2.0

//! The Win32 calls this backend makes, declared by hand.
//!
//! The tree does not link the `windows` crate: it does its platform calls
//! by hand, in the seam, once — and this is the seam for the Windows
//! discovery backend. Every type here is the platform's own, whole: a
//! struct the platform writes is the platform's struct, and a field left
//! out is a buffer the platform writes past.
//!
//! Only the calls this crate uses are declared. A declaration that is not
//! used is a declaration that can be wrong.
//!
//! **The error code is read where the call is.** `GetLastError` says what
//! the last call failed with, and any other call in between would change
//! the answer. So the functions here return the code themselves, and a
//! caller never has to remember to ask for it before it is too late.
//!
//! **A buffer is a slice, not a pointer.** The platform takes a pointer
//! and a length, and the two travel together; a caller that hands over a
//! length and a pointer separately can hand over one without the other.
//! The seam takes the slice, and the length is the slice's own. The
//! handle and the wide path stay the platform's own types, because they
//! are values the platform defines and this crate only carries them.

#![allow(non_snake_case, non_camel_case_types)]
// The names here are the **platform's** names: `BOOL`, `DWORD`, `HANDLE`,
// `OVERLAPPED`. Renaming them to satisfy a lint would make the seam
// harder to read against the documentation it implements, and a reader
// who knows Win32 would have to translate every name in their head. The
// lint is right about Rust and wrong about this file.
#![allow(clippy::upper_case_acronyms)]
// A handle and a wide path are pointers, and this file never writes
// through either: it hands them to the platform, which is the only thing
// that may read them. The lint is about a function that dereferences a
// pointer it was given, and a function that passes one on is a different
// thing. The buffers are the ones that get written, and those are slices
// here -- a slice carries its own length and cannot be null.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use core::ffi::c_void;

/// The platform's own types.
pub type BOOL = i32;
pub type DWORD = u32;
pub type HANDLE = *mut c_void;
pub type PCWSTR = *const u16;

/// `GENERIC_READ`. The NTFS driver rejects volume reads opened with
/// anything weaker.
pub const GENERIC_READ: DWORD = 0x8000_0000;

pub const FILE_SHARE_READ: DWORD = 0x0000_0001;
pub const FILE_SHARE_WRITE: DWORD = 0x0000_0002;
pub const FILE_SHARE_DELETE: DWORD = 0x0000_0004;

/// `OPEN_EXISTING`.
pub const OPEN_EXISTING: DWORD = 3;

/// A directory handle needs the backup semantics: without them the call
/// opens the path as a file and fails.
pub const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;

/// `FILE_LIST_DIRECTORY`: the right that reads a directory's names, and
/// the one an ordinary process has.
pub const FILE_LIST_DIRECTORY: DWORD = 0x0000_0001;

pub const FILE_NOTIFY_CHANGE_FILE_NAME: DWORD = 0x0000_0001;
pub const FILE_NOTIFY_CHANGE_DIR_NAME: DWORD = 0x0000_0002;
pub const FILE_NOTIFY_CHANGE_SIZE: DWORD = 0x0000_0008;
pub const FILE_NOTIFY_CHANGE_LAST_WRITE: DWORD = 0x0000_0010;
pub const FILE_NOTIFY_CHANGE_CREATION: DWORD = 0x0000_0040;

/// `FILE_ACTION_ADDED`: the platform's record of a name that appeared.
pub const FILE_ACTION_ADDED: DWORD = 1;

/// The control codes of the NTFS driver.
///
/// A control code is a number the platform gives to one operation, and
/// the numbers are not the ones a reader would guess: each is the
/// device, the function and the way the buffers are exchanged, packed
/// into one word. They are written down here exactly as the platform
/// spells them.
pub const FSCTL_GET_NTFS_FILE_RECORD: DWORD = 589_928;
pub const FSCTL_GET_NTFS_VOLUME_DATA: DWORD = 589_924;
pub const FSCTL_QUERY_USN_JOURNAL: DWORD = 590_068;
pub const FSCTL_READ_USN_JOURNAL: DWORD = 590_011;

/// `FILE_INFO_BY_HANDLE_CLASS::FileIdInfo` — the class that answers with
/// the file's own identifier.
pub const FILE_ID_INFO_CLASS: i32 = 18;

/// `OVERLAPPED` — the platform's struct, whole.
///
/// The offset is a union with a pointer, and the event is a handle: the
/// platform writes into this struct, so a field left out is a field the
/// platform writes past.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OVERLAPPED {
    pub Internal: usize,
    pub InternalHigh: usize,
    pub Anonymous: OVERLAPPED_UNION,
    pub hEvent: HANDLE,
}

/// The offset of an overlapped read: two words, low and high.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OVERLAPPED_OFFSET {
    pub Offset: DWORD,
    pub OffsetHigh: DWORD,
}

/// The union of the offset and the pointer.
#[repr(C)]
#[derive(Clone, Copy)]
pub union OVERLAPPED_UNION {
    pub Anonymous: OVERLAPPED_OFFSET,
    pub Pointer: *mut c_void,
}

/// The platform's own layout, checked at compile time: a layout that
/// drifts fails the build and not the read.
///
/// The struct is four pointer-sized words on a 64-bit target, and five
/// 32-bit words on a 32-bit one — the offset union is eight bytes wide
/// either way, because its two words sit next to each other.
const _: () = {
    if core::mem::size_of::<usize>() == 8 {
        assert!(core::mem::size_of::<OVERLAPPED>() == 32);
    } else {
        assert!(core::mem::size_of::<OVERLAPPED>() == 20);
    }
};

/// `FILE_NOTIFY_INFORMATION` — the platform's struct, whole.
///
/// The name is a one-character array and the platform writes more of it,
/// so the structure is read out of a buffer that has the room for it: the
/// header is three words and the name follows it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FILE_NOTIFY_INFORMATION {
    pub NextEntryOffset: DWORD,
    pub Action: DWORD,
    pub FileNameLength: DWORD,
    pub FileName: [u16; 1],
}

/// The platform's own layout, checked at compile time: the header is
/// three words, and the name follows it.
const _: () = assert!(core::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName) == 12);

/// `FILE_ID_INFO` — the file's own identifier, and the volume it is on.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FILE_ID_INFO {
    pub VolumeSerialNumber: u64,
    pub FileId: FILE_ID_128,
}

/// The 128-bit identifier the platform hands back.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FILE_ID_128 {
    pub Identifier: [u8; 16],
}

/// The platform's own layout, checked at compile time.
const _: () = assert!(core::mem::size_of::<FILE_ID_INFO>() == 24);

unsafe extern "system" {
    fn CreateFileW(
        lpFileName: PCWSTR,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    fn CloseHandle(hObject: HANDLE) -> BOOL;
    fn GetLastError() -> DWORD;
    fn ReadFile(
        hFile: HANDLE,
        lpBuffer: *mut u8,
        nNumberOfBytesToRead: DWORD,
        lpNumberOfBytesRead: *mut DWORD,
        lpOverlapped: *mut OVERLAPPED,
    ) -> BOOL;
    fn DeviceIoControl(
        hDevice: HANDLE,
        dwIoControlCode: DWORD,
        lpInBuffer: *const c_void,
        nInBufferSize: DWORD,
        lpOutBuffer: *mut c_void,
        nOutBufferSize: DWORD,
        lpBytesReturned: *mut DWORD,
        lpOverlapped: *mut OVERLAPPED,
    ) -> BOOL;
    fn GetDriveTypeW(lpRootPathName: PCWSTR) -> DWORD;
    fn GetVolumeInformationW(
        lpRootPathName: PCWSTR,
        lpVolumeNameBuffer: *mut u16,
        nVolumeNameSize: DWORD,
        lpVolumeSerialNumber: *mut DWORD,
        lpMaximumComponentLength: *mut DWORD,
        lpFileSystemFlags: *mut DWORD,
        lpFileSystemNameBuffer: *mut u16,
        nFileSystemNameSize: DWORD,
    ) -> BOOL;
    fn ReadDirectoryChangesW(
        hDirectory: HANDLE,
        lpBuffer: *mut c_void,
        nBufferLength: DWORD,
        bWatchSubtree: BOOL,
        dwNotifyFilter: DWORD,
        lpBytesReturned: *mut DWORD,
        lpOverlapped: *mut OVERLAPPED,
        lpCompletionRoutine: *mut c_void,
    ) -> BOOL;
    fn GetFileInformationByHandleEx(
        hFile: HANDLE,
        FileInformationClass: i32,
        lpFileInformation: *mut c_void,
        dwBufferSize: DWORD,
    ) -> BOOL;
}

/// The error code of the call that just failed.
///
/// This is only ever right immediately after the call: any other call in
/// between changes the answer.
pub fn last_error() -> DWORD {
    unsafe { GetLastError() }
}

/// Is the handle one the platform says is not a handle?
///
/// `CreateFileW` fails with `INVALID_HANDLE_VALUE`, and some calls fail
/// with a null pointer. A handle that is neither is a handle.
pub fn is_invalid(handle: HANDLE) -> bool {
    handle.is_null() || handle as usize == usize::MAX
}

/// Open a file, a directory or a volume.
///
/// The name is the platform's own: a NUL-terminated wide string the
/// caller owns and keeps alive across the call.
///
/// The security attributes and the template handle are not in the
/// signature because the crate never uses them: it inherits no handle
/// and creates nothing out of a template, so the call always passes
/// `NULL` for both.
///
/// The error code is the one the platform left behind, read here so that
/// it is the code of *this* call.
pub fn create_file_w(
    name: PCWSTR,
    desired_access: DWORD,
    share_mode: DWORD,
    creation_disposition: DWORD,
    flags_and_attributes: DWORD,
) -> Result<HANDLE, DWORD> {
    let handle = unsafe {
        CreateFileW(
            name,
            desired_access,
            share_mode,
            core::ptr::null(),
            creation_disposition,
            flags_and_attributes,
            core::ptr::null_mut(),
        )
    };
    if is_invalid(handle) {
        return Err(last_error());
    }
    Ok(handle)
}

/// Close a handle.
pub fn close_handle(handle: HANDLE) -> Result<(), DWORD> {
    if unsafe { CloseHandle(handle) } == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Read from a handle, at an offset when the overlapped says where.
///
/// The overlapped is what positions the read: Windows honours the offset
/// on a synchronous handle too.
pub fn read_file(
    file: HANDLE,
    buffer: Option<&mut [u8]>,
    bytes_read: Option<&mut DWORD>,
    overlapped: Option<&mut OVERLAPPED>,
) -> Result<(), DWORD> {
    let (pointer, length) = match buffer {
        Some(bytes) => (bytes.as_mut_ptr(), bytes.len() as DWORD),
        None => (core::ptr::null_mut(), 0),
    };
    let ok = unsafe {
        ReadFile(
            file,
            pointer,
            length,
            bytes_read.map_or(core::ptr::null_mut(), |value| value as *mut DWORD),
            overlapped.map_or(core::ptr::null_mut(), |value| value as *mut OVERLAPPED),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Ask the driver to do something to a device.
///
/// The control code is one of the `FSCTL_` values above, and the buffers
/// are the ones the control names: the driver writes into the output
/// buffer and says how much of it it wrote.
pub fn device_io_control(
    device: HANDLE,
    control_code: DWORD,
    in_buffer: Option<&[u8]>,
    out_buffer: Option<&mut [u8]>,
    bytes_returned: Option<&mut DWORD>,
    overlapped: Option<&mut OVERLAPPED>,
) -> Result<(), DWORD> {
    let (in_pointer, in_length) = match in_buffer {
        Some(bytes) => (bytes.as_ptr().cast::<c_void>(), bytes.len() as DWORD),
        None => (core::ptr::null(), 0),
    };
    let (out_pointer, out_length) = match out_buffer {
        Some(bytes) => (bytes.as_mut_ptr().cast::<c_void>(), bytes.len() as DWORD),
        None => (core::ptr::null_mut(), 0),
    };
    let ok = unsafe {
        DeviceIoControl(
            device,
            control_code,
            in_pointer,
            in_length,
            out_pointer,
            out_length,
            bytes_returned.map_or(core::ptr::null_mut(), |value| value as *mut DWORD),
            overlapped.map_or(core::ptr::null_mut(), |value| value as *mut OVERLAPPED),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// The kind of drive a root path is.
pub fn get_drive_type_w(root: PCWSTR) -> DWORD {
    unsafe { GetDriveTypeW(root) }
}

/// What the platform knows about a volume.
///
/// Every buffer is optional because the platform asks for each of them
/// separately: the volume's name, its serial, the largest component it
/// allows, the flags its filesystem carries and the filesystem's own
/// name are five questions, and a caller asks the ones it needs.
pub fn get_volume_information_w(
    root: PCWSTR,
    volume_name: Option<&mut [u16]>,
    serial: Option<&mut DWORD>,
    max_component_length: Option<&mut DWORD>,
    file_system_flags: Option<&mut DWORD>,
    file_system_name: Option<&mut [u16]>,
) -> Result<(), DWORD> {
    let (volume_name_pointer, volume_name_length) = match volume_name {
        Some(buffer) => (buffer.as_mut_ptr(), buffer.len() as DWORD),
        None => (core::ptr::null_mut(), 0),
    };
    let (file_system_name_pointer, file_system_name_length) = match file_system_name {
        Some(buffer) => (buffer.as_mut_ptr(), buffer.len() as DWORD),
        None => (core::ptr::null_mut(), 0),
    };
    let ok = unsafe {
        GetVolumeInformationW(
            root,
            volume_name_pointer,
            volume_name_length,
            serial.map_or(core::ptr::null_mut(), |value| value as *mut DWORD),
            max_component_length.map_or(core::ptr::null_mut(), |value| value as *mut DWORD),
            file_system_flags.map_or(core::ptr::null_mut(), |value| value as *mut DWORD),
            file_system_name_pointer,
            file_system_name_length,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Watch a directory for changes.
///
/// The buffer is filled with the platform's own records, and the number
/// of bytes it wrote says how much of it holds them.
pub fn read_directory_changes_w(
    directory: HANDLE,
    buffer: &mut [u8],
    watch_subtree: bool,
    notify_filter: DWORD,
    bytes_returned: Option<&mut DWORD>,
    overlapped: Option<&mut OVERLAPPED>,
) -> Result<(), DWORD> {
    let ok = unsafe {
        ReadDirectoryChangesW(
            directory,
            buffer.as_mut_ptr().cast::<c_void>(),
            buffer.len() as DWORD,
            if watch_subtree { 1 } else { 0 },
            notify_filter,
            bytes_returned.map_or(core::ptr::null_mut(), |value| value as *mut DWORD),
            overlapped.map_or(core::ptr::null_mut(), |value| value as *mut OVERLAPPED),
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// The file's own identifier, and the volume it is on.
pub fn get_file_information_by_handle_ex(
    file: HANDLE,
    information: &mut FILE_ID_INFO,
) -> Result<(), DWORD> {
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file,
            FILE_ID_INFO_CLASS,
            information as *mut FILE_ID_INFO as *mut c_void,
            core::mem::size_of::<FILE_ID_INFO>() as DWORD,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}
