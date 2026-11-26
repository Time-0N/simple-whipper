//! The ripping worker.
//!
//! A rip runs on its own OS thread, because every libcdio-paranoia call
//! blocks (often for seconds while paranoia re-reads a bad spot) and the
//! drive/paranoia handles are not `Send`. The thread reports back through an
//! unbounded channel whose receiver is the `Stream` the GUI subscribes to.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::futures::Stream;
use iced::futures::channel::mpsc;
use paranoia::{Drive, FRAMES_PER_SECTOR, Mode, Paranoia, SECTOR_SAMPLES, Stats, Track};

use crate::encode::{self, Format};

/// How many times paranoia may re-read a sector before skipping it.
const MAX_RETRIES: i32 = 20;
/// Send a progress event every this many sectors (75 sectors = 1 s audio).
const PROGRESS_EVERY: u32 = 25;

/// Everything the worker needs. Plain data, so it can cross threads.
#[derive(Debug, Clone)]
pub struct RipRequest {
    pub device: String,
    pub tracks: Vec<Track>,
    pub output_dir: PathBuf,
    pub format: Format,
    /// Drive read offset in samples (stereo frames).
    pub read_offset: i32,
}

/// Messages from the worker to the GUI.
#[derive(Debug, Clone)]
pub enum RipEvent {
    TrackStarted {
        number: u8,
        sectors: u32,
    },
    Progress {
        /// Sectors done in the current track.
        track_done: u32,
        /// Sectors done over the whole job.
        overall_done: u64,
        /// Stats so far for the current track.
        stats: Stats,
    },
    TrackDone {
        number: u8,
        path: PathBuf,
        stats: Stats,
    },
    /// Terminal events — exactly one of these ends every job.
    Finished,
    Cancelled,
    Failed(String),
}

/// Start a rip on a new thread. Setting `cancel` stops it between sectors.
pub fn start(req: RipRequest, cancel: Arc<AtomicBool>) -> impl Stream<Item = RipEvent> {
    let (tx, rx) = mpsc::unbounded();
    std::thread::Builder::new()
        .name("rip".into())
        .spawn(move || {
            let send = |ev| {
                let _ = tx.unbounded_send(ev);
            };
            let end = match rip_all(&req, &cancel, &send) {
                Ok(()) => RipEvent::Finished,
                Err(Stop::Cancelled) => RipEvent::Cancelled,
                Err(Stop::Failed(e)) => RipEvent::Failed(e),
            };
            send(end);
        })
        .expect("spawn rip thread");
    rx
}

