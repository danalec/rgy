// SPDX-License-Identifier: Apache-2.0

//! How many logical processors share a core.
//!
//! A machine's processors are not its parallelism. A core with two threads
//! is one core: the second thread shares the first one's load units, its
//! cache and its place in the memory system, and the tree's scan is a
//! memory-bound pass over arrays. The difference was measured on a
//! sixteen-core machine with two threads per core: the same scan took 55
//! microseconds with sixteen workers and 81 with thirty-two -- and the
//! tree that came before the pool measured 61, because its worker chain
//! lost half of them to a race and never ran the thirty-two it had asked
//! for. The tree that runs them all is honest, and honest is 47% slower.
//!
//! So the width the tree asks for by itself is the machine's *cores*. A
//! caller that wants a width of its own says so, and gets it.

/// How many logical processors share a core.
///
/// `None` when the platform does not say -- and the caller then keeps the
/// logical count, which is what it always was.
#[cfg(feature = "std")]
pub(crate) fn smt_width() -> Option<usize> {
    #[cfg(target_os = "windows")]
    {
        windows::smt_width()
    }
    #[cfg(target_os = "linux")]
    {
        linux::smt_width()
    }
    #[cfg(target_os = "macos")]
    {
        macos::smt_width()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Windows says which processors share a core.
#[cfg(all(feature = "std", target_os = "windows"))]
mod windows {
    use core::ffi::c_void;

    /// `RelationProcessorCore`: the relationships that say which logical
    /// processors are the same core.
    const RELATION_PROCESSOR_CORE: u32 = 0;

    /// Where the core's mask sits inside a relationship: the header is 26
    /// bytes and the mask is eight-byte aligned.
    const MASK_OFFSET: usize = 32;

    /// The relationship type, so a buffer that is not what we asked for is
    /// refused rather than believed.
    const RELATIONSHIP_CORE: u32 = 0;

    unsafe extern "system" {
        fn GetLogicalProcessorInformationEx(
            relationship: u32,
            buffer: *mut c_void,
            returned: *mut u32,
        ) -> i32;
    }

    /// The first core's mask: the processors that share one core.
    ///
    /// The call asks the OS for the relationships and reads the first one:
    /// a relationship is a header, the core's mask and the group it is in,
    /// and the first core's mask is the set of processors that share it.
    /// How many there are in it is how many processors a core carries.
    pub fn smt_width() -> Option<usize> {
        let mut size: u32 = 0;
        // The first call asks how much room the relationships need.
        unsafe {
            GetLogicalProcessorInformationEx(
                RELATION_PROCESSOR_CORE,
                core::ptr::null_mut(),
                &mut size,
            )
        };
        if (size as usize) < MASK_OFFSET + 8 {
            return None;
        }
        let mut buffer = alloc::vec![0u8; size as usize];
        let mut got: u32 = size;
        let ok = unsafe {
            GetLogicalProcessorInformationEx(
                RELATION_PROCESSOR_CORE,
                buffer.as_mut_ptr() as *mut c_void,
                &mut got,
            )
        };
        if ok == 0 {
            return None;
        }
        // The first relationship must be a core's, or this is a layout this
        // code does not understand and must not guess at.
        let relationship = u32::from_le_bytes(buffer[0..4].try_into().ok()?);
        if relationship != RELATIONSHIP_CORE {
            return None;
        }
        let mask = u64::from_le_bytes(buffer[MASK_OFFSET..MASK_OFFSET + 8].try_into().ok()?);
        let width = mask.count_ones() as usize;
        if width == 0 { None } else { Some(width) }
    }
}

/// Linux says which processors share a core.
#[cfg(all(feature = "std", target_os = "linux"))]
mod linux {
    /// Where the kernel describes the processors.
    const SYSFS: &str = "/sys/devices/system/cpu";

    /// How many processors the kernel lists for one core.
    ///
    /// The kernel writes, per processor, the list of the processors that
    /// share its core: "0,1" on a machine with the second thread, "0" on a
    /// machine without it. The widest list is the machine's answer -- a
    /// hybrid part has cores of both kinds, and the widest is the one that
    /// carries the second thread.
    ///
    /// Only the first processors are read: the width is the same for every
    /// core of a kind, and the sysfs tree of a large machine is a hundred
    /// files to walk for nothing.
    const READ: usize = 8;

    /// How many processors share a core.
    pub fn smt_width() -> Option<usize> {
        let mut widest = 0;
        let mut read = 0;
        for entry in std::fs::read_dir(SYSFS).ok()? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !is_processor(name) {
                continue;
            }
            let path = std::path::Path::new(SYSFS)
                .join(name)
                .join("topology")
                .join("thread_siblings_list");
            let Ok(list) = std::fs::read_to_string(&path) else {
                continue;
            };
            let width = siblings(&list);
            if width > widest {
                widest = width;
            }
            read += 1;
            if read >= READ {
                break;
            }
        }
        if widest == 0 { None } else { Some(widest) }
    }

    /// Whether a directory of the sysfs tree is a processor's.
    fn is_processor(name: &str) -> bool {
        let Some(digits) = name.strip_prefix("cpu") else {
            return false;
        };
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
    }

    /// How many processors a sibling list names.
    ///
    /// The kernel writes the list as a comma-separated list ("0,2,4") or as
    /// a range ("0-1"), and both mean the same thing: this many processors
    /// share the core. "0" is one, "0,1" is two, "0-1" is two.
    fn siblings(list: &str) -> usize {
        let mut count = 0;
        for part in list.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.split_once('-') {
                Some((first, last)) => {
                    let first = first.trim().parse::<u32>().unwrap_or(0);
                    let last = last.trim().parse::<u32>().unwrap_or(first);
                    if last >= first {
                        count += (last - first + 1) as usize;
                    }
                }
                None => count += 1,
            }
        }
        count
    }

    #[cfg(test)]
    mod tests {
        use super::{is_processor, siblings};

        #[test]
        fn a_sibling_list_counts_its_processors() {
            assert_eq!(siblings("0\n"), 1);
            assert_eq!(siblings("0,1\n"), 2);
            assert_eq!(siblings("0,2,4,6\n"), 4);
            assert_eq!(siblings("0-1\n"), 2);
            assert_eq!(siblings("0-3\n"), 4);
            assert_eq!(siblings("0,4-6\n"), 4);
            assert_eq!(siblings(""), 0);
        }

        #[test]
        fn the_processor_directories_are_recognised() {
            assert!(is_processor("cpu0"));
            assert!(is_processor("cpu127"));
            assert!(!is_processor("cpu"));
            assert!(!is_processor("cpufreq"));
            assert!(!is_processor("cpuidle"));
            assert!(!is_processor("self"));
        }
    }
}

