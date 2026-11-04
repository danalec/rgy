// SPDX-License-Identifier: Apache-2.0

//! Win32 volume access: raw NTFS file records and USN journal reads.

use crate::ntfs::{self, FileRecordInput, ReadUsnJournalData};
use crate::win::{
    self, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FSCTL_GET_NTFS_FILE_RECORD,
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, HANDLE,
    OPEN_EXISTING, OVERLAPPED,
};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// The tests, and only the tests, need `std`: a temp file to read at an
/// offset, and the process id to name it after. The crate body is `core`
/// plus `alloc`.
#[cfg(test)]
extern crate std;

/// `GENERIC_READ`. The NTFS driver rejects volume reads opened with anything
/// weaker; Zen uses the same value (0x80000000) for its volume handles.
const GENERIC_READ_ACCESS: u32 = win::GENERIC_READ;

/// `DRIVE_FIXED` — `GetDriveTypeW` returns this as a bare `u32`, so the
/// value is spelled out here.
const DRIVE_FIXED: u32 = 3;

/// USN read buffer size, matching what Zen uses (64 KiB).
pub const USN_BUFFER_SIZE: usize = 0x1_0000;
/// `FSCTL_READ_USN_JOURNAL` output begins with the resume USN (i64) before any
/// `USN_RECORD`. Records start after this prefix.
pub const USN_HEADER_PREFIX: usize = 8;
/// MFT records are a fixed 1 KiB on NTFS.
pub const MFT_RECORD_SIZE: usize = 1024;

pub type VolumeResult<T> = Result<T, VolumeError>;

#[derive(Debug)]
pub enum VolumeError {
    Open { path: String, code: u32 },
    IoControl { op: &'static str, code: u32 },
    JournalUnavailable { code: u32 },
}

impl core::fmt::Display for VolumeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            VolumeError::Open { path, code } => {
                write!(f, "failed to open volume {path} (win32 error {code})")
            }
            VolumeError::IoControl { op, code } => {
                write!(f, "DeviceIoControl {op} failed (win32 error {code})")
            }
            VolumeError::JournalUnavailable { code } => {
                write!(f, "USN journal not available (win32 error {code})")
            }
        }
    }
}

impl core::error::Error for VolumeError {}

/// An open NTFS volume handle.
pub struct Volume {
    handle: HANDLE,
    /// DOS-style root of the volume, e.g. `C:\`. This is what indexed paths are
    /// built on, so it must come from the caller's argument and not the device
    /// path — indexing `D:` should not yield `C:\...` paths.
    pub root: String,
    pub path: String,
    /// Retained for volume identity/debugging; `FileRecordInput` carries no
    /// serial, so nothing in the read path consumes it.
    pub serial: u32,
    /// Bytes in one MFT file record segment, from `FSCTL_GET_NTFS_VOLUME_DATA`.
    pub record_size: usize,
    /// Upper bound on file reference numbers worth reading.
    pub record_count: u64,
    /// Raw `NTFS_VOLUME_DATA_BUFFER` fields the record size is derived from.
    /// Surfaced so a wrong record size can be diagnosed rather than guessed at:
    /// `MftRecordSize` is not populated on every driver.
    pub volume_data: Option<ntfs::NtfsVolumeData>,
    /// Bytes in one cluster, from `FSCTL_GET_NTFS_VOLUME_DATA`.
    ///
    /// This is the only volume geometry the indexer consumes. Bytes-per-sector
    /// is deliberately absent: MFT fixups are fixed at 512 regardless of the
    /// volume (`ntfs::MFT_SECTOR_SIZE`), so carrying the value here only ever
    /// invited a caller to pass it to something that must not be parameterised.
    pub bytes_per_cluster: usize,
}

// The handle is owned exclusively by this struct and closed on drop.
unsafe impl Send for Volume {}

