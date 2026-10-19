//! What the window needs from the LazyOS platform, so the app itself is
//! host-testable: the mixer, the directories, the sample ROM, and the one
//! place evidence lines come out.

use std::path::PathBuf;

/// Something the app wants on the console's evidence channel. The binary
/// turns each into one `NES:*` trace line; the tests collect them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// A cartridge loaded.
    Loaded {
        path: PathBuf,
        mapper: u16,
        prg_kb: usize,
        chr_kb: usize,
        battery: bool,
        region: &'static str,
    },
    /// A file was refused, or a frame failed.
    Failed(String),
    /// The console has run this many display frames.
    Running { frames: u64 },
    /// The battery file was written.
    Battery { path: PathBuf },
    /// A save state was written.
    StateSaved { slot: usize, path: PathBuf },
    /// A save state was restored.
    StateLoaded { slot: usize, path: PathBuf },
    /// Sound is playing (the mixer granted these parameters).
    Sound { rate: u32, channels: u32 },
    /// Sound is not playing, and why.
    Silent(String),
    /// The user asked to quit.
    Quit,
}

/// The platform behind the window.
pub trait Host {
    /// Queues mono `i16` samples at [`Host::audio_rate`]; the implementation
    /// takes what fits and drops the rest.
    fn write_audio(&mut self, samples: &[i16]);
    /// Room in the mixer's ring, in frames; `None` when there is no stream,
    /// in which case the console is paced by the clock alone.
    fn audio_free(&mut self) -> Option<usize>;
    /// The rate the stream runs at (the session is built with it).
    fn audio_rate(&self) -> u32;
    /// The channels the mixer granted.
    fn audio_channels(&self) -> u32;
    /// Mutes or unmutes the stream.
    fn set_muted(&mut self, muted: bool);
    /// Whether the stream is muted.
    fn muted(&self) -> bool;
    /// One evidence event.
    fn report(&self, report: Report);
    /// The directory the app keeps its files in.
    fn app_dir(&self) -> PathBuf;
    /// Where a file picker starts.
    fn default_dir(&self) -> PathBuf;
    /// The sample ROM the system ships, when it is installed.
    fn sample_rom(&self) -> Option<PathBuf>;
}
