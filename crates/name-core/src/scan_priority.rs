// SPDX-License-Identifier: Apache-2.0

//! The scan workers' interactive priority.
//!
//! A search is interactive work: somebody is waiting for the answer, so the
//! threads doing the scan should not be starved by the rest of the machine.
//! Windows says so with MMCSS ("Games") and `SetThreadPriority`; macOS says
//! so with `QOS_CLASS_USER_INTERACTIVE`. Linux has no such class, and what
//! is here is an approximation of those two, not their guarantee -- the
//! kernel has no way to say "this work is interactive" beyond these knobs.
//!
//! - **Affinity** ([`sched_setaffinity`]) pins the worker to the machine's
//!   performance cores when the machine says which they are. This is the
//!   half that holds on an ordinary desktop: the scan keeps the fast
//!   silicon instead of sharing it with whatever else is running.
//! - **Nice** ([`setpriority`]) puts the worker at the default level. A
//!   daemon started from a background shell inherits nice 19 and is starved
//!   by everything else on the machine; the worker asks for the default
//!   instead. Linux refuses to raise a nice value without `CAP_SYS_NICE`,
//!   so where the process inherited the default the value stays where it
//!   is -- the affinity is the lever that applies everywhere.
//!
//! The syscalls are declared here rather than taken from `libc`, the same
//! way `content.rs` declares the Darwin bits: name-core carries no libc
//! dependency, on purpose.

use alloc::vec::Vec;

/// Make the calling thread a scan worker.
///
/// The platform seam: on Linux this is affinity plus nice; where the
/// platform has its own mechanism it does nothing.
pub fn apply() {
    #[cfg(target_os = "linux")]
    linux::apply();
}

/// The CPUs the scan workers may run on.
///
/// `allowed` is what the process may use (`sched_getaffinity`); `fast` is
/// the machine's performance cores. The fast set wins when the process may
/// use any of them -- a cpuset can take the fast cores away, and then there
/// is nothing to pin to. Otherwise the workers keep the whole allowed set:
/// a scan spread over every CPU the process has is slower than one that is
/// not.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn select_cpus(allowed: &[u32], fast: &[u32]) -> Vec<u32> {
    let mut picked: Vec<u32> = fast.iter().copied().filter(|c| allowed.contains(c)).collect();
    picked.sort_unstable();
    picked.dedup();
    if !picked.is_empty() {
        return picked;
    }
    let mut all = allowed.to_vec();
    all.sort_unstable();
    all.dedup();
    all
}

#[cfg(all(target_os = "linux", feature = "std"))]
mod linux {
    //! Linux's approximation of the interactive priority the other platforms
    //! have natively.

    use crate::sync::OnceCell;
    use alloc::format;
    use alloc::vec::Vec;
    use std::cell::Cell;
    use std::path::Path;

    /// `PRIO_PROCESS` with `who == 0`: the calling thread. Linux's nice
    /// value is per-thread, so this is the worker's own value and not the
    /// process's.
    const PRIO_PROCESS: i32 = 0;

    /// The nice value the workers ask for: the default.
    ///
    /// Above the default -- the nice 19 a background shell hands its jobs --
    /// a search is starved by everything else on the machine. Below it the
    /// search would outrank the user's own work. The default is the only
    /// level that is neither.
    const SCAN_NICE: i32 = 0;

    /// The CPU mask, in bytes: 8192 bits is as many CPUs as the kernel has.
    const MASK_BYTES: usize = 1024;

    /// Where the kernel describes the CPUs.
    const SYSFS: &str = "/sys/devices/system/cpu";