impl Volume {
    /// Open a volume by DOS-style root path, e.g. `C:\`.
    pub fn open(root: &str) -> VolumeResult<Self> {
        let device = device_path(root);

        // Volume devices are opened with flags=0: passing FILE_FLAG_BACKUP_SEMANTICS
        // makes \\.\X: resolve as a directory handle rather than the volume.
        let wide: Vec<u16> = device.encode_utf16().chain(core::iter::once(0)).collect();

        let handle = win::create_file_w(
            wide.as_ptr(),
            GENERIC_READ_ACCESS,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            OPEN_EXISTING,
            0,
        )
        .map_err(|code| VolumeError::Open {
            path: device.clone(),
            code,
        })?;

        let serial = volume_serial(root).unwrap_or(0);

        // Record size is a volume property; a fixed 1024 gets rejected with
        // ERROR_INVALID_PARAMETER on volumes with 4 KiB records.
        let mut volume = Volume {
            handle,
            root: volume_root(root),
            path: device,
            serial,
            record_size: MFT_RECORD_SIZE,
            record_count: 0,
            bytes_per_cluster: 4096,
            volume_data: None,
        };

        let data = volume.query_volume_data()?;
        volume.record_size = data.bytes_per_file_record_segment();
        volume.record_count = data.mft_record_count();
        volume.volume_data = Some(data);
        volume.bytes_per_cluster = data.bytes_per_cluster();

        Ok(volume)
    }

    /// Query `FSCTL_GET_NTFS_VOLUME_DATA` to learn the MFT record size.
    ///
    /// The record size is a volume property, not a constant, so the output
    /// buffer for every subsequent record read is sized from this.
    pub fn query_volume_data(&self) -> VolumeResult<ntfs::NtfsVolumeData> {
        let mut out = vec![0u8; 0x60];
        let mut returned: u32 = 0;

        let ok = win::device_io_control(
            self.handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            Some(out.as_mut_slice()),
            Some(&mut returned),
            None,
        );

        if let Err(code) = ok {
            return Err(VolumeError::IoControl {
                op: "FSCTL_GET_NTFS_VOLUME_DATA",
                code,
            });
        }

        out.truncate(returned.max(0x60) as usize);
        ntfs::NtfsVolumeData::parse(&out).ok_or(VolumeError::IoControl {
            op: "FSCTL_GET_NTFS_VOLUME_DATA",
            code: 0,
        })
    }

    /// Query `FSCTL_QUERY_USN_JOURNAL` for the journal's identity and USN window.
    ///
    /// The journal ID changes when the journal is deleted and recreated. A
    /// reader that keeps resuming from its old USN across that change is reading
    /// a different journal and will silently miss everything in between.
    pub fn query_usn_journal(&self) -> VolumeResult<ntfs::UsnJournalData> {
        let mut out = vec![0u8; 0x60];
        let mut returned: u32 = 0;

        let ok = win::device_io_control(
            self.handle,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            Some(out.as_mut_slice()),
            Some(&mut returned),
            None,
        );

        if let Err(code) = ok {
            return Err(VolumeError::JournalUnavailable { code });
        }

        out.truncate(returned.max(0x60) as usize);
        ntfs::UsnJournalData::parse(&out).ok_or(VolumeError::JournalUnavailable { code: 0 })
    }

