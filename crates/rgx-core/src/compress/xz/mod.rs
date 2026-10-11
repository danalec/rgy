//! XZ container format. Decoding only — rgx and the rest of upstream never
//! need to *produce* an XZ stream, so there is no writer here.

pub mod container;
pub mod crc64;

pub use container::XzReader;