/// macOS says how many processors the machine has, and how many cores.
#[cfg(all(feature = "std", target_os = "macos"))]
mod macos {
    use core::ffi::{c_char, c_void};

    /// The machine's processors, and the cores they live on.
    const LOGICAL: &str = "hw.logicalcpu";
    const PHYSICAL: &str = "hw.physicalcpu";

    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> i32;
    }

    /// How many processors share a core: the machine's processors over its
    /// cores.
    pub fn smt_width() -> Option<usize> {
        let logical = count(LOGICAL)?;
        let physical = count(PHYSICAL)?;
        if physical == 0 {
            return None;
        }
        Some((logical / physical).max(1))
    }

    /// A count the kernel holds under a name.
    fn count(name: &str) -> Option<usize> {
        let mut value: u32 = 0;
        let mut len = core::mem::size_of::<u32>();
        let ok = unsafe {
            sysctlbyname(
                name.as_ptr() as *const c_char,
                &mut value as *mut u32 as *mut c_void,
                &mut len,
                core::ptr::null_mut(),
                0,
            )
        };
        if ok != 0 || len != core::mem::size_of::<u32>() {
            return None;
        }
        Some(value as usize)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    #[test]
    fn the_width_the_platform_says_is_sane() {
        if let Some(width) = super::smt_width() {
            assert!(width >= 1, "a core carries at least one processor");
            assert!(width <= 64, "a core does not carry {width} processors");
        }
    }

    #[test]
    fn the_default_width_is_within_the_machines_processors() {
        let logical = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let width = crate::default_workers();
        assert!(width >= 1, "the tree asks for at least one worker");
        assert!(
            width <= logical,
            "the tree asked for {width} workers on a machine with {logical}"
        );
    }
}