    /// Read one MFT record. `frn` is the file reference number (low 48 bits).
    ///
    /// Returns `None` when the record is free or otherwise absent, which is the
    /// normal case once we walk past the end of the allocated MFT. The returned
    /// slice is the bare `FILE_RECORD`, with the driver's 12-byte prefix removed.
    pub fn read_file_record(&self, frn: u64) -> VolumeResult<Option<Vec<u8>>> {
        let record_size = self.record_size;
        let input = FileRecordInput {
            file_reference: ntfs::frn_number(frn),
        };

        let input_bytes = input.to_bytes();
        // Zen sizes this as BytesPerFileRecordSegment + 15, which must
        // also cover the 12-byte prefix; keep a margin so the driver never
        // answers ERROR_MORE_DATA.
        let mut out = vec![0u8; ntfs::FILE_RECORD_OUTPUT_PREFIX + record_size + 64];
        let mut returned: u32 = 0;

        let ok = win::device_io_control(
            self.handle,
            FSCTL_GET_NTFS_FILE_RECORD,
            Some(&input_bytes),
            Some(out.as_mut_slice()),
            Some(&mut returned),
            None,
        );

        if let Err(code) = ok {
            // ERROR_BAD_FRN / ERROR_FILE_CORRUPT mean "this record is not a live
            // file", which we report as absent rather than as a hard failure.
            if code == 0xC000_008C || code == 1006 || code == 0xC000_0035 {
                return Ok(None);
            }
            return Err(VolumeError::IoControl {
                op: "FSCTL_GET_NTFS_FILE_RECORD",
                code,
            });
        }

        if returned <= ntfs::FILE_RECORD_OUTPUT_PREFIX as u32 {
            return Ok(None);
        }

        // The driver echoes the file reference of the record it actually
        // returned in the first 8 bytes of the output. When the requested slot
        // is free it hands back some other record's bytes instead of failing —
        // zen's walker treats exactly this case as "FRN not in use".
        // Without the check a free slot is silently replaced by a neighbour,
        // every slot looks live, and a scan invents paths that do not exist.
        let echoed = ntfs::frn_number(u64::from_le_bytes(
            out[..8].try_into().expect("8-byte prefix"),
        ));
        if echoed != ntfs::frn_number(frn) {
            return Ok(None);
        }

        // Strip the file-reference / context prefix the driver prepends.
        let record = &out[ntfs::FILE_RECORD_OUTPUT_PREFIX..returned as usize];

        // Attribute values straddling a sector boundary are scrambled until the
        // update sequence is reapplied.
        let mut fixed = record.to_vec();
        // Fixups are 512-byte for MFT records regardless of volume sector size.
        // Tolerant here on purpose: the single-record FSCTL path keeps the
        // historically forgiving behaviour and just restores what matches; the
        // bulk walk (scan_via_runs) is the one that abandons on a bad USN.
        let _ = ntfs::apply_fixups(&mut fixed);

        match ntfs::parse_file_record(&fixed) {
            Some(hdr) if hdr.flags & ntfs::MFT_RECORD_IN_USE != 0 => Ok(Some(fixed)),
            _ => Ok(None),
        }
    }

    /// Read USN journal records from `start_usn`.
    ///
    /// `Ok(vec![])` means the journal has been fully consumed; the caller should
    /// then re-read with the updated USN from the returned checkpoint.
    pub fn read_usn(&self, read: &ReadUsnJournalData) -> VolumeResult<(Vec<ntfs::UsnRecord>, i64)> {
        let in_buf = read.to_bytes();
        let mut out = vec![0u8; USN_BUFFER_SIZE];
        let mut returned: u32 = 0;

        let result = win::device_io_control(
            self.handle,
            FSCTL_READ_USN_JOURNAL,
            Some(&in_buf),
            Some(out.as_mut_slice()),
            Some(&mut returned),
            None,
        );

        if let Err(code) = result {
            // ERROR_HANDLE_EOF / ERROR_JOURNAL_NOT_IN_EXIST mean nothing newer.
            if code == 38 || code == 0xC000_00D9 || code == 1179 {
                return Ok((Vec::new(), read.start_usn));
            }
            // A distinct error so a failing read is not mistaken for a failing
            // journal query — both used to report the same message.
            return Err(VolumeError::IoControl {
                op: "FSCTL_READ_USN_JOURNAL",
                code,
            });
        }

        if (returned as usize) < USN_HEADER_PREFIX {
            return Ok((Vec::new(), read.start_usn));
        }

        out.truncate(returned as usize);
        let Some((next_start, record_bytes)) = ntfs::split_usn_output(&out) else {
            return Ok((Vec::new(), read.start_usn));
        };
        let records = ntfs::walk_usn_buffer(record_bytes);

        Ok((records, next_start))
    }

