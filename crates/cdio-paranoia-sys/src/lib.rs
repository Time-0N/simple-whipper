//! Raw, unsafe bindings to libcdio-paranoia.
//!
//! Generated at build time by bindgen from `wrapper.h`. Use the `paranoia`
//! crate for a safe API.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
#![allow(clippy::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

/// Bytes in one raw CD-DA sector (588 stereo frames × 2 ch × 2 bytes).
pub const SECTOR_BYTES: usize = 2352;
/// Signed 16-bit samples in one sector (interleaved L/R).
pub const SECTOR_SAMPLES: usize = SECTOR_BYTES / 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_reports_version() {
        let v = unsafe { std::ffi::CStr::from_ptr(cdio_paranoia_version()) };
        assert!(!v.to_bytes().is_empty());
    }

    #[test]
    fn frame_size_matches() {
        assert_eq!(CDIO_CD_FRAMESIZE_RAW as usize, SECTOR_BYTES);
    }
}
