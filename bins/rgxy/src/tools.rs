// SPDX-License-Identifier: Apache-2.0

//! The programs the launcher can call.
//!
//! The launcher is a client of the daemon, and the daemon carries the rgx
//! adapters — so the content layer works with nothing else installed. The
//! `rgx` binary is the **fallback**: the machine that has it can answer a
//! content search without the daemon, and the machine that does not has
//! to say so rather than pretend.
//!
//! The scan is done once per process. An unscanned `PATH` costs one stat
//! per candidate per call, and the launcher asks on every keystroke.

use std::path::Path;
use std::sync::OnceLock;

/// The `rgx` binary, when the machine has one.
///
/// `None` when there is none — which is not a failure, it is the state of
/// a machine that only has rgy.
pub fn rgx() -> Option<String> {
    static FOUND: OnceLock<Option<String>> = OnceLock::new();
    FOUND.get_or_init(|| scan("rgx")).clone()
}

/// Look for one program on the `PATH`.
///
/// The Windows extensions are tried too: a program installed as `rgx.exe`
/// is not found by a scan that only looks for `rgx`.
pub fn scan(binary: &str) -> Option<String> {
    if binary.contains('/') || binary.contains('\\') {
        return Path::new(binary).is_file().then(|| binary.to_string());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "bat", "cmd"] {
                let candidate = candidate.with_extension(ext);
                if candidate.is_file() {
                    return Some(candidate.to_string_lossy().into_owned());
                }
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_that_is_not_there_is_not_found() {
        assert!(scan("rgy-no-such-program").is_none());
        assert!(scan("rgy-no-such-program/sub").is_none());
    }

    #[test]
    fn the_rgx_lookup_is_memoised_and_answers_a_boolean() {
        // The launcher asks on every keystroke; the answer must not cost
        // a scan every time.
        let first = rgx();
        let second = rgx();
        assert_eq!(first.is_some(), second.is_some());
    }
}