    /// Read `buf.len()` bytes from the volume at `byte_offset`.
    ///
    /// A non-NULL `lpOverlapped` is what positions the read: even though
    /// `Volume::open` does not pass `FILE_FLAG_OVERLAPPED`, Windows honours
    /// `Offset`/`OffsetHigh` on a synchronous handle and reads from the given
    /// offset rather than the handle's file pointer. Verified by
    /// `an_overlapped_offset_positions_a_read_on_a_synchronous_handle`.
    ///
    /// Passing `lpOverlapped` also keeps the read from advancing the shared file
    /// pointer, so concurrent scans can share one handle. An overlapped read that
    /// cannot complete inline would report ERROR_IO_PENDING; volume reads at
    /// these offsets do not, and that case is handled as a hard error below.
    pub fn read_at(&self, byte_offset: u64, buf: &mut [u8]) -> VolumeResult<usize> {
        let mut overlapped: OVERLAPPED = unsafe { core::mem::zeroed() };
        overlapped.Anonymous.Anonymous.Offset = (byte_offset & 0xFFFF_FFFF) as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (byte_offset >> 32) as u32;

        let mut bytes_read = 0u32;
        let ok = win::read_file(
            self.handle,
            Some(buf),
            Some(&mut bytes_read),
            Some(&mut overlapped),
        );

        if let Err(code) = ok {
            // ERROR_HANDLE_EOF means the read ran off the end of the volume.
            if code == 38 {
                return Ok(bytes_read as usize);
            }
            return Err(VolumeError::IoControl {
                op: "ReadFile",
                code,
            });
        }

        Ok(bytes_read as usize)
    }

    /// Read MFT record 0 and collect the full extent map of the MFT's own
    /// `$DATA` attribute — the logical record stream, keyed by VCN.
    ///
    /// Record 0 only ever holds the *first* extent of the run list. When the
    /// MFT's `$DATA` attribute overflows, the continuation extents live in
    /// sibling records named by record 0's `$ATTRIBUTE_LIST`, each carrying its
    /// own `StartingVCN`. Reading only record 0's run list covers a fraction of
    /// the stream and silently drops every record past it — and any attempt to
    /// number records by counting through the physical extents then drifts.
    /// This is the same chase `Indexer::overflow_names` does for names.
    pub fn read_mft_data_runs(&self) -> VolumeResult<Vec<ntfs::MftRun>> {
        let rec = self.read_file_record(0)?.ok_or(VolumeError::IoControl {
            op: "FSCTL_GET_NTFS_FILE_RECORD(0)",
            code: 0,
        })?;
        let mut runs = ntfs::extract_data_runs(&rec).map_err(|_| VolumeError::IoControl {
            op: "extract_data_runs",
            code: 0,
        })?;

        if ntfs::attrs_split(&rec) {
            let Some(list) = ntfs::attr_list_value(&rec) else {
                return Ok(runs);
            };
            // The targets already read: the attribute list names a handful
            // of siblings, so a vector and a scan are cheaper than a table.
            let mut seen: Vec<u64> = Vec::new();
            for entry in ntfs::parse_attr_list(list) {
                // $DATA run-list fragments only. A nonzero VCN marks run data,
                // and `start_vcn == 0` is the fragment already in record 0.
                if entry.attr_type != ntfs::ATTR_DATA || entry.start_vcn == 0 {
                    continue;
                }
                let target = ntfs::frn_number(entry.mft_reference);
                if target == 0 || seen.contains(&target) {
                    continue;
                }
                seen.push(target);
                let Ok(Some(sibling)) = self.read_file_record(target) else {
                    continue;
                };
                if let Ok(mut extra) = ntfs::extract_data_runs(&sibling) {
                    runs.append(&mut extra);
                }
            }
        }

        // Extents may arrive out of record order; the stream order is VCN order.
        runs.sort_by_key(|r| r.start_vcn);
        runs.dedup_by_key(|r| r.start_vcn);
        Ok(runs)
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        if !win::is_invalid(self.handle) {
            let _ = win::close_handle(self.handle);
        }
    }
}

