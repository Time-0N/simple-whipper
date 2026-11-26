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
