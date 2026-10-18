//! `nes`: the NES Player (`os.lazy.nes`), an NES emulator with sound.
//!
//! The emulator session, the picture, the menus and the welcome screen live in
//! `crates/nes` (`xui-nes`); this file supplies the LazyOS platform: the mixer
//! through `libs/audioclient`, `$HOME`, the shipped sample cartridge, and the
//! `NES:*` evidence lines.
//!
//! `nes.elf --headless <rom> [--frames N]` runs the console with no window and
//! prints `NES:HEADLESS:PASS frames=N crc=<hex>` (the same deterministic check
//! Doom's `-frames` and Quake's `-headless` give the harness).
//!
//! Serial evidence: `NES:UP:PASS` after the first frame; `NES:LOAD:PASS:...`
//! when a cartridge loads; `NES:RUN:PASS:frames=N` once the console has run;
//! `NES:SAVE:PASS:<path>` (battery), `NES:STATE:PASS:<slot>:<path>`;
//! `NES:SOUND:PASS:<rate>:<channels>` / `NES:SOUND:FAIL:<why>`;
//! `NES:QUIT:PASS` on quit; `NES:FAIL:<what>` for anything refused.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_app::backend::DropEvent;
use xui_app::launch;
use xui_app::platform::audio::{Audio, Params, PlaybackStream};
use xui_app::platform::urilist;
use xui_app::trace::{self, Channel};
use xui_nes::host::{Host, Report};
use xui_nes::{Msg, NesApp, Session, WINDOW};

/// The mixer stream's period: 2048 bytes is 1024 mono frames at 48 kHz, so
/// the ring (four periods) holds about 85 ms.
const PERIOD_BYTES: u32 = 2048;
/// The rate the app asks the mixer for.
const RATE_HZ: u32 = 48_000;
/// Frames a `--headless` run clocks when no `--frames` is given.
const HEADLESS_FRAMES: u64 = 600;
/// Where a headless run leaves its result, for a harness that reads files.
const RESULT_FILE: &str = "nes-result.txt";

/// The LazyOS side of the app.
struct Platform {
    /// The mixer stream, when the system has one.
    stream: Option<Stream>,
    /// The app's directory (`$HOME/.apps/os.lazy.nes`).
    dir: PathBuf,
    muted: bool,
}

/// An open mixer stream, converted to whatever the mixer granted.
struct Stream {
    stream: PlaybackStream<Audio>,
    channels: u32,
    rate: u32,
    /// The mono-to-stereo scratch when the mixer granted two channels.
    scratch: Vec<i16>,
}

impl Platform {
    /// Connects the mixer, if there is one, and opens a mono stream at
    /// [`RATE_HZ`].
    fn connect(dir: PathBuf) -> Platform {
        let mut platform = Platform {
            stream: None,
            dir,
            muted: false,
        };
        let Some(audio) = Audio::try_connect() else {
            trace::fail!(Channel::Nes, "SOUND", "no mixer");
            return platform;
        };
        let params = Params::new(RATE_HZ, 1).period_bytes(PERIOD_BYTES);
        match PlaybackStream::open(audio, params) {
            Ok(stream) => {
                let rate = stream.rate();
                let channels = stream.channels();
                trace::pass!(Channel::Nes, "SOUND", "{rate}:{channels}");
                platform.stream = Some(Stream {
                    stream,
                    channels,
                    rate,
                    scratch: Vec::new(),
                });
            }
            Err(error) => trace::fail!(Channel::Nes, "SOUND", "{error}"),
        }
        platform
    }

    /// The rate the session's APU runs at: the mixer's grant, else the rate we
    /// asked for (silent).
    fn rate(&self) -> f32 {
        self.stream
            .as_ref()
            .map_or(RATE_HZ as f32, |stream| stream.rate as f32)
    }
}