enum Stop {
    Cancelled,
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for Stop {
    fn from(e: E) -> Self {
        Stop::Failed(e.to_string())
    }
}

fn rip_all(req: &RipRequest, cancel: &AtomicBool, send: &dyn Fn(RipEvent)) -> Result<(), Stop> {
    std::fs::create_dir_all(&req.output_dir)
        .map_err(|e| format!("cannot create {}: {e}", req.output_dir.display()))?;

    let mut drive = Drive::open(&req.device)?;
    let disc = drive.disc_first_lsn()..=drive.disc_last_lsn();
    let mut paranoia = Paranoia::new(&mut drive, Mode::default())?;

    let mut overall_done = 0u64;
    for track in &req.tracks {
        let path = encode::track_path(&req.output_dir, track.number, req.format);
        let result = rip_track(
            &mut paranoia,
            track,
            &disc,
            req,
            &path,
            cancel,
            send,
            &mut overall_done,
        );
        match result {
            Ok(stats) => send(RipEvent::TrackDone {
                number: track.number,
                path,
                stats,
            }),
            Err(stop) => {
                // Never leave a half-written file that looks complete.
                let _ = std::fs::remove_file(&path);
                return Err(stop);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn rip_track(
    paranoia: &mut Paranoia,
    track: &Track,
    disc: &std::ops::RangeInclusive<i32>,
    req: &RipRequest,
    path: &Path,
    cancel: &AtomicBool,
    send: &dyn Fn(RipEvent),
    overall_done: &mut u64,
) -> Result<Stats, Stop> {
    let plan = TrackPlan::new(track, req.read_offset);
    send(RipEvent::TrackStarted {
        number: track.number,
        sectors: track.sectors(),
    });

    let mut sink = encode::create(path, req.format)?;
    let mut buf = [0i16; SECTOR_SAMPLES];
    let mut stats = Stats::default();
    let _ = paranoia.take_stats(); // drop anything left from the previous track

    // Sectors before the first readable one are never read, so the first
    // real read always follows a seek and every later read is sequential.
    paranoia.seek(plan.sectors.start.max(*disc.start()));

    for (i, sector) in plan.sectors.clone().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Stop::Cancelled);
        }

        if disc.contains(&sector) {
            paranoia.read(&mut buf, MAX_RETRIES)?;
        } else {
            // Offset pushed us before the lead-in or past the lead-out:
            // most drives can't read there, so pad with silence.
            buf.fill(0);
        }
        sink.write(&buf[plan.slice(sector)])?;
        stats.add(&paranoia.take_stats());

        // Progress is counted in track sectors, so the bar ends at 100 %
        // even when the offset makes us read one extra sector.
        let done = (i as u32 + 1).min(track.sectors());
        if done.is_multiple_of(PROGRESS_EVERY) || done == track.sectors() {
            send(RipEvent::Progress {
                track_done: done,
                overall_done: *overall_done + u64::from(done),
                stats,
            });
        }
    }
    *overall_done += u64::from(track.sectors());
    sink.finish()?;
    Ok(stats)
}

/// Which sectors to read for a track, and which samples of each to keep,
/// once the drive's read offset is applied.
///
/// A drive with read offset `+N` delivers audio `N` samples too early, so
/// the true track `[start, end)` lives at `[start + N, end + N)` in what the
/// drive returns. We read every sector touching that window and trim.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackPlan {
    /// First and one-past-last stereo frame to keep, in disc frame numbers.
    frames: Range<i64>,
    /// Sectors to read (may extend one sector beyond the track).
    sectors: Range<i32>,
}

impl TrackPlan {
    fn new(track: &Track, offset: i32) -> Self {
        let fps = FRAMES_PER_SECTOR as i64;
        let start = i64::from(track.first_lsn) * fps + i64::from(offset);
        let end = (i64::from(track.last_lsn) + 1) * fps + i64::from(offset);
        let first_sector = start.div_euclid(fps) as i32;
        let last_sector = (end - 1).div_euclid(fps) as i32;
        TrackPlan {
            frames: start..end,
            sectors: first_sector..last_sector + 1,
        }
    }

    /// Range of *samples* (not frames) within `sector`'s buffer to keep.
    fn slice(&self, sector: i32) -> Range<usize> {
        let fps = FRAMES_PER_SECTOR as i64;
        let sector_start = i64::from(sector) * fps;
        let from = (self.frames.start - sector_start).clamp(0, fps);
        let to = (self.frames.end - sector_start).clamp(0, fps);
        (from as usize * 2)..(to as usize * 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(first: i32, last: i32) -> Track {
        Track {
            number: 1,
            first_lsn: first,
            last_lsn: last,
            is_audio: true,
        }
    }

    fn kept_frames(p: &TrackPlan) -> usize {
        p.sectors.clone().map(|s| p.slice(s).len() / 2).sum()
    }

    #[test]
    fn zero_offset_is_identity() {
        let p = TrackPlan::new(&track(100, 199), 0);
        assert_eq!(p.sectors, 100..200);
        assert_eq!(p.slice(100), 0..SECTOR_SAMPLES);
        assert_eq!(kept_frames(&p), 100 * FRAMES_PER_SECTOR);
    }

    #[test]
    fn positive_offset_reads_one_more_sector() {
        let p = TrackPlan::new(&track(100, 199), 6);
        assert_eq!(p.sectors, 100..201);
        assert_eq!(p.slice(100), 12..SECTOR_SAMPLES); // skip 6 frames
        assert_eq!(p.slice(200), 0..12); // take 6 frames
        assert_eq!(kept_frames(&p), 100 * FRAMES_PER_SECTOR);
    }

    #[test]
    fn negative_offset_starts_a_sector_early() {
        let p = TrackPlan::new(&track(100, 199), -30);
        assert_eq!(p.sectors, 99..200);
        assert_eq!(p.slice(99), (588 - 30) * 2..SECTOR_SAMPLES);
        assert_eq!(p.slice(199), 0..(588 - 30) * 2);
        assert_eq!(kept_frames(&p), 100 * FRAMES_PER_SECTOR);
    }

    #[test]
    fn whole_sector_offsets() {
        let p = TrackPlan::new(&track(0, 9), 588);
        assert_eq!(p.sectors, 1..11);
        assert_eq!(kept_frames(&p), 10 * FRAMES_PER_SECTOR);
        let p = TrackPlan::new(&track(0, 9), -588);
        assert_eq!(p.sectors, -1..9);
        assert_eq!(kept_frames(&p), 10 * FRAMES_PER_SECTOR);
    }
}
