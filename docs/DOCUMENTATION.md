# simple-whipper — Developer Documentation

simple-whipper is a small CD ripper for the Linux desktop. It reads audio CDs through **libcdio-paranoia**, the maintained fork of Xiph's cdparanoia, using hand-written Rust bindings. The GUI is built with **iced 0.14**. Each track is written as a FLAC or WAV file.

This document explains how the program works, layer by layer. It also contains the **complete source code** of every file in the project. The listings are generated from the real files by `docs/sync-docs.py`, so they always match the code. Run `python3 docs/sync-docs.py` after editing code; `--check` fails if the doc is stale.

---

## Contents

1. [Features](#1-features)
2. [Building and running](#2-building-and-running)
3. [Architecture](#3-architecture)
4. [Layer 1 — `cdio-paranoia-sys` (raw FFI)](#4-layer-1--cdio-paranoia-sys-raw-ffi)
5. [Layer 2 — `paranoia` (safe wrapper)](#5-layer-2--paranoia-safe-wrapper)
6. [Layer 3 — the rip worker](#6-layer-3--the-rip-worker)
7. [Layer 4 — encoders](#7-layer-4--encoders)
8. [Layer 5 — the iced GUI](#8-layer-5--the-iced-gui)
9. [Settings file](#9-settings-file)
10. [Read offsets explained](#10-read-offsets-explained)
11. [Testing](#11-testing)
12. [Troubleshooting](#12-troubleshooting)
13. [Limitations and next steps](#13-limitations-and-next-steps)
14. [Full source listing](#14-full-source-listing)

---

## 1. Features

- **Drive selection.** All CD drives that libcdio finds are listed in a drop-down. **Refresh** scans the drives again and re-reads the disc.
- **Table of contents.** Every track is shown with its length. Data tracks (for example the CD-ROM part of an enhanced CD) are listed but cannot be selected.
- **Track selection.** Each track has a checkbox, and an **All** checkbox toggles every track.
- **Output folder.** Type the path in, or press **Browse…** to open the desktop's native folder picker (through the XDG portal).
- **Read offset correction.** You set the drive's sample offset once. The app saves it and applies it at sample precision.
- **Formats.** FLAC (lossless and compressed, encoded in pure Rust) or WAV.
- **Paranoia reading.** Reads use full verification and jitter correction (`PARANOIA_MODE_FULL` without `NEVERSKIP`) with up to 20 retries per sector.
- **Live progress.** There is one progress bar for the current track and one for the whole rip, plus live counters for reads, verifications, fixes, skips and errors.
- **Cancel.** Cancelling stops the rip after the current sector and deletes the partly written file.
- **Summary.** After a rip, each track is listed with a ✓ (clean read) or ⚠ (some sectors were skipped or had errors).
- **Open folder.** This button opens the output directory in your preferred file manager through `xdg-open`.
- **Saved settings.** The drive, output folder, offset and format are saved between runs.

## 2. Building and running

All dependencies come from the Nix flake:

```sh
nix develop           # or: direnv allow  (the repo has an .envrc with `use flake`)
cargo run --release
```

The dev shell provides:

| Package | Why |
|---|---|
| `libcdio`, `libcdio-paranoia` | The C libraries we link against. Their headers are used by bindgen. |
| `pkg-config` | Lets `build.rs` find those libraries. |
| `rustPlatform.bindgenHook` | Sets `LIBCLANG_PATH` and `BINDGEN_EXTRA_CLANG_ARGS` so bindgen's clang can find the libc headers. Without it the build fails with `'sys/types.h' file not found`. |
| `wayland`, `libxkbcommon`, `vulkan-loader`, `libGL`, `libx11`, … | iced/winit/wgpu load these at runtime with `dlopen`, so they are put on `LD_LIBRARY_PATH`. |
| `xdg-utils` | Provides `xdg-open` for the **Open folder** button. |

**Permissions.** Your user must be able to open the drive's device node (`/dev/sr0`). On most desktops logind grants the user at the seat an ACL for it automatically. Otherwise, add yourself to the `cdrom` group (on Arch, the `optical` group). If this is missing, the GUI shows *"permission denied opening /dev/sr0 (add your user to the `cdrom`/`optical` group)"*.

Outside Nix, install the development packages for libcdio and libcdio-paranoia, plus clang, then run `cargo build`. Package names:
- Debian/Ubuntu: `libcdio-dev`, `libcdio-paranoia-dev`, `libclang-dev`
- Fedora: `libcdio-devel`, `libcdio-paranoia-devel`, `clang-devel`
- Arch: `libcdio`, `libcdio-paranoia`, `clang`

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│  GUI thread (iced)                       src/app.rs, src/main.rs     │
│   App { config, drives, disc, rip, status }                          │
│     update(Message) ──► Task::perform(blocking(...))  (TOC reads)    │
│                    └──► Task::run(rip::start(..), Message::Rip)      │
└───────────────▲──────────────────────────────────────────────────────┘
                │ RipEvent over futures::mpsc (unbounded)
┌───────────────┴──────────────────────────────────────────────────────┐
│  "rip" OS thread                          src/rip.rs                 │
│   Drive::open → Paranoia::new → for each track:                      │
│     TrackPlan (offset math) → seek → read sectors → trim → Sink      │
└───────────────┬───────────────────────────────┬──────────────────────┘
                │                               │
┌───────────────▼──────────────┐   ┌────────────▼─────────────────────┐
│ paranoia crate (safe)        │   │ encode.rs                        │
│  Drive, Paranoia, Stats,     │   │  WavSink (hound)                 │
│  status-callback trampoline  │   │  FlacSink (flacenc, streaming)   │
└───────────────┬──────────────┘   └──────────────────────────────────┘
┌───────────────▼──────────────┐
│ cdio-paranoia-sys (bindgen)  │
└───────────────┬──────────────┘
                ▼
     libcdio_paranoia.so / libcdio_cdda.so / libcdio.so
```

The workspace contains three crates:

```
Cargo.toml                       workspace + the `simple-whipper` binary
├── crates/cdio-paranoia-sys/    raw bindings (build.rs, wrapper.h)
├── crates/paranoia/             safe API
└── src/                         the application
    ├── main.rs                  iced entry point
    ├── app.rs                   state, messages, update, view
    ├── rip.rs                   worker thread + offset math
    ├── encode.rs                WAV and FLAC sinks
    └── config.rs                ~/.config/simple-whipper/config.toml
```

Two threading rules shape the design:

1. **libcdio calls block.** A single sector read can take seconds while paranoia re-reads a scratch. Reading the TOC can also stall while the drive spins up. None of this may run on the GUI thread. Short jobs such as listing drives or reading the TOC run through a small `blocking()` helper: it spawns a thread and returns a future that resolves through a oneshot channel. A rip gets its own long-lived thread.
2. **The C handles are not `Send`.** `Drive` and `Paranoia` hold raw pointers, so the compiler refuses to move them between threads, and that is intentional. The rip thread opens its own `Drive`. Only plain data crosses threads: `RipRequest` goes in and `RipEvent` comes out.

## 4. Layer 1 — `cdio-paranoia-sys` (raw FFI)

`build.rs` does three things:

1. It asks pkg-config for `libcdio`, `libcdio_cdda` and `libcdio_paranoia`. Each `probe` also prints the `cargo:rustc-link-lib` / `-search` lines, so linking needs no further setup.
2. It runs bindgen over `wrapper.h`, passing pkg-config's include paths to clang.
3. It **allowlists** only the functions and types the safe layer uses. This keeps the generated file small and the API surface obvious.

The C enums (`paranoia_mode_t`, `paranoia_cb_mode_t`, `driver_id_t`) are generated with `constified_enum_module`. The result is a module of `const` values, for example `paranoia_cb_mode_t::PARANOIA_CB_READ`, not a Rust `enum`. If C passes a value we don't know, a Rust `enum` would be undefined behaviour; a plain integer is always valid.

The include order in `wrapper.h` matters: `paranoia.h` uses types (`cdrom_drive_t`) that are declared in `cdda.h`.

One constant had to be written by hand: `CDDA_MESSAGE_FORGETIT` (0). It is defined in an anonymous C enum, which bindgen does not export by name.

## 5. Layer 2 — `paranoia` (safe wrapper)

### `Drive`

| Method | C function(s) |
|---|---|
| `Drive::list()` | `cdio_get_devices(DRIVER_DEVICE)`, `cdio_free_device_list` |
| `Drive::open(dev)` | `cdio_cddap_identify`, `cdio_cddap_open` |
| `tracks()` | `cdio_cddap_tracks`, `_track_firstsector`, `_track_lastsector`, `_track_audiop` |
| `disc_first_lsn()` / `disc_last_lsn()` | `cdio_cddap_disc_firstsector` / `_lastsector` |
| `model()` | reads `drive_model` from the C struct |
| `Drop` | `cdio_cddap_close` (also frees the struct) |

libcdio reports **every** failure from `identify` as `NULL`, whether the device is missing, permission was denied, or there is no disc. To give the GUI a useful message, `Drive::open` first tries `std::fs::File::open(device)` and maps the `io::ErrorKind` to `PermissionDenied`, `NoDevice` or `NoDisc`.

### `Paranoia<'d>`

`Paranoia::new(&mut drive, mode)` mutably borrows the drive. This means:

- the borrow checker guarantees the drive outlives the paranoia object, which holds a pointer to it;
- nobody can use the drive for anything else while it is being read through paranoia.

`read()` calls `cdio_paranoia_read_limited`. The pointer it returns refers to a buffer **owned by libcdio that is overwritten by the next read**, so the 1176 samples are copied into the caller's `[i16; 1176]` straight away. A `NULL` return means paranoia gave up completely; this becomes `Error::Read { lsn }`.

Sample format: interleaved L/R signed 16-bit in host byte order. libcdio already byte-swaps if needed.

### Modes

| `Mode` | Bits | Meaning |
|---|---|---|
| `FULL_ALLOW_SKIP` (default) | `0xff ^ 0x20` | Full verification, overlap and jitter correction. After `max_retries` the sector is skipped (counted in `Stats::skips`). This is cdparanoia's default. |
| `FULL` | `0xff` | Like the default, but it never skips and retries forever, so it can hang on a badly damaged disc. |
| `OVERLAP` | `0x04` | Overlap checks only. Faster. |
| `DISABLE` | `0x00` | Raw reads with no correction. |

### The callback trampoline

paranoia reports what it is doing (reads, verifications, fixes, skips, errors) through a callback with this signature:

```c
void callback(long position, paranoia_cb_mode_t mode);
```

There is **no user-data pointer**, so a closure can't be passed. The wrapper uses an `unsafe extern "C" fn status_callback` that increments counters in a `thread_local!` `Cell<Stats>`. This is sound because:

- `Paranoia` is `!Send`, so the thread that calls `read()`, which invokes the callback synchronously, is always the thread that owns the paranoia object. Two rips on two threads each get their own counters.
- The callback only does integer arithmetic on a `Cell`. It can't panic, so it never unwinds across the FFI boundary.

`take_stats()` returns the counters and resets them. The rip worker calls it after every sector to add up per-track statistics.

## 6. Layer 3 — the rip worker

`rip::start(req, cancel)` spawns a thread named `rip` and returns the receiving end of an unbounded `futures::mpsc` channel. That receiver is already a `Stream<Item = RipEvent>`. The GUI passes it to `Task::run(stream, Message::Rip)`, which turns every event into a message.

Event protocol, for each track in order:

```
TrackStarted{number, sectors}
Progress{track_done, overall_done, stats}   (every 25 sectors and on the last one)
TrackDone{number, path, stats}
```

The job then ends with **exactly one** of `Finished`, `Cancelled` or `Failed(msg)`.

**Cancellation** uses an `Arc<AtomicBool>` that the loop checks before each sector. On cancel or error, the current track's file is deleted so a truncated file can't be mistaken for a good one. Tracks that were already finished are kept.

**Per-sector loop.** For each sector in the track's plan (see [§10](#10-read-offsets-explained)):

- If the sector lies inside the disc, it is read through paranoia.
- Otherwise it is filled with zeros. Most drives cannot read before the first sector or past the lead-out ("overread"), and padding with silence is what EAC and whipper do as well.
- Only the part of the sector the plan says to keep is passed to the encoder.

The first sector actually read always comes after an explicit `seek`, and all later reads are sequential. Zero-padded sectors can only occur at the very start (negative offset on track 1) or the very end (positive offset on the last track), so the reader's position never needs to jump.

## 7. Layer 4 — encoders

`encode::Sink` is a two-method trait: `write(&[i16])` and `finish()`. `encode::create(path, format)` returns the matching implementation as a `Box<dyn Sink>`.

- **`WavSink`** wraps `hound::WavWriter` (44.1 kHz, stereo, 16-bit). `finish()` patches the RIFF sizes in the header.
- **`FlacSink`** uses **flacenc**, a pure-Rust FLAC encoder, so there is no extra C dependency. flacenc's high-level API, `encode_with_fixed_block_size`, needs the whole signal in memory. To stream instead:
  1. Write a 42-byte placeholder header: `fLaC` + metadata block header + 34-byte STREAMINFO.
  2. Collect samples until a block of 4096 stereo frames is full. Pass the block to `(FrameBuf, Context)`: `FrameBuf` holds the samples, and `Context` keeps a running MD5 and sample count. Encode it with `encode_fixed_size_frame`, write the frame bytes to disk, and record its size with `StreamInfo::update_frame_info`.
  3. In `finish()`, encode the last (shorter) block, then set the MD5, the total sample count and the block sizes (fixed at 4096). A `Stream` with no frames serialises to exactly `fLaC` + STREAMINFO, so it is written into the 42-byte placeholder after seeking back to offset 0.

  The result passes `flac -t`, which verifies the MD5 by decoding the file.

Files are named `NN - Track NN.flac` (or `.wav`). Tagging is planned; see §13.

## 8. Layer 5 — the iced GUI

`main.rs` builds the application with `iced::application(App::new, App::update, App::view)` and a fixed dark theme.

### State

```
App
├── config: Config          // saved settings (drive, folder, offset, format)
├── drives: Vec<String>     // from Drive::list()
├── offset_text / output_text   // raw text in the inputs
├── disc: Disc              // Unknown | Loading | Loaded{model, tracks:[(Track, selected)]} | Error
├── rip:  Rip               // Idle | Running{cancel, progress…, results} | Done{results, outcome, ok}
└── status: String          // footer line
```

The offset field keeps its own raw text, so half-typed input like `-` is not rejected while you type. `parsed_offset()` validates it (an integer, optional `+`, absolute value ≤ 10 000). While it's invalid, a red hint appears and **Rip** is disabled.

### Message flow

| User action | Message | Effect |
|---|---|---|
| App starts | → `DrivesListed` | Lists drives in the background, keeps the saved drive if present, then reads the TOC |
| Pick drive | `DriveSelected` | Saves the config and reads the TOC in the background → `DiscLoaded` |
| Refresh | `RefreshDisc` | Scans drives again → `DrivesListed` → TOC |
| Tick a track or All | `TrackToggled` / `AllTracksToggled` | Updates the selection (data tracks are never selectable) |
| Type or browse the folder | `OutputDirChanged` / `BrowseOutputDir` → `OutputDirPicked` | rfd portal dialog; saves the config |
| Type the offset | `OffsetChanged` | Saves the config when the value is valid |
| Pick a format | `FormatSelected` | Saves the config |
| Rip | `StartRip` | Builds a `RipRequest` and calls `Task::run(rip::start(..), Message::Rip)` |
| (worker) | `Rip(RipEvent)` | Updates the progress bars and stats, then switches to `Done` |
| Cancel | `CancelRip` | Sets the `AtomicBool` |
| Open folder | `OpenOutputDir` | Runs `xdg-open <dir>` detached, with a thread that reaps the child |
| Back | `DismissSummary` | Returns to `Idle` |

All inputs are disabled while a rip is running: the `on_press_maybe`, `on_toggle_maybe` and conditional `on_input` calls are given `None`.

### Layout

```
┌────────────────────────────────────────────────────────────┐
│ Drive          [/dev/sr0                    ▾] [Refresh]   │
│ Output folder  [/home/you/Music/CD Rips      ] [Browse…]   │
│ Read offset    [6   ] samples (see the AccurateRip …)      │
│ Format         [FLAC ▾]                                    │
│ ────────────────────────────────────────────────────────── │
│ [x] All   HL-DT-ST DVDRAM — 12 tracks, 48:31               │
│ [x] Track 01   4:12                                        │
│ [x] Track 02   3:55                                        │
│  …  (scrollable)                                           │
│ ────────────────────────────────────────────────────────── │
│ Track 03: 42%                                              │
│ [██████████░░░░░░░░░░░░]                                   │
│ Overall: 21% (2 track(s) done)                             │
│ [████░░░░░░░░░░░░░░░░░░]                                   │
│ reads 3120 · verified 3100 · fixed 2 · skipped 0 · errors 0│
│ [Cancel]                                                   │
│ status line                                                │
└────────────────────────────────────────────────────────────┘
```

After the rip, the progress area is replaced by the summary: a headline, one ✓/⚠ line per track, and the **Open folder** and **Back** buttons.

## 9. Settings file

The settings are stored in `~/.config/simple-whipper/config.toml` (more precisely `$XDG_CONFIG_HOME/simple-whipper/config.toml`):

```toml
device = "/dev/sr0"
output_dir = "/home/you/Music/CD Rips"
read_offset = 6
format = "Flac"
```

Every key is optional (`#[serde(default)]`). If the file is missing or can't be read, the defaults are used: the first drive found, `~/Music/CD Rips` (the XDG music directory), offset 0, FLAC. The file is saved whenever a setting changes.

## 10. Read offsets explained

CD drives don't return audio from exactly the requested position. Each drive model is shifted by a fixed number of samples, called its **read offset**. For example, many LG and Plextor drives need `+6`, and older Pioneer drives `+48`. Bit-identical rips, and AccurateRip verification later on, depend on correcting this. You can look up your drive's value in the AccurateRip drive offset database. The convention is the same as in EAC, whipper and `cd-paranoia -O`: a correction of `+N` means "read N samples later".

`TrackPlan::new(track, offset)` works in **stereo frames** (588 per sector):

```
start = first_lsn × 588 + offset            (first frame to keep)
end   = (last_lsn + 1) × 588 + offset       (one past the last frame)
sectors to read = ⌊start / 588⌋ ..= ⌊(end − 1) / 588⌋   (floored division)
```

`slice(sector)` clamps `[start, end)` to the sector's own frame range and converts the result to sample indices (×2 for the two channels).

Example: offset `+6`, track sectors 100–199.

- Read sectors 100–200, which is one extra sector.
- From sector 100, drop the first 6 frames.
- From sector 200, keep only the first 6 frames.
- Total: exactly 100 × 588 frames, the same length as the track.

Negative offsets mirror this: reading starts one sector early. When the plan reaches before the first readable sector or past the lead-out, those sectors are filled with silence (§6). The unit tests in `rip.rs` cover zero, positive, negative and whole-sector offsets.

## 11. Testing

```sh
cargo test --workspace                       # unit tests, no hardware needed
cargo test -p paranoia -- --ignored --nocapture  # needs an audio CD in the drive
cargo clippy --workspace --all-targets
python3 docs/sync-docs.py --check            # doc listings up to date
```

What's covered:

| Test | Checks |
|---|---|
| `cdio-paranoia-sys::links_and_reports_version` | Linking works and the library answers |
| `paranoia::missing_device_is_reported` | Errors are mapped to `NoDevice` and friends |
| `paranoia::default_mode_allows_skip` | The mode bit arithmetic |
| `paranoia::read_first_second_of_disc` (ignored) | Reading a real disc: TOC and 75 sectors |
| `encode::wav_roundtrip` | WAV output is bit-exact |
| `encode::flac_header_and_size` | FLAC header layout and total sample count |
| `rip::*offset*` | Offset trimming always yields exactly the track's length |
| `config::roundtrip_and_partial_files` | TOML round-trip; missing keys fall back to defaults |

Manual end-to-end check against the reference tool:

```sh
cd-paranoia -d /dev/sr0 -w 3 ref.wav      # offset 0
# rip track 3 as WAV with offset 0 in the GUI, then:
cmp <(tail -c +45 ref.wav) <(tail -c +45 "03 - Track 03.wav") && echo identical
flac -t "03 - Track 03.flac"              # FLAC integrity + MD5
```

## 12. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Build: `'sys/types.h' file not found` | bindgen's clang can't find the libc headers. Use `nix develop` (it provides `bindgenHook`), or install clang and your distro's libc development headers. |
| Build: `pkg-config could not find libcdio_paranoia` | Install the libcdio-paranoia development package. |
| "permission denied opening /dev/sr0" | Add your user to the `cdrom` or `optical` group, then log out and back in. |
| "no audio CD found in /dev/cdrom" | No disc, a data-only disc, or the tray is open. Insert the disc and press **Refresh**. |
| GUI says "Reading table of contents…" for a long time | The drive is spinning up or struggling with the disc. The GUI stays responsive because the read runs on a background thread. |
| Rip summary shows ⚠ | paranoia skipped sectors after 20 retries. Clean the disc and rip that track again. |
| **Open folder** does nothing | `xdg-open` is missing (install `xdg-utils`) or no default file manager is set (`xdg-mime default <app>.desktop inode/directory`). |
| **Browse…** does nothing | No XDG desktop portal is running (`xdg-desktop-portal` plus a backend such as `-gtk`, `-kde`, `-gnome` or `-wlr`). Type the path instead. |

## 13. Limitations and next steps

- **No metadata yet.** Files are named by track number. Next: MusicBrainz disc ID lookup, then tags (Vorbis comments for FLAC) and `Artist/Album/NN - Title` paths.
- **No AccurateRip verification.** Next: compute AR v1/v2 CRCs per track while ripping (the samples already pass through the worker) and compare them with the database. That would also allow automatic offset detection.
- **No pregap/HTOA handling, no cue sheet.**
- Existing files with the same name are **overwritten**.

## 14. Full source listing

Each file is shown in full. To refresh this section, run `python3 docs/sync-docs.py`.

### 14.1 `Cargo.toml` — workspace and application manifest

The root package is the binary. The two library crates are workspace members. `rfd` uses its default `xdg-portal` backend, so no GTK is linked.

<!-- include: Cargo.toml -->
```toml
[workspace]
members = [".", "crates/cdio-paranoia-sys", "crates/paranoia"]
resolver = "3"

[package]
name = "simple-whipper"
version = "0.1.0"
edition = "2024"
description = "Small iced GUI CD ripper built on libcdio-paranoia"
license = "MIT"

[dependencies]
paranoia = { path = "crates/paranoia" }
iced = { version = "0.14", features = ["tokio"] }
hound = "3.5"
flacenc = "0.5"
rfd = "0.18"
serde = { version = "1", features = ["derive"] }
toml = "1"
dirs = "6"

[profile.release]
lto = "thin"
```
<!-- /include -->

### 14.2 `flake.nix` — development environment

<!-- include: flake.nix -->
```nix
{
  description = "simple-whipper: iced GUI CD ripper built on libcdio-paranoia";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux"; # change if needed
      pkgs = nixpkgs.legacyPackages.${system};

      # winit/wgpu dlopen these at runtime, so they must be on LD_LIBRARY_PATH
      runtimeLibs = with pkgs; [
        wayland
        libxkbcommon
        vulkan-loader
        libGL
        libx11
        libxcursor
        libxi
        libxrandr
      ];

      # C libraries we link against through FFI
      cdLibs = with pkgs; [
        libcdio
        libcdio-paranoia
      ];
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          cargo
          rustc
          rustfmt
          clippy
          rust-analyzer
          pkg-config
          xdg-utils # xdg-open for the "Open folder" button
        ];
        # sets LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS (libc headers) for bindgen
        nativeBuildInputs = [ pkgs.rustPlatform.bindgenHook ];
        buildInputs = runtimeLibs ++ cdLibs ++ [ pkgs.fontconfig ];

        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs ++ cdLibs);
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    };
}
```
<!-- /include -->

### 14.3 `crates/cdio-paranoia-sys/Cargo.toml`

`links = "cdio_paranoia"` tells Cargo that this crate owns linking against the native library, so no other crate in the build graph may also declare it.

<!-- include: crates/cdio-paranoia-sys/Cargo.toml -->
```toml
[package]
name = "cdio-paranoia-sys"
version = "0.1.0"
edition = "2024"
description = "Raw FFI bindings to libcdio-paranoia (generated with bindgen)"
license = "MIT"
links = "cdio_paranoia"

[build-dependencies]
bindgen = "0.73"
pkg-config = "0.3"
```
<!-- /include -->

### 14.4 `crates/cdio-paranoia-sys/wrapper.h`

<!-- include: crates/cdio-paranoia-sys/wrapper.h -->
```c
/* Everything bindgen needs to see. cdda.h pulls in <cdio/cdio.h> itself,
 * and paranoia.h depends on the types declared in cdda.h, so order matters. */
#include <cdio/cdio.h>
#include <cdio/paranoia/cdda.h>
#include <cdio/paranoia/paranoia.h>
```
<!-- /include -->

### 14.5 `crates/cdio-paranoia-sys/build.rs`

<!-- include: crates/cdio-paranoia-sys/build.rs -->
```rust
//! Locates libcdio / libcdio-paranoia with pkg-config, emits the linker
//! flags, and runs bindgen over `wrapper.h`.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");

    // Each probe prints `cargo:rustc-link-lib=...` / `-search=...` for us.
    let mut include_paths = Vec::new();
    for lib in ["libcdio", "libcdio_cdda", "libcdio_paranoia"] {
        let found = pkg_config::Config::new()
            .probe(lib)
            .unwrap_or_else(|e| panic!("pkg-config could not find {lib}: {e}"));
        include_paths.extend(found.include_paths);
    }

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        // Only the pieces we actually use; keeps the bindings small.
        .allowlist_function("cdio_cddap_.*")
        .allowlist_function("cdio_paranoia_.*")
        .allowlist_function("cdio_get_devices")
        .allowlist_function("cdio_free_device_list")
        .allowlist_type("cdrom_drive_t")
        .allowlist_type("cdrom_paranoia_t")
        .allowlist_type("paranoia_mode_t")
        .allowlist_type("paranoia_cb_mode_t")
        .allowlist_type("driver_id_t")
        .allowlist_var("CDIO_CD_FRAMESIZE_RAW")
        .allowlist_var("CDDA_MESSAGE_.*")
        // C enums become Rust modules of consts: no UB on unknown values.
        .constified_enum_module("paranoia_mode_t")
        .constified_enum_module("paranoia_cb_mode_t")
        .constified_enum_module("driver_id_t")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    for path in &include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }

    let bindings = builder
        .generate()
        .expect("bindgen failed to generate libcdio-paranoia bindings");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs");
    bindings
        .write_to_file(out)
        .expect("could not write bindings.rs");
}
```
<!-- /include -->

### 14.6 `crates/cdio-paranoia-sys/src/lib.rs`

<!-- include: crates/cdio-paranoia-sys/src/lib.rs -->
```rust
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
```
<!-- /include -->

### 14.7 `crates/paranoia/Cargo.toml`

<!-- include: crates/paranoia/Cargo.toml -->
```toml
[package]
name = "paranoia"
version = "0.1.0"
edition = "2024"
description = "Safe Rust wrapper around libcdio-paranoia"
license = "MIT"

[dependencies]
cdio-paranoia-sys = { path = "../cdio-paranoia-sys" }
```
<!-- /include -->

### 14.8 `crates/paranoia/src/lib.rs` — the safe API

Every `unsafe` block has a `SAFETY:` comment stating what it relies on. See §5 for how the callback trampoline works.

<!-- include: crates/paranoia/src/lib.rs -->
```rust
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
```
<!-- /include -->

### 14.9 `src/main.rs` — entry point

<!-- include: src/main.rs -->
```rust
//! simple-whipper — a small CD ripper for the Linux desktop.
//!
//! libcdio-paranoia does the reading (through the `paranoia` crate), iced
//! draws the window.

mod app;
mod config;
mod encode;
mod rip;

fn main() -> iced::Result {
    iced::application(app::App::new, app::App::update, app::App::view)
        .title("Simple Whipper")
        .theme(app::App::theme)
        .window_size((760.0, 680.0))
        .run()
}
```
<!-- /include -->

### 14.10 `src/config.rs` — saved settings

<!-- include: src/config.rs -->
```rust
//! Settings that survive restarts: `~/.config/simple-whipper/config.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::encode::Format;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Last selected drive, e.g. `/dev/sr0`.
    pub device: Option<String>,
    /// Where ripped files go.
    pub output_dir: PathBuf,
    /// Drive read offset in samples (stereo frames). Look yours up in the
    /// AccurateRip drive offset list; e.g. many drives are +6, +667, +48.
    pub read_offset: i32,
    pub format: Format,
}

impl Default for Config {
    fn default() -> Self {
        let base = dirs::audio_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            device: None,
            output_dir: base.join("CD Rips"),
            read_offset: 0,
            format: Format::default(),
        }
    }
}

impl Config {
    fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("simple-whipper").join("config.toml"))
    }

    /// Load the config, falling back to defaults on any problem.
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Best-effort save; returns an error message for the status line.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no config directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let s = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, s).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial_files() {
        let c = Config {
            device: Some("/dev/sr0".into()),
            output_dir: "/music".into(),
            read_offset: 6,
            format: Format::Wav,
        };
        let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.read_offset, 6);
        assert_eq!(back.format, Format::Wav);
        // Missing keys fall back to defaults.
        let partial: Config = toml::from_str("read_offset = -12").unwrap();
        assert_eq!(partial.read_offset, -12);
        assert_eq!(partial.format, Format::Flac);
    }
}
```
<!-- /include -->

### 14.11 `src/encode.rs` — WAV and streaming FLAC

<!-- include: src/encode.rs -->
```rust
//! Output encoders. Both take interleaved 16-bit stereo PCM at 44.1 kHz,
//! one sector (or less) at a time, so a track never has to sit in memory.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream};
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};

pub const SAMPLE_RATE: u32 = 44_100;
pub const CHANNELS: u16 = 2;
pub const BITS: u16 = 16;

/// Output container the user picked in the GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Format {
    Wav,
    #[default]
    Flac,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::Flac, Format::Wav];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Flac => "flac",
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Format::Wav => "WAV",
            Format::Flac => "FLAC",
        })
    }
}

/// Something that consumes PCM and produces a file.
pub trait Sink {
    /// Append interleaved L/R samples.
    fn write(&mut self, samples: &[i16]) -> Result<(), String>;
    /// Flush, fix up headers, close.
    fn finish(self: Box<Self>) -> Result<(), String>;
}

/// `NN - Track NN.ext` inside `dir`.
pub fn track_path(dir: &Path, track: u8, format: Format) -> PathBuf {
    dir.join(format!(
        "{track:02} - Track {track:02}.{}",
        format.extension()
    ))
}

/// Create the right encoder for `format` writing to `path`.
pub fn create(path: &Path, format: Format) -> Result<Box<dyn Sink>, String> {
    Ok(match format {
        Format::Wav => Box::new(WavSink::create(path)?),
        Format::Flac => Box::new(FlacSink::create(path)?),
    })
}

// ---------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------

pub struct WavSink {
    writer: hound::WavWriter<BufWriter<File>>,
}

impl WavSink {
    pub fn create(path: &Path) -> Result<Self, String> {
        let spec = hound::WavSpec {
            channels: CHANNELS,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: BITS,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
        Ok(Self { writer })
    }
}

impl Sink for WavSink {
    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        let mut w = self.writer.get_i16_writer(samples.len() as u32);
        for &s in samples {
            w.write_sample(s);
        }
        w.flush().map_err(|e| e.to_string())
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        self.writer.finalize().map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// FLAC (streaming, with flacenc)
// ---------------------------------------------------------------------------

/// Samples per channel in one FLAC frame (the reference encoder's default).
const BLOCK_SIZE: usize = 4096;
/// "fLaC" + metadata block header (4) + STREAMINFO (34).
const HEADER_BYTES: usize = 4 + 4 + 34;

/// Encodes one FLAC frame per `BLOCK_SIZE` samples and writes it straight to
/// disk. A placeholder header is written first and rewritten on `finish`
/// once the MD5, total sample count and frame sizes are known.
pub struct FlacSink {
    file: BufWriter<File>,
    config: flacenc::error::Verified<flacenc::config::Encoder>,
    stream: Stream,
    framebuf: FrameBuf,
    context: Context,
    /// Interleaved samples waiting to fill the next block.
    pending: Vec<i32>,
    sink: ByteSink,
}

impl FlacSink {
    pub fn create(path: &Path) -> Result<Self, String> {
        let mut file = BufWriter::new(File::create(path).map_err(|e| e.to_string())?);
        file.write_all(&[0u8; HEADER_BYTES])
            .map_err(|e| e.to_string())?;
        let config = flacenc::config::Encoder::default()
            .into_verified()
            .map_err(|(_, e)| format!("{e:?}"))?;
        Ok(Self {
            file,
            config,
            stream: Stream::new(SAMPLE_RATE as usize, CHANNELS as usize, BITS as usize)
                .map_err(|e| format!("{e:?}"))?,
            framebuf: FrameBuf::with_size(CHANNELS as usize, BLOCK_SIZE)
                .map_err(|e| format!("{e:?}"))?,
            context: Context::new(BITS as usize, CHANNELS as usize),
            pending: Vec::with_capacity(BLOCK_SIZE * CHANNELS as usize),
            sink: ByteSink::new(),
        })
    }

    /// Encode `pending` (≤ one block) as the next frame and write it out.
    fn flush_block(&mut self) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut target = (&mut self.framebuf, &mut self.context);
        target
            .fill_interleaved(&self.pending)
            .map_err(|e| format!("{e:?}"))?;
        self.pending.clear();

        let frame_number = self.context.current_frame_number().unwrap_or(0);
        let frame = flacenc::encode_fixed_size_frame(
            &self.config,
            &self.framebuf,
            frame_number,
            self.stream.stream_info(),
        )
        .map_err(|e| format!("{e:?}"))?;
        self.stream.stream_info_mut().update_frame_info(&frame);

        self.sink.clear();
        frame.write(&mut self.sink).map_err(|e| format!("{e:?}"))?;
        self.file
            .write_all(self.sink.as_slice())
            .map_err(|e| e.to_string())
    }
}

impl Sink for FlacSink {
    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        let block = BLOCK_SIZE * CHANNELS as usize;
        for &s in samples {
            self.pending.push(i32::from(s));
            if self.pending.len() == block {
                self.flush_block()?;
            }
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), String> {
        self.flush_block()?; // last, possibly short, block

        let info = self.stream.stream_info_mut();
        info.set_md5_digest(&self.context.md5_digest());
        info.set_total_samples(self.context.total_samples());
        // Fixed-blocksize stream: the short last block does not count as min.
        info.set_block_sizes(BLOCK_SIZE, BLOCK_SIZE)
            .map_err(|e| format!("{e:?}"))?;

        // A frame-less Stream serialises to exactly "fLaC" + STREAMINFO.
        self.sink.clear();
        self.stream
            .write(&mut self.sink)
            .map_err(|e| format!("{e:?}"))?;
        let header = self.sink.as_slice();
        debug_assert_eq!(header.len(), HEADER_BYTES);

        self.file.flush().map_err(|e| e.to_string())?;
        let mut f = self.file.into_inner().map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        f.write_all(header).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize) -> Vec<i16> {
        (0..frames)
            .flat_map(|i| {
                let v = ((i as f32 * 0.05).sin() * 8000.0) as i16;
                [v, -v]
            })
            .collect()
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("simple-whipper-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn wav_roundtrip() {
        let pcm = tone(10_000);
        let path = tmp("t.wav");
        let mut s = create(&path, Format::Wav).unwrap();
        for chunk in pcm.chunks(1176) {
            s.write(chunk).unwrap();
        }
        s.finish().unwrap();
        let back: Vec<i16> = hound::WavReader::open(&path)
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(back, pcm);
    }

    #[test]
    fn flac_header_and_size() {
        // 3.5 blocks so the last frame is short.
        let pcm = tone(BLOCK_SIZE * 7 / 2);
        let path = tmp("t.flac");
        let mut s = create(&path, Format::Flac).unwrap();
        for chunk in pcm.chunks(1176) {
            s.write(chunk).unwrap();
        }
        s.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"fLaC");
        // STREAMINFO: last-block flag set, type 0, length 34
        assert_eq!(&bytes[4..8], &[0x80, 0, 0, 34]);
        // total samples (36 bits) end at byte 8+18; low 32 bits are bytes 22..26
        let total = u32::from_be_bytes(bytes[22..26].try_into().unwrap());
        assert_eq!(total as usize, BLOCK_SIZE * 7 / 2);
        assert!(bytes.len() > HEADER_BYTES);
    }

    #[test]
    fn names() {
        assert_eq!(
            track_path(Path::new("/x"), 3, Format::Flac),
            PathBuf::from("/x/03 - Track 03.flac")
        );
    }
}
```
<!-- /include -->

### 14.12 `src/rip.rs` — worker thread and offset math

<!-- include: src/rip.rs -->
```rust
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
```
<!-- /include -->

### 14.13 `src/app.rs` — the GUI

<!-- include: src/app.rs -->
```rust
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
```
<!-- /include -->
