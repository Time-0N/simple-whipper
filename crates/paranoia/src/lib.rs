//! Safe wrapper around libcdio-paranoia.
//!
//! * [`Drive`] — an opened CD drive with an audio disc in it (owns a
//!   `cdrom_drive_t`). Gives you the table of contents.
//! * [`Paranoia`] — a paranoia reader bound to a drive (owns a
//!   `cdrom_paranoia_t`). Gives you verified, jitter-corrected sectors.
//! * [`Stats`] — what paranoia had to do while reading, collected through the
//!   C status callback.
//!
//! Neither handle is `Send`: open the drive on the thread that will rip.

use std::cell::Cell;
use std::ffi::{CStr, CString, c_long};
use std::marker::PhantomData;
use std::os::raw::c_char;
use std::ptr::NonNull;

use cdio_paranoia_sys as sys;

pub use sys::{SECTOR_BYTES, SECTOR_SAMPLES};

/// Stereo sample frames (one left + one right sample) per CD sector.
pub const FRAMES_PER_SECTOR: usize = 588;
/// CD sectors per second of audio.
pub const SECTORS_PER_SECOND: u32 = 75;

/// `CDDA_MESSAGE_FORGETIT`: tell libcdio not to collect/print messages.
/// (It lives in an anonymous C enum, so bindgen does not export it.)
const MESSAGE_FORGET: i32 = 0;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The device node could not be opened (usually: not in the `cdrom` group).
    PermissionDenied(String),
    /// The device does not exist.
    NoDevice(String),
    /// The drive exists but has no (audio) disc in it, or is not a CD drive.
    NoDisc(String),
    /// `cdio_cddap_open` failed.
    Open(String),
    /// `cdio_paranoia_init` returned NULL.
    ParanoiaInit,
    /// Paranoia gave up on a sector (it returned NULL).
    Read { lsn: i32 },
    /// A device path contained a NUL byte.
    InvalidPath,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::PermissionDenied(d) => write!(
                f,
                "permission denied opening {d} (add your user to the `cdrom`/`optical` group)"
            ),
            Error::NoDevice(d) => write!(f, "no such device: {d}"),
            Error::NoDisc(d) => write!(f, "no audio CD found in {d}"),
            Error::Open(d) => write!(f, "could not open {d} for CD-DA reading"),
            Error::ParanoiaInit => write!(f, "could not initialise paranoia"),
            Error::Read { lsn } => write!(f, "unrecoverable read error at sector {lsn}"),
            Error::InvalidPath => write!(f, "invalid device path"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Table of contents
// ---------------------------------------------------------------------------

/// One entry of the disc's table of contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    /// 1-based track number as printed on the case.
    pub number: u8,
    /// First sector (LSN) of the track.
    pub first_lsn: i32,
    /// Last sector (LSN) of the track, inclusive.
    pub last_lsn: i32,
    /// `false` for data tracks (e.g. the CD-ROM part of an enhanced CD).
    pub is_audio: bool,
}

impl Track {
    /// Length in sectors.
    pub fn sectors(&self) -> u32 {
        (self.last_lsn - self.first_lsn + 1).max(0) as u32
    }

    /// Length in whole seconds (sectors / 75).
    pub fn seconds(&self) -> u32 {
        self.sectors() / SECTORS_PER_SECOND
    }
}

// ---------------------------------------------------------------------------
// Drive
// ---------------------------------------------------------------------------

/// An opened CD drive with a CD-DA disc in it.
pub struct Drive {
    ptr: NonNull<sys::cdrom_drive_t>,
    device: String,
}

impl Drive {
    /// All CD drives libcdio can find, e.g. `["/dev/sr0"]`.
    pub fn list() -> Vec<String> {
        let mut out = Vec::new();
        // SAFETY: returns a NULL-terminated array of C strings (or NULL),
        // which we free with cdio_free_device_list.
        unsafe {
            let list = sys::cdio_get_devices(sys::driver_id_t::DRIVER_DEVICE);
            if list.is_null() {
                return out;
            }
            let mut p = list;
            while !(*p).is_null() {
                out.push(CStr::from_ptr(*p).to_string_lossy().into_owned());
                p = p.add(1);
            }
            sys::cdio_free_device_list(list);
        }
        out
    }

