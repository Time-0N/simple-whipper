//! The iced application: state, messages, update logic and view.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{
    Column, button, checkbox, column, container, pick_list, progress_bar, row, rule, scrollable,
    text, text_input,
};
use iced::{Alignment, Element, Fill, Task, Theme};
use paranoia::{Drive, Stats, Track};

use crate::config::Config;
use crate::encode::Format;
use crate::rip::{self, RipEvent, RipRequest};

/// Accepted read offset range in samples. Real drives are within ±3000.
const OFFSET_LIMIT: i32 = 10_000;

/// What we know about the disc in the selected drive.
#[derive(Debug, Clone)]
pub struct DiscInfo {
    pub model: String,
    pub tracks: Vec<Track>,
}

#[derive(Debug)]
enum Disc {
    /// Nothing read yet (or no drive selected).
    Unknown,
    Loading,
    Loaded {
        model: String,
        /// Each track with its "rip me" checkbox state.
        tracks: Vec<(Track, bool)>,
    },
    Error(String),
}

/// Result of one finished track, shown in the summary.
#[derive(Debug, Clone)]
struct TrackResult {
    number: u8,
    path: PathBuf,
    stats: Stats,
}

#[derive(Debug)]
enum Rip {
    Idle,
    Running {
        cancel: Arc<AtomicBool>,
        current: Option<u8>,
        track_done: u32,
        track_total: u32,
        overall_done: u64,
        overall_total: u64,
        stats: Stats,
        results: Vec<TrackResult>,
    },
    /// The rip ended. `outcome` is the user-facing summary line.
    Done {
        results: Vec<TrackResult>,
        outcome: String,
        ok: bool,
    },
}

#[derive(Debug, Clone)]
pub enum Message {
    DrivesListed(Vec<String>),
    DriveSelected(String),
    RefreshDisc,
    DiscLoaded(Result<DiscInfo, String>),
    TrackToggled(usize, bool),
    AllTracksToggled(bool),
    OutputDirChanged(String),
    BrowseOutputDir,
    OutputDirPicked(Option<PathBuf>),
    OffsetChanged(String),
    FormatSelected(Format),
    StartRip,
    CancelRip,
    Rip(RipEvent),
    OpenOutputDir,
    DismissSummary,
}

pub struct App {
    config: Config,
    drives: Vec<String>,
    /// Raw text of the offset field, so half-typed input like "-" survives.
    offset_text: String,
    output_text: String,
    disc: Disc,
    rip: Rip,
    /// One-line feedback at the bottom of the window.
    status: String,
}

/// Run blocking work (libcdio calls) off the GUI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.expect("worker thread panicked")
}

fn read_disc(device: String) -> Result<DiscInfo, String> {
    let drive = Drive::open(&device).map_err(|e| e.to_string())?;
    let tracks = drive.tracks();
    if !tracks.iter().any(|t| t.is_audio) {
        return Err(format!("the disc in {device} has no audio tracks"));
    }
    Ok(DiscInfo {
        model: drive.model(),
        tracks,
    })
}