impl Host for Platform {
    fn write_audio(&mut self, samples: &[i16]) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        if stream.channels >= 2 {
            stream.scratch.clear();
            for sample in samples {
                stream.scratch.push(*sample);
                stream.scratch.push(*sample);
            }
            let _ = stream.stream.try_write(&stream.scratch);
        } else {
            let _ = stream.stream.try_write(samples);
        }
    }

    fn audio_free(&mut self) -> Option<usize> {
        let stream = self.stream.as_mut()?;
        stream
            .stream
            .free_frames()
            .ok()
            .map(|frames| frames as usize)
    }

    fn audio_rate(&self) -> u32 {
        self.stream.as_ref().map_or(RATE_HZ, |stream| stream.rate)
    }

    fn audio_channels(&self) -> u32 {
        self.stream.as_ref().map_or(0, |stream| stream.channels)
    }

    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if let Some(stream) = &mut self.stream {
            let _ = stream.stream.set_mute(muted);
        }
    }

    fn muted(&self) -> bool {
        self.muted
    }

    fn report(&self, report: Report) {
        match report {
            Report::Loaded {
                path,
                mapper,
                prg_kb,
                chr_kb,
                battery,
                region,
            } => trace::pass!(
                Channel::Nes,
                "LOAD",
                "{}:mapper={mapper}:prg={prg_kb}K:chr={chr_kb}K:battery={}:region={region}",
                path.display(),
                u8::from(battery)
            ),
            Report::Failed(why) => trace::fail!(Channel::Nes, "FAIL", "{why}"),
            Report::Running { frames } => trace::pass!(Channel::Nes, "RUN", "frames={frames}"),
            Report::Battery { path } => {
                trace::pass!(Channel::Nes, "SAVE", "{}", path.display());
            }
            Report::StateSaved { slot, path } => {
                trace::pass!(Channel::Nes, "STATE", "save={slot}:{}", path.display());
            }
            Report::StateLoaded { slot, path } => {
                trace::pass!(Channel::Nes, "STATE", "load={slot}:{}", path.display());
            }
            Report::Sound { rate, channels } => {
                trace::pass!(Channel::Nes, "AUDIO", "{rate}:{channels}");
            }
            Report::Silent(why) => trace::fail!(Channel::Nes, "AUDIO", "{why}"),
            Report::Quit => trace::pass!(Channel::Nes, "QUIT"),
        }
    }

    fn app_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    fn default_dir(&self) -> PathBuf {
        xui_app::platform::dirs::default_dir()
    }

    fn sample_rom(&self) -> Option<PathBuf> {
        let path = PathBuf::from(fhs::share::NES_SAMPLE);
        path.is_file().then_some(path)
    }
}

/// `$HOME/.apps/os.lazy.nes`, or `/transient`'s when there is no home.
fn app_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute() && home.is_dir());
    let base = home.unwrap_or_else(|| PathBuf::from(fhs::mount::TRANSIENT));
    base.join(".apps").join("os.lazy.nes")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--headless") {
        std::process::exit(headless(&args));
    }
    let rom = xui_app::platform::argv::file_arg(std::env::args_os());
    let platform = Platform::connect(app_dir());
    let rate = platform.rate();
    launch::run(Channel::Nes, "NES", WINDOW, |ui, backend| {
        backend.on_first_frame(|| trace::pass!(Channel::Nes, "UP"));
        // The picture keeps its aspect; the window may be any size.
        backend.set_size_hints(320, 260, 0, 0);
        let host: Rc<RefCell<dyn Host>> = Rc::new(RefCell::new(platform));
        let session = Session::new(host.borrow().app_dir(), rate);
        let app = NesApp::build(ui, host, session, rom).inspect_err(|error| {
            trace::fail!(Channel::Nes, "BUILD", "{error}");
        })?;
        // A cartridge dropped on the window opens (a `text/uri-list` from the
        // Files app, `clipboardd`'s token; docs/xui-archiver.md).
        let outbox = ui.proxy();
        backend.on_drag_event(move |_, event| {
            if let DropEvent::Drop {
                mime,
                data: Ok(bytes),
                ..
            } = event
            {
                if mime == urilist::MIME {
                    if let Some(path) = urilist::decode(bytes).into_iter().next() {
                        let _ = outbox.send(Msg::RomChosen(path));
                    }
                }
            }
        });
        Ok(app)
    });
}

/// `--headless <rom> [--frames N]`: run the console with no window and print
/// the frame's CRC, the deterministic check the harness reads.
fn headless(args: &[String]) -> i32 {
    let mut rom: Option<PathBuf> = None;
    let mut frames = HEADLESS_FRAMES;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--headless" => {}
            "--frames" => {
                frames = rest
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(frames);
            }
            other => rom = Some(PathBuf::from(other)),
        }
    }
    let Some(rom) = rom else {
        trace::fail!(Channel::Nes, "HEADLESS", "no ROM named");
        return 1;
    };
    if !rom.is_absolute() || !rom.is_file() {
        trace::fail!(Channel::Nes, "HEADLESS", "no such ROM: {}", rom.display());
        return 1;
    }
    let mut session = Session::new(app_dir(), RATE_HZ as f32);
    let info = match session.load_rom(&rom) {
        Ok(info) => info,
        Err(error) => {
            trace::fail!(Channel::Nes, "HEADLESS", "{error}");
            return 1;
        }
    };
    trace::pass!(
        Channel::Nes,
        "LOAD",
        "{}:mapper={}:prg={}K:chr={}K:battery={}:region={}",
        info.path.display(),
        info.mapper,
        info.prg_kb,
        info.chr_kb,
        u8::from(info.battery),
        xui_nes::rom::region_name(info.region)
    );
    for _ in 0..frames {
        if let Err(error) = session.clock() {
            trace::fail!(Channel::Nes, "HEADLESS", "{error}");
            return 1;
        }
    }
    let crc = xui_nes::rom::crc32(session.frame());
    let line = format!("NES:HEADLESS:PASS frames={frames} crc={crc:08x}");
    trace::pass!(Channel::Nes, "HEADLESS", "frames={frames} crc={crc:08x}");
    let _ = std::fs::write(
        std::path::Path::new(fhs::mount::TMP).join(RESULT_FILE),
        format!("{line}\n"),
    );
    0
}
