//! Error type for the codec. Everything is recoverable: malformed input,
//! CRC mismatches, and (distinctly) features this pure-Rust codec does not
//! implement (encryption, PPMd filters, arbitrary VM filter programs, the
//! RAR 7.x algorithm).

use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Buffer ended before a complete structure could be read.
    Truncated,
    /// Missing or wrong `Rar!` magic.
    BadMagic,
    /// Structurally invalid header (bad vint, size overflow, ...).
    Malformed(&'static str),
    /// A CRC check failed. `what` says which one (`"rar5 header"`,
    /// `"file data: <name>"`).
    BadCrc(&'static str),
    /// The archive uses a feature this pure-Rust codec does not implement:
    /// encrypted payload, PPMd filters, arbitrary RAR VM filter programs,
    /// or the RAR 7.x compression algorithm. The reference implementation
    /// is RARLAB's `unrar`.
    Unsupported(&'static str),
}

pub type Result<T> = core::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => write!(f, "truncated archive"),
            Error::BadMagic => write!(f, "not a RAR file (bad magic)"),
            Error::Malformed(what) => write!(f, "malformed RAR structure: {what}"),
            Error::BadCrc(what) => write!(f, "CRC check failed: {what}"),
            Error::Unsupported(what) => {
                write!(
                    f,
                    "unsupported RAR feature ({what}); only RARLAB unrar implements it"
                )
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}