fn mmss(sectors: u64) -> String {
    let secs = sectors / u64::from(paranoia::SECTORS_PER_SECOND);
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn stats_line(s: &Stats) -> String {
    format!(
        "reads {} · verified {} · fixed {} · skipped {} · errors {}",
        s.reads, s.verifies, s.fixups, s.skips, s.read_errors
    )
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let config = Config::load();
        let app = Self {
            offset_text: config.read_offset.to_string(),
            output_text: config.output_dir.display().to_string(),
            config,
            drives: Vec::new(),
            disc: Disc::Unknown,
            rip: Rip::Idle,
            status: format!("libcdio-paranoia {}", paranoia::version()),
        };
        (
            app,
            Task::perform(blocking(Drive::list), Message::DrivesListed),
        )
    }

    pub fn theme(&self) -> Theme {
        Theme::Dark
    }

    fn ripping(&self) -> bool {
        matches!(self.rip, Rip::Running { .. })
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.status = format!("could not save settings: {e}");
        }
    }

    fn parsed_offset(&self) -> Option<i32> {
        self.offset_text
            .trim()
            .trim_start_matches('+')
            .parse::<i32>()
            .ok()
            .filter(|o| o.abs() <= OFFSET_LIMIT)
    }

    fn selected_tracks(&self) -> Vec<Track> {
        match &self.disc {
            Disc::Loaded { tracks, .. } => tracks
                .iter()
                .filter(|(t, on)| *on && t.is_audio)
                .map(|(t, _)| *t)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn load_disc(&mut self) -> Task<Message> {
        let Some(device) = self.config.device.clone() else {
            self.disc = Disc::Unknown;
            return Task::none();
        };
        self.disc = Disc::Loading;
        Task::perform(blocking(move || read_disc(device)), Message::DiscLoaded)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::DrivesListed(drives) => {
                self.drives = drives;
                // Keep the saved drive if it still exists, else take the first.
                let keep = self
                    .config
                    .device
                    .as_ref()
                    .is_some_and(|d| self.drives.contains(d));
                if !keep {
                    self.config.device = self.drives.first().cloned();
                }
                if self.drives.is_empty() {
                    self.disc = Disc::Error("no CD drive found".into());
                    return Task::none();
                }
                return self.load_disc();
            }
            Message::DriveSelected(device) => {
                self.config.device = Some(device);
                self.save_config();
                return self.load_disc();
            }
            Message::RefreshDisc => {
                // Re-scan drives too, in case one was plugged in.
                return Task::perform(blocking(Drive::list), Message::DrivesListed);
            }
            Message::DiscLoaded(result) => {
                self.disc = match result {
                    Ok(info) => Disc::Loaded {
                        model: info.model,
                        tracks: info.tracks.into_iter().map(|t| (t, t.is_audio)).collect(),
                    },
                    Err(e) => Disc::Error(e),
                };
            }
            Message::TrackToggled(i, on) => {
                if let Disc::Loaded { tracks, .. } = &mut self.disc
                    && let Some(entry) = tracks.get_mut(i)
                {
                    entry.1 = on && entry.0.is_audio;
                }
            }
            Message::AllTracksToggled(on) => {
                if let Disc::Loaded { tracks, .. } = &mut self.disc {
                    for (t, sel) in tracks {
                        *sel = on && t.is_audio;
                    }
                }
            }
            Message::OutputDirChanged(s) => {
                self.output_text = s;
                self.config.output_dir = PathBuf::from(self.output_text.trim());
                self.save_config();
            }
            Message::BrowseOutputDir => {
                let start = self.config.output_dir.clone();
                return Task::perform(
                    async move {
                        let mut dialog =
                            rfd::AsyncFileDialog::new().set_title("Choose output folder");
                        if start.is_dir() {
                            dialog = dialog.set_directory(&start);
                        }
                        dialog.pick_folder().await.map(|h| h.path().to_path_buf())
                    },
                    Message::OutputDirPicked,
                );
            }
            Message::OutputDirPicked(Some(dir)) => {
                self.output_text = dir.display().to_string();
                self.config.output_dir = dir;
                self.save_config();
            }
            Message::OutputDirPicked(None) => {}
            Message::OffsetChanged(s) => {
                self.offset_text = s;
                if let Some(o) = self.parsed_offset() {
                    self.config.read_offset = o;
                    self.save_config();
                }
            }
            Message::FormatSelected(f) => {
                self.config.format = f;
                self.save_config();
            }
            Message::StartRip => {
                let tracks = self.selected_tracks();
                let (Some(device), Some(offset)) =
                    (self.config.device.clone(), self.parsed_offset())
                else {
                    return Task::none();
                };
                if tracks.is_empty() || self.ripping() {
                    return Task::none();
                }
                let cancel = Arc::new(AtomicBool::new(false));
                self.rip = Rip::Running {
                    cancel: cancel.clone(),
                    current: None,
                    track_done: 0,
                    track_total: 1,
                    overall_done: 0,
                    overall_total: tracks.iter().map(|t| u64::from(t.sectors())).sum(),
                    stats: Stats::default(),
                    results: Vec::new(),
                };
                self.status = "ripping…".into();
                let req = RipRequest {
                    device,
                    tracks,
                    output_dir: self.config.output_dir.clone(),
                    format: self.config.format,
                    read_offset: offset,
                };
                return Task::run(rip::start(req, cancel), Message::Rip);
            }
            Message::CancelRip => {
                if let Rip::Running { cancel, .. } = &self.rip {
                    cancel.store(true, Ordering::Relaxed);
                    self.status = "cancelling…".into();
                }
            }
            Message::Rip(event) => self.on_rip_event(event),
            Message::OpenOutputDir => {
                // xdg-open hands the folder to the desktop's default file
                // manager (Nautilus, Dolphin, Thunar, …).
                let dir = self.config.output_dir.clone();
                match std::process::Command::new("xdg-open")
                    .arg(&dir)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    // Reap the child in the background so it doesn't linger
                    // as a zombie.
                    Ok(mut child) => {
                        std::thread::spawn(move || child.wait());
                    }
                    Err(e) => self.status = format!("could not run xdg-open: {e}"),
                }
            }
            Message::DismissSummary => self.rip = Rip::Idle,
        }
        Task::none()
    }

    fn on_rip_event(&mut self, event: RipEvent) {
        let Rip::Running {
            current,
            track_done,
            track_total,
            overall_done,
            stats,
            results,
            ..
        } = &mut self.rip
        else {
            return;
        };
        let finish = |results: &mut Vec<TrackResult>, outcome: String, ok: bool| Rip::Done {
            results: std::mem::take(results),
            outcome,
            ok,
        };
        match event {
            RipEvent::TrackStarted { number, sectors } => {
                *current = Some(number);
                *track_done = 0;
                *track_total = sectors.max(1);
                *stats = Stats::default();
            }
            RipEvent::Progress {
                track_done: d,
                overall_done: o,
                stats: s,
            } => {
                *track_done = d;
                *overall_done = o;
                *stats = s;
            }
            RipEvent::TrackDone {
                number,
                path,
                stats,
            } => results.push(TrackResult {
                number,
                path,
                stats,
            }),
            RipEvent::Finished => {
                let clean = results.iter().all(|r| r.stats.is_clean());
                let n = results.len();
                let outcome = if clean {
                    format!("Ripped {n} track(s), all read cleanly.")
                } else {
                    format!("Ripped {n} track(s) — some sectors could not be read perfectly.")
                };
                self.status = "done".into();
                self.rip = finish(results, outcome, clean);
            }
            RipEvent::Cancelled => {
                self.status = "cancelled".into();
                self.rip = finish(results, "Rip cancelled.".into(), false);
            }
            RipEvent::Failed(e) => {
                self.status = "rip failed".into();
                self.rip = finish(results, format!("Rip failed: {e}"), false);
            }
        }
    }

    // -----------------------------------------------------------------------
    // View
    // -----------------------------------------------------------------------

    pub fn view(&self) -> Element<'_, Message> {
        let busy = self.ripping();

        // --- Settings ----------------------------------------------------
        let drive_row = row![
            text("Drive").width(110),
            pick_list(
                self.drives.as_slice(),
                self.config.device.as_ref(),
                Message::DriveSelected
            )
            .placeholder("no drive found")
            .width(Fill),
            button("Refresh").on_press_maybe((!busy).then_some(Message::RefreshDisc)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut out_input = text_input("/path/to/output", &self.output_text);
        if !busy {
            out_input = out_input.on_input(Message::OutputDirChanged);
        }
        let output_row = row![
            text("Output folder").width(110),
            out_input.width(Fill),
            button("Browse…").on_press_maybe((!busy).then_some(Message::BrowseOutputDir)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut offset_input = text_input("0", &self.offset_text).width(90);
        if !busy {
            offset_input = offset_input.on_input(Message::OffsetChanged);
        }
        let offset_hint = if self.parsed_offset().is_some() {
            text("samples (see the AccurateRip drive offset list)").size(13)
        } else {
            text(format!(
                "enter a whole number between -{OFFSET_LIMIT} and {OFFSET_LIMIT}"
            ))
            .size(13)
            .style(text::danger)
        };
        let offset_row = row![text("Read offset").width(110), offset_input, offset_hint]
            .spacing(8)
            .align_y(Alignment::Center);

        let format_row = row![
            text("Format").width(110),
            pick_list(
                &Format::ALL[..],
                Some(self.config.format),
                Message::FormatSelected
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        // --- Track list --------------------------------------------------
        let tracks: Element<'_, Message> = match &self.disc {
            Disc::Unknown => text("Select a drive.").into(),
            Disc::Loading => text("Reading table of contents…").into(),
            Disc::Error(e) => text(e).style(text::danger).into(),
            Disc::Loaded { model, tracks } => {
                let all = tracks.iter().filter(|(t, _)| t.is_audio).all(|(_, on)| *on);
                let total: u64 = tracks
                    .iter()
                    .filter(|(t, _)| t.is_audio)
                    .map(|(t, _)| u64::from(t.sectors()))
                    .sum();
                let header = row![
                    checkbox(all)
                        .label("All")
                        .on_toggle_maybe((!busy).then_some(Message::AllTracksToggled)),
                    text(format!(
                        "{} — {} tracks, {}",
                        if model.is_empty() { "CD drive" } else { model },
                        tracks.len(),
                        mmss(total)
                    ))
                    .size(13),
                ]
                .spacing(16)
                .align_y(Alignment::Center);

                let list = Column::with_children(tracks.iter().enumerate().map(|(i, (t, on))| {
                    let label = if t.is_audio {
                        format!("Track {:02}   {}", t.number, mmss(u64::from(t.sectors())))
                    } else {
                        format!("Track {:02}   data track", t.number)
                    };
                    checkbox(*on)
                        .label(label)
                        .on_toggle_maybe(
                            (!busy && t.is_audio).then_some(move |v| Message::TrackToggled(i, v)),
                        )
                        .into()
                }))
                .spacing(4);

                column![header, scrollable(list).height(Fill).width(Fill)]
                    .spacing(8)
                    .into()
            }
        };

        // --- Rip controls / progress / summary ---------------------------
        let can_rip = !busy
            && self.parsed_offset().is_some()
            && !self.selected_tracks().is_empty()
            && !self.output_text.trim().is_empty();

        let bottom: Element<'_, Message> = match &self.rip {
            Rip::Idle => row![
                button("Rip selected tracks").on_press_maybe(can_rip.then_some(Message::StartRip))
            ]
            .into(),
            Rip::Running {
                current,
                track_done,
                track_total,
                overall_done,
                overall_total,
                stats,
                results,
                ..
            } => {
                let track_label = match current {
                    Some(n) => format!(
                        "Track {n:02}: {:.0}%",
                        *track_done as f32 * 100.0 / *track_total as f32
                    ),
                    None => "Opening drive…".into(),
                };
                let overall = *overall_done as f32 / (*overall_total).max(1) as f32;
                column![
                    text(track_label),
                    progress_bar(0.0..=*track_total as f32, *track_done as f32).girth(10),
                    text(format!(
                        "Overall: {:.0}% ({} track(s) done)",
                        overall * 100.0,
                        results.len()
                    )),
                    progress_bar(0.0..=1.0, overall).girth(10),
                    text(stats_line(stats)).size(12),
                    button("Cancel")
                        .style(button::danger)
                        .on_press(Message::CancelRip),
                ]
                .spacing(6)
                .into()
            }
            Rip::Done {
                results,
                outcome,
                ok,
            } => {
                let headline = text(outcome).style(if *ok { text::success } else { text::warning });
                let lines = Column::with_children(results.iter().map(|r| {
                    let name = r
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let flag = if r.stats.is_clean() { "✓" } else { "⚠" };
                    text(format!(
                        "{flag} {:02}  {name}  ({} fixed, {} skipped)",
                        r.number, r.stats.fixups, r.stats.skips
                    ))
                    .size(13)
                    .into()
                }))
                .spacing(2);
                column![
                    headline,
                    lines,
                    row![
                        button("Open folder").on_press(Message::OpenOutputDir),
                        button("Back")
                            .style(button::secondary)
                            .on_press(Message::DismissSummary),
                    ]
                    .spacing(8),
                ]
                .spacing(8)
                .into()
            }
        };

        let content = column![
            drive_row,
            output_row,
            offset_row,
            format_row,
            rule::horizontal(1),
            container(tracks).height(Fill),
            rule::horizontal(1),
            bottom,
            text(&self.status).size(12).style(text::secondary),
        ]
        .spacing(10)
        .padding(16);

        container(content).width(Fill).height(Fill).into()
    }
}