    /// Open `device` (e.g. `/dev/sr0`) and read its table of contents.
    pub fn open(device: &str) -> Result<Drive> {
        // libcdio reports every failure as NULL, so first probe the device
        // node ourselves to produce a useful error.
        if let Err(e) = std::fs::File::open(device) {
            return Err(match e.kind() {
                std::io::ErrorKind::PermissionDenied => Error::PermissionDenied(device.into()),
                std::io::ErrorKind::NotFound => Error::NoDevice(device.into()),
                // ENOMEDIUM and friends: no disc
                _ => Error::NoDisc(device.into()),
            });
        }

        let c_dev = CString::new(device).map_err(|_| Error::InvalidPath)?;
        // SAFETY: c_dev outlives the call; NULL message pointer is allowed
        // with FORGETIT.
        let raw = unsafe {
            sys::cdio_cddap_identify(c_dev.as_ptr(), MESSAGE_FORGET, std::ptr::null_mut())
        };
        let ptr = NonNull::new(raw).ok_or_else(|| Error::NoDisc(device.into()))?;
        let drive = Drive {
            ptr,
            device: device.into(),
        };

        // SAFETY: ptr is a valid drive from identify.
        if unsafe { sys::cdio_cddap_open(drive.ptr.as_ptr()) } != 0 {
            // Drop closes it.
            return Err(Error::Open(device.into()));
        }
        Ok(drive)
    }

    /// The device path this drive was opened with.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Vendor/model string reported by the drive, if any.
    pub fn model(&self) -> String {
        // SAFETY: the struct is valid while self lives; drive_model may be NULL.
        unsafe {
            let m: *const c_char = (*self.ptr.as_ptr()).drive_model;
            if m.is_null() {
                String::new()
            } else {
                CStr::from_ptr(m).to_string_lossy().trim().to_string()
            }
        }
    }

    /// The table of contents. Includes data tracks (see [`Track::is_audio`]).
    pub fn tracks(&self) -> Vec<Track> {
        let d = self.ptr.as_ptr();
        // SAFETY: d is a valid, opened drive.
        unsafe {
            let n = sys::cdio_cddap_tracks(d);
            (1..=n)
                .map(|i| Track {
                    number: i,
                    first_lsn: sys::cdio_cddap_track_firstsector(d, i),
                    last_lsn: sys::cdio_cddap_track_lastsector(d, i),
                    is_audio: sys::cdio_cddap_track_audiop(d, i) == 1,
                })
                .collect()
        }
    }

    /// First readable sector of the disc.
    pub fn disc_first_lsn(&self) -> i32 {
        // SAFETY: valid, opened drive.
        unsafe { sys::cdio_cddap_disc_firstsector(self.ptr.as_ptr()) }
    }

    /// Last readable sector of the disc (inclusive).
    pub fn disc_last_lsn(&self) -> i32 {
        // SAFETY: valid, opened drive.
        unsafe { sys::cdio_cddap_disc_lastsector(self.ptr.as_ptr()) }
    }
}

impl Drop for Drive {
    fn drop(&mut self) {
        // SAFETY: we own the pointer; close also frees it.
        unsafe {
            sys::cdio_cddap_close(self.ptr.as_ptr());
        }
    }
}

// ---------------------------------------------------------------------------
// Paranoia modes
// ---------------------------------------------------------------------------

/// Paranoia correction mode (a bit set of `PARANOIA_MODE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode(i32);