    unsafe extern "C" {
        fn sched_getaffinity(pid: i32, cpusetsize: usize, mask: *mut u32) -> i32;
        fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const u32) -> i32;
        fn getpriority(which: i32, who: u32) -> i32;
        fn setpriority(which: i32, who: u32, prio: i32) -> i32;
    }

    /// The CPUs the workers may run on, decided once per process.
    ///
    /// Once per process because the set is the machine's, and reading it
    /// costs a file read per CPU; once per thread because the affinity is
    /// per thread.
    static SCAN_CPUS: OnceCell<Vec<u32>> = OnceCell::new();

    std::thread_local! {
        /// Whether this thread has been set up already.
        ///
        /// The setup is a handful of syscalls: once per thread is free,
        /// once per scan is noise, once per entry would be a disaster.
        static SET_UP: Cell<bool> = const { Cell::new(false) };
    }

    /// Make this thread a scan worker: pin it, and put it at the default
    /// nice.
    pub fn apply() {
        if SET_UP.with(|s| s.get()) {
            return;
        }
        SET_UP.with(|s| s.set(true));
        let cpus = SCAN_CPUS.get_or_init(scan_cpus);
        pin(cpus);
        apply_nice();
    }

    /// The CPUs the workers may run on: the machine's performance cores
    /// when it says which they are, else everything the process may use.
    fn scan_cpus() -> Vec<u32> {
        let allowed = allowed_cpus();
        if allowed.is_empty() {
            return allowed;
        }
        super::select_cpus(&allowed, &fast_cpus(Path::new(SYSFS), &allowed))
    }

    /// The CPUs this process may run on (`sched_getaffinity`).
    fn allowed_cpus() -> Vec<u32> {
        let mut mask = [0u32; MASK_BYTES / 4];
        // The kernel returns the size of its own mask; glibc normalizes that
        // to 0 and musl passes it through, so only a negative return is a
        // failure.
        let rc = unsafe { sched_getaffinity(0, MASK_BYTES, mask.as_mut_ptr()) };
        if rc < 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (word, bits) in mask.iter().enumerate() {
            let mut bits = *bits;
            while bits != 0 {
                out.push((word * 32 + bits.trailing_zeros() as usize) as u32);
                bits &= bits - 1;
            }
        }
        out
    }

    /// The CPUs the machine calls fast.
    ///
    /// Two sources, in order:
    ///
    /// - `topology/core_type`: on an Intel hybrid part the kernel names the
    ///   performance cores (2) and the efficiency cores (1). The attribute
    ///   does not exist on a homogeneous machine.
    /// - `cpufreq/cpuinfo_max_freq`: the hardware's top frequency per CPU.
    ///   The CPUs at the top frequency are the fast ones; on a homogeneous
    ///   machine they are all equal and there is nothing to choose.
    fn fast_cpus(root: &Path, allowed: &[u32]) -> Vec<u32> {
        let kinds: Vec<Option<i32>> = allowed.iter().map(|&c| core_type(root, c)).collect();
        if kinds.iter().all(|k| k.is_some()) {
            let fast: Vec<u32> = allowed.iter().zip(&kinds).filter(|(_, k)| **k == Some(2)).map(|(&c, _)| c).collect();
            if !fast.is_empty() && fast.len() < allowed.len() {
                return fast;
            }
        }
        let freqs: Vec<Option<u64>> = allowed.iter().map(|&c| max_freq(root, c)).collect();
        if freqs.iter().any(|f| f.is_none()) {
            return Vec::new();
        }
        let freqs: Vec<u64> = freqs.into_iter().map(|f| f.unwrap_or(0)).collect();
        let top = freqs.iter().copied().max().unwrap_or(0);
        let fast: Vec<u32> = allowed.iter().zip(&freqs).filter(|(_, f)| **f == top).map(|(&c, _)| c).collect();
        // Every CPU at the same frequency: the machine has no fast subset
        // to choose, and the workers keep the set they have.
        if fast.len() == allowed.len() { Vec::new() } else { fast }
    }

    /// The kind of core a CPU is, per the kernel: 1 is an efficiency core,
    /// 2 a performance core. `None` when the machine does not say.
    fn core_type(root: &Path, cpu: u32) -> Option<i32> {
        let path = root.join(format!("cpu{cpu}/topology/core_type"));
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// The CPU's top hardware frequency, in kHz.
    fn max_freq(root: &Path, cpu: u32) -> Option<u64> {
        let path = root.join(format!("cpu{cpu}/cpufreq/cpuinfo_max_freq"));
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// Pin this thread to `cpus`.
    ///
    /// A mask the kernel refuses -- an empty set, or one it cannot honour --
    /// leaves the thread where the scheduler had it.
    fn pin(cpus: &[u32]) {
        if cpus.is_empty() {
            return;
        }
        let mut mask = [0u32; MASK_BYTES / 4];
        for &c in cpus {
            let c = c as usize;
            if c / 32 >= mask.len() {
                continue;
            }
            mask[c / 32] |= 1 << (c % 32);
        }
        unsafe { sched_setaffinity(0, MASK_BYTES, mask.as_ptr()) };
    }

    /// Put this thread at the default nice value, if it is below it.
    ///
    /// Linux refuses to raise a nice value without `CAP_SYS_NICE`: where it
    /// refuses, the inherited value stands. That is the honest shape of
    /// this half -- on an ordinary desktop the affinity is the lever that
    /// applies, and this one applies where the daemon was given the
    /// capability.
    fn apply_nice() {
        // getpriority returns -1 both for "failed" and for a real nice of
        // -1. Both mean the thread is already at or above the default, and
        // neither is worth acting on.
        let current = unsafe { getpriority(PRIO_PROCESS, 0) };
        if current <= SCAN_NICE {
            return;
        }
        unsafe { setpriority(PRIO_PROCESS, 0, SCAN_NICE) };
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use alloc::vec;
        use std::fs;
        use std::path::PathBuf;

        /// A fake `/sys/devices/system/cpu`: the kernel's description of a
        /// machine, written out so the detection can be checked without one.
        fn fake_sysfs(name: &str, cpus: &[(&str, &str)]) -> PathBuf {
            let root = std::env::temp_dir().join(format!("rgy-sysfs-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            for (cpu, (kind, khz)) in cpus.iter().enumerate() {
                let cpu = cpu as u32;
                if !kind.is_empty() {
                    let dir = root.join(format!("cpu{cpu}/topology"));
                    fs::create_dir_all(&dir).expect("the fake sysfs tree");
                    fs::write(dir.join("core_type"), kind).expect("the fake sysfs tree");
                }
                if !khz.is_empty() {
                    let dir = root.join(format!("cpu{cpu}/cpufreq"));
                    fs::create_dir_all(&dir).expect("the fake sysfs tree");
                    fs::write(dir.join("cpuinfo_max_freq"), khz).expect("the fake sysfs tree");
                }
            }
            root
        }

        /// A hybrid part: the kernel names the performance cores, and those
        /// are the ones the scan gets.
        #[test]
        fn a_hybrid_part_gives_its_performance_cores() {
            let root = fake_sysfs("hybrid", &[("2", ""), ("2", ""), ("1", ""), ("1", "")]);
            let fast = fast_cpus(&root, &[0, 1, 2, 3]);
            let _ = fs::remove_dir_all(&root);
            assert_eq!(fast, vec![0, 1]);
        }

        /// No hybrid: the fastest CPUs are the ones at the top frequency.
        #[test]
        fn the_fastest_cpus_win_when_the_kernel_names_no_core_type() {
            let root = fake_sysfs("freq", &[("", "5000000"), ("", "4000000"), ("", "5000000")]);
            let fast = fast_cpus(&root, &[0, 1, 2]);
            let _ = fs::remove_dir_all(&root);
            assert_eq!(fast, vec![0, 2]);
        }

        /// A machine with no fast subset: every CPU at the same frequency,
        /// so there is nothing to choose and the workers keep their set.
        #[test]
        fn a_homogeneous_machine_has_no_fast_subset() {
            let root = fake_sysfs("flat", &[("", "4000000"), ("", "4000000")]);
            let fast = fast_cpus(&root, &[0, 1]);
            let _ = fs::remove_dir_all(&root);
            assert!(fast.is_empty(), "a homogeneous machine offered a fast subset: {fast:?}");
        }

        /// A machine that says nothing at all: no fast subset either.
        #[test]
        fn a_machine_that_says_nothing_has_no_fast_subset() {
            let root = fake_sysfs("silent", &[("", "")]);
            let fast = fast_cpus(&root, &[0]);
            let _ = fs::remove_dir_all(&root);
            assert!(fast.is_empty());
        }

        /// The syscalls do what they say.
        ///
        /// The affinity and the nice value are per-thread on Linux, so this
        /// touches the thread the test runs on and nothing else: the set
        /// stays inside what the process may use, and the nice value never
        /// gets worse.
        #[test]
        fn the_worker_keeps_a_cpu_set_it_may_run_on() {
            let before = allowed_cpus();
            assert!(!before.is_empty(), "the test process has no CPU to run on");
            let nice_before = unsafe { getpriority(PRIO_PROCESS, 0) };

            apply();

            let after = allowed_cpus();
            assert!(!after.is_empty(), "the worker was left with no CPU to run on");
            assert!(
                after.iter().all(|c| before.contains(c)),
                "the worker was pinned outside the set the process may use: {after:?} not in {before:?}"
            );
            let nice_after = unsafe { getpriority(PRIO_PROCESS, 0) };
            assert!(nice_after <= nice_before, "the worker's nice got worse: {nice_before} -> {nice_after}");
            assert!(nice_after <= 0 || nice_after == nice_before, "the worker's nice went above the default: {nice_after}");
        }

        /// Pinning to a subset really restricts the thread: the mask
        /// round-trips through the kernel.
        #[test]
        fn pinning_to_a_subset_restricts_the_thread() {
            let allowed = allowed_cpus();
            if allowed.len() < 2 {
                return;
            }
            let one = vec![allowed[0]];
            pin(&one);
            let after = allowed_cpus();
            pin(&allowed);
            let restored = allowed_cpus();
            assert_eq!(after, one, "the thread was not pinned to the CPU it was given");
            assert_eq!(restored, allowed, "the thread was not given its CPUs back");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::select_cpus;
    use alloc::vec;

    /// The fast cores win when the process may use them.
    #[test]
    fn the_fast_cores_win_when_the_process_may_use_them() {
        assert_eq!(select_cpus(&[0, 1, 2, 3], &[2, 3]), vec![2, 3]);
    }

    /// A cpuset that took the fast cores away leaves the whole set: there
    /// is nothing to pin to.
    #[test]
    fn a_cpuset_that_took_the_fast_cores_away_leaves_the_whole_set() {
        assert_eq!(select_cpus(&[0, 1], &[2, 3]), vec![0, 1]);
    }

    /// No fast cores means every CPU the process has.
    #[test]
    fn no_fast_cores_means_the_whole_allowed_set() {
        assert_eq!(select_cpus(&[0, 1, 2], &[]), vec![0, 1, 2]);
    }

    /// The result is a set: sorted, without repeats.
    #[test]
    fn the_result_is_sorted_and_without_repeats() {
        assert_eq!(select_cpus(&[3, 1, 2], &[3, 2, 3]), vec![2, 3]);
        assert_eq!(select_cpus(&[3, 1, 2], &[]), vec![1, 2, 3]);
    }
}