#[cfg(test)]
impl Volume {
    /// A handle-less `Volume` for pure-logic tests (indexer name routing and
    /// the like). Never opens a device; `Drop` sees an invalid handle and
    /// closes nothing.
    pub(crate) fn dummy_for_tests() -> Self {
        Volume {
            handle: core::ptr::null_mut(),
            root: "C:\\".to_string(),
            path: String::new(),
            serial: 0,
            record_size: 1024,
            record_count: 0,
            volume_data: None,
            bytes_per_cluster: 4096,
        }
    }
}

/// Read the volume serial number, which `FSCTL_GET_NTFS_FILE_RECORD` requires.
///
/// The serial is a property of the filesystem, so it must come from the drive
/// root path rather than the `\\.\X:` device path.
fn volume_serial(root: &str) -> Option<u32> {
    let drive = root.trim_end_matches(['\\', '/']);
    let drive = if drive.is_empty() {
        "C:".to_string()
    } else {
        drive.to_string()
    };
    let root_path = format!("{}\\", drive);
    let wide: Vec<u16> = root_path
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();

    let mut serial: u32 = 0;
    let ok =
        win::get_volume_information_w(wide.as_ptr(), None, Some(&mut serial), None, None, None);
    if ok.is_ok() { Some(serial) } else { None }
}

/// Turn `C:\` into the NT device path `\\.\C:` that raw NTFS ioctls require.
fn device_path(root: &str) -> String {
    let trimmed = trim_root(root);
    if trimmed.is_empty() {
        return "\\\\.\\C:".to_string();
    }
    format!("\\\\.\\{trimmed}")
}

/// Roots of every fixed NTFS volume, in drive-letter order (`C:\`, `D:\`, …).
///
/// Fixed only: removable and network volumes are excluded because a USB stick
/// that disappears mid-scan would leave a half-built index, and a network mount
/// has no MFT to read. CD/DVD and RAM disks are excluded for the same reason.
pub fn list_fixed_ntfs_roots() -> Vec<String> {
    let mut out = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        let wide: Vec<u16> = root.encode_utf16().chain(core::iter::once(0)).collect();

        let drive_type = win::get_drive_type_w(wide.as_ptr());
        if drive_type != DRIVE_FIXED {
            continue;
        }

        // Ask the filesystem name rather than assuming: a ReFS fixed disk is
        // not readable with NTFS ioctls and would fail every record read.
        let mut fs_buf = [0u16; 32];
        let ok =
            win::get_volume_information_w(wide.as_ptr(), None, None, None, None, Some(&mut fs_buf));
        if ok.is_err() {
            continue;
        }
        let end = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
        let fs = String::from_utf16_lossy(&fs_buf[..end]);
        if !fs.eq_ignore_ascii_case("NTFS") {
            continue;
        }

        out.push(root);
    }
    out
}

/// The display root that indexed paths are built on, e.g. `C:\`.
///
/// Mirrors `device_path`'s normalisation but keeps the trailing separator, which
/// `join` then trims before appending a child name.
fn volume_root(root: &str) -> String {
    let trimmed = trim_root(root);
    if trimmed.is_empty() {
        return "C:\\".to_string();
    }
    format!("{trimmed}\\")
}