impl Mode {
    /// No verification at all: plain fast reads.
    pub const DISABLE: Mode = Mode(sys::paranoia_mode_t::PARANOIA_MODE_DISABLE as i32);
    /// Full paranoia, and never skip a sector (retry forever).
    pub const FULL: Mode = Mode(sys::paranoia_mode_t::PARANOIA_MODE_FULL as i32);
    /// Full paranoia, but give up (skip) a sector after the retry limit.
    /// This is cdparanoia's default and what we use.
    pub const FULL_ALLOW_SKIP: Mode = Mode(
        (sys::paranoia_mode_t::PARANOIA_MODE_FULL ^ sys::paranoia_mode_t::PARANOIA_MODE_NEVERSKIP)
            as i32,
    );
    /// Only overlap checking (faster, catches jitter but not much else).
    pub const OVERLAP: Mode = Mode(sys::paranoia_mode_t::PARANOIA_MODE_OVERLAP as i32);

    pub fn bits(self) -> i32 {
        self.0
    }
}

impl Default for Mode {
    fn default() -> Self {
        Mode::FULL_ALLOW_SKIP
    }
}

// ---------------------------------------------------------------------------
// Callback statistics
// ---------------------------------------------------------------------------

/// Counters for what paranoia reported through its status callback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Raw reads issued to the drive.
    pub reads: u32,
    /// Overlap verifications performed.
    pub verifies: u32,
    /// Jitter / dropped / duplicated bytes that were corrected.
    pub fixups: u32,
    /// Sectors paranoia gave up on after exhausting retries.
    pub skips: u32,
    /// Hard read errors reported by the drive.
    pub read_errors: u32,
    /// Drift or cache-modelling problems detected.
    pub drift: u32,
}

impl Stats {
    /// `true` if every sector came back without skips or hard errors.
    pub fn is_clean(&self) -> bool {
        self.skips == 0 && self.read_errors == 0
    }

    /// Field-wise sum.
    pub fn add(&mut self, o: &Stats) {
        self.reads += o.reads;
        self.verifies += o.verifies;
        self.fixups += o.fixups;
        self.skips += o.skips;
        self.read_errors += o.read_errors;
        self.drift += o.drift;
    }
}

thread_local! {
    // The C callback has no user-data pointer, so it reports into a
    // per-thread slot. Each Paranoia is !Send, so the reader and the
    // callback always run on the same thread.
    static STATS: Cell<Stats> = const { Cell::new(Stats {
        reads: 0, verifies: 0, fixups: 0, skips: 0, read_errors: 0, drift: 0,
    }) };
}

/// Trampoline handed to `cdio_paranoia_read_limited`. Must never unwind.
unsafe extern "C" fn status_callback(_pos: c_long, mode: sys::paranoia_cb_mode_t::Type) {
    use sys::paranoia_cb_mode_t as cb;
    STATS.with(|cell| {
        let mut s = cell.get();
        match mode {
            cb::PARANOIA_CB_READ => s.reads += 1,
            cb::PARANOIA_CB_VERIFY => s.verifies += 1,
            cb::PARANOIA_CB_FIXUP_EDGE
            | cb::PARANOIA_CB_FIXUP_ATOM
            | cb::PARANOIA_CB_FIXUP_DROPPED
            | cb::PARANOIA_CB_FIXUP_DUPED => s.fixups += 1,
            cb::PARANOIA_CB_SKIP => s.skips += 1,
            cb::PARANOIA_CB_READERR => s.read_errors += 1,
            cb::PARANOIA_CB_DRIFT | cb::PARANOIA_CB_CACHEERR => s.drift += 1,
            _ => {}
        }
        cell.set(s);
    });
}

// ---------------------------------------------------------------------------
// Paranoia reader
// ---------------------------------------------------------------------------

/// A paranoia reader bound to (and mutably borrowing) a [`Drive`].
pub struct Paranoia<'d> {
    ptr: NonNull<sys::cdrom_paranoia_t>,
    next_lsn: i32,
    _drive: PhantomData<&'d mut Drive>,
}

