//! Serial evidence for the session scripts and the sound judge: an
//! [`AudioBackend`] wrapper that prints one line per event.
//!
//! - `EMUSIC:PLAY:<file>` when a track opens, `EMUSIC:OPEN:FAIL:<file>` when
//!   it cannot;
//! - `EMUSIC:POS:<seconds>` each time the played position reaches a new whole
//!   second (the player polls it every tick);
//! - `EMUSIC:SEEK:<milliseconds>`, `EMUSIC:PAUSE` and `EMUSIC:RESUME`;
//! - `EMUSIC:END:<file>` when a track has played to its end.

use std::any::Any;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use emusic_player::backend::{AudioBackend, BackendChannel, ChannelCapabilities};
use emusic_player::error::PlayerError;
use emusic_player::tracker::TrackerSettings;

/// Where the lines go: the serial console on LazyOS, a buffer in tests.
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// Prints evidence for everything `inner` does.
pub struct MarkingBackend {
    inner: Arc<dyn AudioBackend>,
    sink: Sink,
}

impl MarkingBackend {
    /// Wraps `inner`, printing to `sink`.
    pub fn new(inner: Arc<dyn AudioBackend>, sink: Sink) -> MarkingBackend {
        MarkingBackend { inner, sink }
    }
}

/// The name a marker gives `path`: its file name, spaces kept.
fn name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl AudioBackend for MarkingBackend {
    fn open(&self, path: &Path) -> Result<Box<dyn BackendChannel>, PlayerError> {
        let track = name(path);
        match self.inner.open(path) {
            Ok(inner) => {
                (self.sink)(&format!("EMUSIC:PLAY:{track}"));
                Ok(Box::new(MarkingChannel {
                    inner,
                    sink: Arc::clone(&self.sink),
                    track,
                    second: AtomicU64::new(u64::MAX),
                }))
            }
            Err(error) => {
                (self.sink)(&format!("EMUSIC:OPEN:FAIL:{track}"));
                Err(error)
            }
        }
    }

    fn set_tracker_resampling_quality(&self, quality: u8) -> Result<(), PlayerError> {
        self.inner.set_tracker_resampling_quality(quality)
    }
}

struct MarkingChannel {
    inner: Box<dyn BackendChannel>,
    sink: Sink,
    track: String,
    /// The last whole second reported.
    second: AtomicU64,
}

impl BackendChannel for MarkingChannel {
    fn play(&self, restart: bool) -> Result<(), PlayerError> {
        let resumed = self.inner.position().is_ok_and(|at| at > Duration::ZERO);
        self.inner.play(restart)?;
        if resumed && !restart {
            (self.sink)("EMUSIC:RESUME");
        }
        Ok(())
    }

    fn pause(&self) -> Result<(), PlayerError> {
        self.inner.pause()?;
        (self.sink)("EMUSIC:PAUSE");
        Ok(())
    }

    fn stop(&self) -> Result<(), PlayerError> {
        self.inner.stop()
    }

    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    fn position(&self) -> Result<Duration, PlayerError> {
        let position = self.inner.position()?;
        let second = position.as_secs();
        if self.second.swap(second, Ordering::Relaxed) != second {
            (self.sink)(&format!("EMUSIC:POS:{second}"));
        }
        Ok(position)
    }

    fn duration(&self) -> Result<Duration, PlayerError> {
        self.inner.duration()
    }

    fn seek(&self, position: Duration) -> Result<(), PlayerError> {
        self.inner.seek(position)?;
        (self.sink)(&format!("EMUSIC:SEEK:{}", position.as_millis()));
        Ok(())
    }

    fn set_volume(&self, gain: f32) -> Result<(), PlayerError> {
        self.inner.set_volume(gain)
    }

    fn apply_tracker_settings(&self, settings: &TrackerSettings) -> Result<(), PlayerError> {
        self.inner.apply_tracker_settings(settings)
    }

    fn on_end(&self, callback: Box<dyn Fn() + Send>) -> Result<Box<dyn Any + Send>, PlayerError> {
        let sink = Arc::clone(&self.sink);
        let line = format!("EMUSIC:END:{}", self.track);
        self.inner.on_end(Box::new(move || {
            sink(&line);
            callback();
        }))
    }

    fn fft(&self) -> Option<Vec<f32>> {
        self.inner.fft()
    }

    fn samples(&self) -> Option<Vec<f32>> {
        self.inner.samples()
    }

    fn capabilities(&self) -> ChannelCapabilities {
        self.inner.capabilities()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use emusic_lazyaudio::LazyBackend;
    use emusic_lazyaudio::output::MemorySink;

    use super::*;

    fn backend() -> (MarkingBackend, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&lines);
        let sink: Sink = Arc::new(move |line| log.lock().unwrap().push(line.to_string()));
        let inner = LazyBackend::new(Arc::new(MemorySink::new(8192, 2048, 20.0)));
        (MarkingBackend::new(Arc::new(inner), sink), lines)
    }

    #[test]
    fn a_file_that_does_not_open_is_reported() {
        let (backend, lines) = backend();
        assert!(backend.open(Path::new("/nowhere/song.mp3")).is_err());
        assert_eq!(*lines.lock().unwrap(), ["EMUSIC:OPEN:FAIL:song.mp3"]);
    }

    #[test]
    fn whole_seconds_are_reported_once() {
        let (backend, lines) = backend();
        let path = std::env::temp_dir().join("lazyemusic markers.mp3");
        std::fs::write(&path, include_bytes!("../package/resources/tones.mp3")).unwrap();
        let channel = backend.open(&path).expect("open the tones");
        let _guard = channel.on_end(Box::new(|| {})).unwrap();
        channel.play(false).unwrap();
        while channel.is_active() {
            channel.position().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        channel.position().unwrap();
        let lines = lines.lock().unwrap();
        assert_eq!(lines[0], "EMUSIC:PLAY:lazyemusic markers.mp3");
        let seconds: Vec<&String> = lines
            .iter()
            .filter(|l| l.starts_with("EMUSIC:POS:"))
            .collect();
        assert_eq!(
            seconds,
            [
                "EMUSIC:POS:0",
                "EMUSIC:POS:1",
                "EMUSIC:POS:2",
                "EMUSIC:POS:3",
                "EMUSIC:POS:4"
            ]
        );
        assert!(lines.contains(&"EMUSIC:END:lazyemusic markers.mp3".to_string()));
        let _ = std::fs::remove_file(path);
    }
}