/// Strip the extended-length prefix and any trailing separator from a root path.
fn trim_root(root: &str) -> &str {
    root.trim_start_matches("\\\\?\\")
        .trim_end_matches(['\\', '/'])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These came from `Win32::System::Ioctl`. Pinning the values guards against
    /// a plausible-looking but wrong literal: 0x90040 is a real-looking FSCTL
    /// that the NTFS driver rejects with ERROR_INVALID_FUNCTION.
    #[test]
    fn fsctl_codes_match_the_sdk() {
        assert_eq!(FSCTL_GET_NTFS_FILE_RECORD, 0x0009_0068);
        assert_eq!(FSCTL_READ_USN_JOURNAL, 0x0009_00BB);
    }

    #[test]
    fn device_path_strips_trailing_separator() {
        assert_eq!(device_path("C:\\"), "\\\\.\\C:");
        assert_eq!(device_path("E:"), "\\\\.\\E:");
    }

    #[test]
    fn device_path_handles_extended_prefix() {
        assert_eq!(device_path("\\\\?\\D:\\"), "\\\\.\\D:");
    }

    #[test]
    fn volume_root_keeps_a_trailing_separator_for_display() {
        assert_eq!(volume_root("C:\\"), "C:\\");
        assert_eq!(volume_root("D:"), "D:\\");
        assert_eq!(volume_root("\\\\?\\E:\\"), "E:\\");
        // An empty or separator-only argument falls back to the system volume,
        // matching device_path's own fallback.
        assert_eq!(volume_root(""), "C:\\");
        assert_eq!(volume_root("\\"), "C:\\");
    }

    #[test]
    fn volume_root_and_device_path_strip_the_same_affixes() {
        for input in ["C:\\", "D:", "\\\\?\\E:\\", "F:/"] {
            let root = volume_root(input);
            let device = device_path(input);
            assert_eq!(
                root,
                format!("{}\\", device.trim_start_matches("\\\\.\\")),
                "root and device must agree for {input:?}"
            );
        }
    }

    /// Regression guard for `read_at`.
    ///
    /// `Volume::open` creates a *synchronous* handle (no `FILE_FLAG_OVERLAPPED`),
    /// and it is not obvious that such a handle still honours an `OVERLAPPED`
    /// offset. It does: a non-NULL `lpOverlapped` positions the read regardless.
    /// This pins that down against a real handle so nobody "simplifies" `read_at`
    /// into a bare `ReadFile` with a NULL overlapped, which would silently read
    /// from the handle's file pointer and return the wrong bytes.
    #[test]
    fn an_overlapped_offset_positions_a_read_on_a_synchronous_handle() {
        use crate::win;
        use crate::win::OVERLAPPED;

        let path = std::env::temp_dir().join(format!(
            "zen-rs-read-at-{}-{:?}.bin",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"AAAABBBBCCCC").expect("write temp file");

        let wide: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        // Opened exactly like Volume::open: no FILE_FLAG_OVERLAPPED.
        let handle = win::create_file_w(
            wide.as_ptr(),
            GENERIC_READ_ACCESS,
            FILE_SHARE_READ,
            OPEN_EXISTING,
            0,
        )
        .expect("open temp file");

        let read_at_offset = |offset: u32| -> Vec<u8> {
            let mut overlapped: OVERLAPPED = unsafe { core::mem::zeroed() };
            overlapped.Anonymous.Anonymous.Offset = offset;
            let mut buf = [0u8; 4];
            let mut read = 0u32;
            let r = win::read_file(
                handle,
                Some(&mut buf),
                Some(&mut read),
                Some(&mut overlapped),
            );
            assert!(r.is_ok(), "overlapped read at {offset} should succeed");
            assert_eq!(read, 4);
            buf.to_vec()
        };

        assert_eq!(read_at_offset(4), b"BBBB", "offset must be honoured");
        assert_eq!(
            read_at_offset(8),
            b"CCCC",
            "a positioned read must not be affected by the previous one"
        );

        let _ = win::close_handle(handle);
        let _ = std::fs::remove_file(&path);
    }
}