impl<'d> Paranoia<'d> {
    pub fn new(drive: &'d mut Drive, mode: Mode) -> Result<Self> {
        // SAFETY: drive is valid and opened; the borrow keeps it alive and
        // unused elsewhere while the paranoia object exists.
        let raw = unsafe { sys::cdio_paranoia_init(drive.ptr.as_ptr()) };
        let ptr = NonNull::new(raw).ok_or(Error::ParanoiaInit)?;
        // SAFETY: valid paranoia object.
        unsafe { sys::cdio_paranoia_modeset(ptr.as_ptr(), mode.bits()) };
        STATS.with(|c| c.set(Stats::default()));
        Ok(Paranoia {
            ptr,
            next_lsn: 0,
            _drive: PhantomData,
        })
    }

    /// Position the reader so the next [`read`](Self::read) returns `lsn`.
    pub fn seek(&mut self, lsn: i32) {
        // SAFETY: valid paranoia object. libc SEEK_SET == 0.
        unsafe { sys::cdio_paranoia_seek(self.ptr.as_ptr(), lsn, 0) };
        self.next_lsn = lsn;
    }

    /// Read the next verified sector into `buf` (interleaved L/R, host byte
    /// order). `max_retries` bounds how hard paranoia tries before skipping.
    pub fn read(&mut self, buf: &mut [i16; SECTOR_SAMPLES], max_retries: i32) -> Result<()> {
        // SAFETY: valid object; the returned buffer holds SECTOR_SAMPLES
        // i16s and stays valid until the next read, so copy it out now.
        let data = unsafe {
            sys::cdio_paranoia_read_limited(self.ptr.as_ptr(), Some(status_callback), max_retries)
        };
        let lsn = self.next_lsn;
        self.next_lsn += 1;
        if data.is_null() {
            return Err(Error::Read { lsn });
        }
        unsafe {
            std::ptr::copy_nonoverlapping(data, buf.as_mut_ptr(), SECTOR_SAMPLES);
        }
        Ok(())
    }

    /// Return the stats collected since the last call (or since `new`) and
    /// reset them.
    pub fn take_stats(&mut self) -> Stats {
        STATS.with(|c| c.replace(Stats::default()))
    }
}

impl Drop for Paranoia<'_> {
    fn drop(&mut self) {
        // SAFETY: we own the pointer.
        unsafe { sys::cdio_paranoia_free(self.ptr.as_ptr()) };
    }
}

/// libcdio-paranoia version string, e.g. `"10.2+2.0.2"`.
pub fn version() -> String {
    // SAFETY: returns a static C string.
    unsafe {
        CStr::from_ptr(sys::cdio_paranoia_version())
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_reported() {
        assert!(!version().is_empty());
    }

    #[test]
    fn track_length_math() {
        let t = Track {
            number: 1,
            first_lsn: 0,
            last_lsn: 74,
            is_audio: true,
        };
        assert_eq!(t.sectors(), 75);
        assert_eq!(t.seconds(), 1);
    }

    #[test]
    fn missing_device_is_reported() {
        assert_eq!(
            Drive::open("/dev/does-not-exist").err(),
            Some(Error::NoDevice("/dev/does-not-exist".into()))
        );
    }

    #[test]
    fn default_mode_allows_skip() {
        assert_eq!(Mode::default().bits(), 0xff ^ 0x20);
    }

    /// Needs a real drive with an audio CD: `cargo test -p paranoia -- --ignored`
    #[test]
    #[ignore]
    fn read_first_second_of_disc() {
        let dev = Drive::list().into_iter().next().expect("no CD drive found");
        let mut drive = Drive::open(&dev).expect("open drive");
        let tracks = drive.tracks();
        assert!(!tracks.is_empty());
        println!("{} — {} tracks", drive.model(), tracks.len());
        let first = tracks
            .iter()
            .find(|t| t.is_audio)
            .expect("no audio track")
            .first_lsn;
        let mut p = Paranoia::new(&mut drive, Mode::default()).unwrap();
        p.seek(first);
        let mut buf = [0i16; SECTOR_SAMPLES];
        for _ in 0..75 {
            p.read(&mut buf, 20).unwrap();
        }
        let s = p.take_stats();
        println!("{s:?}");
        assert!(s.reads > 0);
    }
}
