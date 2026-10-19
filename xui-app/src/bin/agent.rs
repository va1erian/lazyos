//! `xui-agent`: LazyAgent, an AI chat and diagnostics agent
//! (docs/ai-agent-plan.md, docs/agent.md). The Fireworks client, the
//! conversation store and the agent live in `crates/agent-core` and the
//! window in `crates/agent-ui`, both portable (the `desktop` example of
//! `agent-ui` runs the same app on Windows); this file only supplies the
//! LazyOS [`Platform`] and launches the window.
//!
//! Serial evidence (through `xui_app::trace`, channel `AGENT`): `UP:PASS`
//! after the first frame, `READY`, `CONSENT`, `KEY:SET`, `CHAT:SEND:<turns>`,
//! `CHAT:FIRST:<ms>`, `CHAT:DONE:<in>:<out>:<ms>`, `CHAT:FAIL:<status>`,
//! `ROWS:<n>`, `CONV:<n>`, `FOLLOW:<0|1>`, `COPIED`, `LINK:ASKED|OPENED`,
//! `QUIT:PASS`. No line carries message text, a link, a key or a token. From
//! `launch::run`: `AGENT:BIND:FAIL:<code>` and `AGENT:RUN:FAIL:<error>`.

use std::path::PathBuf;
use std::sync::Arc;

use lazyagent_core::{Platform, Roots};
use lazyagent_ui::{LazyAgentApp, Msg, Report, WINDOW};
use xui_app::launch;
use xui_app::platform::{dirs, launcher};
use xui_app::trace::{self, Channel};

/// The app's id: its data folder is `$HOME/.apps/<SYSTEM_NAME>`.
const SYSTEM_NAME: &str = "os.lazy.agent";

/// LazyOS's answers to the core's questions: the CA bundle from `fhs`, the
/// app's folder under `$HOME/.apps`, links through `mimed`.
struct LazyOs;

impl Platform for LazyOs {
    fn ca_roots(&self) -> Roots {
        let from_env = std::env::var_os("SSL_CERT_FILE").filter(|v| !v.is_empty());
        let path = from_env.map(PathBuf::from).unwrap_or_else(|| {
            [fhs::etc::CA_BUNDLE, fhs::etc::LINUX_CA_BUNDLE]
                .iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
                .unwrap_or_else(|| PathBuf::from(fhs::etc::CA_BUNDLE))
        });
        Roots::Bundle(path)
    }

    fn app_dir(&self) -> PathBuf {
        let home = dirs::default_dir();
        PathBuf::from(fhs::app_data_dir(&home.to_string_lossy(), SYSTEM_NAME))
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        launcher::open_url(url).map_err(|e| e.to_string())
    }

    fn log(&self, line: &str) {
        // One marker line, whatever the text: no newline can start another.
        let one: String = line
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(200)
            .collect();
        trace::line!(Channel::Agent, "LOG:{one}");
    }
}

fn say(report: &Report) {
    let channel = Channel::Agent;
    match report {
        Report::Ready => trace::pass!(channel, "READY"),
        Report::Consented => trace::pass!(channel, "CONSENT"),
        Report::KeySet => trace::pass!(channel, "KEY", "SET"),
        Report::Sent(turns) => trace::line!(channel, "CHAT:SEND:{turns}"),
        Report::FirstChunk(ms) => trace::line!(channel, "CHAT:FIRST:{ms}"),
        Report::Done { prompt, completion, ms } => {
            trace::line!(channel, "CHAT:DONE:{prompt}:{completion}:{ms}");
        }
        Report::Failed(status) => trace::line!(channel, "CHAT:FAIL:{status}"),
        Report::Rows(n) => trace::line!(channel, "ROWS:{n}"),
        Report::Conversations(n) => trace::line!(channel, "CONV:{n}"),
        Report::Following(on) => trace::line!(channel, "FOLLOW:{}", u8::from(*on)),
        Report::Copied => trace::line!(channel, "COPIED"),
        // The address is model text: never in a marker.
        Report::LinkAsked(_) => trace::line!(channel, "LINK:ASKED"),
        Report::LinkOpened(_) => trace::line!(channel, "LINK:OPENED"),
    }
}

fn main() {
    // `panic = "abort"`: a panic on any thread ends the app, so say where on
    // the serial line first (the default hook writes to a stderr nobody reads).
    std::panic::set_hook(Box::new(|info| {
        let place = info.location().map_or(String::new(), |l| format!(" at {}:{}", l.file(), l.line()));
        trace::fail!(Channel::Agent, "PANIC", "{place}");
    }));
    // The TLS provider is `nettls-crypto` (GPLv2-compatible), installed
    // before any thread starts, as Mail and LazySky do.
    lazyagent_core::install_tls_provider();
    launch::run(Channel::Agent, "AI Agent", WINDOW, move |ui, backend| {
        backend.on_first_frame(|| trace::pass!(Channel::Agent, "UP"));
        backend.set_size_hints(560, 400, 0, 0);
        LazyAgentApp::build(ui, Arc::new(LazyOs), |msg| match msg {
            Msg::Report(report) => say(report),
            Msg::Quit => trace::pass!(Channel::Agent, "QUIT"),
            _ => {}
        })
        .inspect_err(|error| trace::fail!(Channel::Agent, "BUILD", "{error}"))
    })
}
