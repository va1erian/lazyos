//! `xui-nettools`: Net Tools, the network demo app.
//!
//! * **Ping** a host (an address or a name) four times through the stack's
//!   `Ping` (`os.lazy.net.stack.v1`), one request per second;
//! * **Look up a name** through the stack's resolver (`Resolve`);
//! * **Fetch a web page** over plain `std::net` (HTTP/1.0) on a worker thread
//!   and show the status line, the size and the first lines of the body;
//! * a **web server** on port 8080, started with the app, so the host can open
//!   `http://localhost:8080` when QEMU forwards the port (`run_demo.py --net`
//!   does; docs/networking-host-access.md). Each visit is logged.
//!
//! The headline shows the interface and address, refreshed every second. Calls
//! to `netd` run on the UI thread and are bounded; sockets live on threads
//! (`xui_app::net::web`).
//!
//! Serial evidence: `NETTOOLS:UP:PASS` after the first frame;
//! `NETTOOLS:PING:PASS host=<ip> rtt=<ms>` / `NETTOOLS:PING:FAIL`;
//! `NETTOOLS:LOOKUP:PASS name=<name> addrs=<n>` / `FAIL`;
//! `NETTOOLS:FETCH:PASS code=<n> bytes=<n>` / `FAIL`;
//! `NETTOOLS:SERVER:LISTENING port=8080` and `NETTOOLS:SERVER:HIT n=<n>
//! from=<peer>` (from the server thread); `NETTOOLS:CLOSE:PASS`.

use std::net::Ipv4Addr;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::format;
use xui_app::net::http::{self, PageInfo};
use xui_app::net::model::{self, NetStatus};
use xui_app::net::stack;
use xui_app::net::web::{Fetch, Server};
use xui_app::sys;
use xui_app::themed::run_themed;
use xui_core::app::{App, Ui};
use xui_core::backend::PlatformSpec;
use xui_core::{Dip, HasText};

#[path = "nettools/widgets.rs"]
mod widgets;

use widgets::{Widgets, WINDOW};

/// The timer period; the status and the pings run every [`SLOW_EVERY`] ticks.
const TICK_MILLIS: u32 = 250;
/// Ticks per second.
const SLOW_EVERY: u64 = 4;
/// Pings per run.
const PING_COUNT: u32 = 4;
/// Echo payload, as the usual `ping`.
const PING_PAYLOAD: u32 = 56;
/// The wait for each echo reply.
const PING_TIMEOUT_MS: u32 = 900;
/// The wait for a name lookup.
const LOOKUP_TIMEOUT_MS: u32 = 4000;
/// The web server's port (QEMU forwards host 8080 to it by default).
const SERVER_PORT: u16 = 8080;

pub enum Msg {
    Tick,
    Ping,
    StopPing,
    Lookup,
    Fetch,
    ToggleServer,
    Close,
}

/// A ping run in progress.
struct PingRun {
    target: [u8; 4],
    sent: u32,
    answered: u32,
    total_rtt: u32,
}

struct NetTools {
    w: Widgets,
    ticks: u64,
    status: Option<NetStatus>,
    ping: Option<PingRun>,
    fetch: Option<Fetch>,
    server: Option<Server>,
    /// A server asked to stop whose worker may still hold the port (it can be
    /// mid-request for up to the request timeout); a Start waits for it.
    stopping: Option<Server>,
    /// Start was clicked while `stopping` was still running.
    start_pending: bool,
    /// The server's hit and log-line counts last shown, so the log redraws
    /// only on change.
    shown_hits: Option<(u64, u64)>,
}

impl App for NetTools {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => self.tick(),
            Msg::Ping => self.start_ping(),
            Msg::StopPing => self.finish_ping("stopped"),
            Msg::Lookup => self.lookup(),
            Msg::Fetch => self.start_fetch(),
            Msg::ToggleServer => self.toggle_server(),
            Msg::Close => {
                println!("NETTOOLS:CLOSE:PASS");
                ui.quit();
            }
        }
    }
}

/// An address typed as a dotted quad, or resolved through the stack.
fn resolve(text: &str) -> Result<[u8; 4], String> {
    let text = text.trim();
    if let Some(addr) = netstack::config::parse_ipv4(text) {
        return Ok(addr);
    }
    match stack::resolve(text, LOOKUP_TIMEOUT_MS) {
        Ok(addrs) => addrs
            .first()
            .copied()
            .ok_or_else(|| format!("{text}: no address")),
        Err(error) => Err(format!("{text}: {}", error.describe())),
    }
}

impl NetTools {
    fn new(w: Widgets) -> NetTools {
        let mut app = NetTools {
            w,
            ticks: 0,
            status: None,
            ping: None,
            fetch: None,
            server: None,
            stopping: None,
            start_pending: false,
            shown_hits: None,
        };
        app.refresh_status();
        app.toggle_server();
        app
    }

    fn tick(&mut self) {
        self.ticks += 1;
        if self.ticks % SLOW_EVERY == 0 {
            self.refresh_status();
            self.next_ping();
        }
        self.poll_fetch();
        self.finish_stopping();
        self.show_server();
    }

    /// Forget the stopped server once its worker has ended (and released the
    /// port), then start the replacement a click asked for meanwhile.
    fn finish_stopping(&mut self) {
        if !self
            .stopping
            .as_ref()
            .is_some_and(|old| old.state().stopped)
        {
            return;
        }
        self.stopping = None;
        if std::mem::take(&mut self.start_pending) {
            self.start_server();
        }
    }

    fn refresh_status(&mut self) {
        match stack::status() {
            Ok(status) => {
                self.w.headline.set_text(&status.headline());
                self.status = Some(status);
            }
            Err(error) => {
                self.w
                    .headline
                    .set_text(&format!("Network: {}", error.describe()));
                self.status = None;
            }
        }
        if let Some(server) = &self.server {
            server.set_page(PageInfo {
                network: self.w.headline.text(),
                uptime: format::uptime(sys::clock_ticks()),
            });
        }
    }

    fn start_ping(&mut self) {
        match resolve(&self.w.host.text()) {
            Ok(target) => {
                self.ping = Some(PingRun {
                    target,
                    sent: 0,
                    answered: 0,
                    total_rtt: 0,
                });
                self.w
                    .ping_result
                    .set_text(&format!("Pinging {}...", model::dotted(target)));
                self.next_ping();
            }
            Err(why) => {
                println!("NETTOOLS:PING:FAIL");
                self.w.ping_result.set_text(&why);
            }
        }
    }

    /// Send the next echo request of the run, if one is in progress.
    fn next_ping(&mut self) {
        let Some(run) = self.ping.as_mut() else {
            return;
        };
        if run.sent == PING_COUNT {
            self.finish_ping("done");
            return;
        }
        run.sent += 1;
        let text = match stack::ping(run.target, PING_PAYLOAD, PING_TIMEOUT_MS) {
            Ok(echo) => {
                run.answered += 1;
                run.total_rtt += echo.rtt_ms;
                println!(
                    "NETTOOLS:PING:PASS host={} rtt={}",
                    model::dotted(echo.from),
                    echo.rtt_ms
                );
                format!(
                    "Reply from {}: {} bytes, {} ms ({}/{})",
                    model::dotted(echo.from),
                    echo.bytes,
                    echo.rtt_ms,
                    run.sent,
                    PING_COUNT
                )
            }
            Err(error) => {
                println!("NETTOOLS:PING:FAIL");
                format!(
                    "No reply: {} ({}/{})",
                    error.describe(),
                    run.sent,
                    PING_COUNT
                )
            }
        };
        self.w.ping_result.set_text(&text);
    }

    fn finish_ping(&mut self, how: &str) {
        let Some(run) = self.ping.take() else { return };
        let average = if run.answered > 0 {
            format!(", average {} ms", run.total_rtt / run.answered)
        } else {
            String::new()
        };
        self.w.ping_result.set_text(&format!(
            "{}: {} sent, {} answered{average} ({how})",
            model::dotted(run.target),
            run.sent,
            run.answered
        ));
    }

    fn lookup(&mut self) {
        let name = self.w.name.text().trim().to_string();
        let text = match stack::resolve(&name, LOOKUP_TIMEOUT_MS) {
            Ok(addrs) if !addrs.is_empty() => {
                println!(
                    "NETTOOLS:LOOKUP:PASS name={} addrs={}",
                    format::clip(&name, 64),
                    addrs.len()
                );
                let list: Vec<String> = addrs.into_iter().map(model::dotted).collect();
                list.join(", ")
            }
            Ok(_) => String::from("no address"),
            Err(error) => {
                println!("NETTOOLS:LOOKUP:FAIL");
                error.describe()
            }
        };
        self.w.lookup_result.set_text(&format::clip(&text, 60));
    }

    fn start_fetch(&mut self) {
        if self.fetch.is_some() {
            return;
        }
        let started = http::parse_url(&self.w.url.text()).and_then(|url| {
            let addr = resolve(&url.host)?;
            let shown = format!(
                "Fetching {}:{}{} ...",
                model::dotted(addr),
                url.port,
                url.path
            );
            Fetch::start(Ipv4Addr::from(addr), url).map(|fetch| (fetch, shown))
        });
        match started {
            Ok((fetch, shown)) => {
                self.w.fetch_result.set_text(&format::clip(&shown, 90));
                self.w.preview.set_items(&[]);
                self.fetch = Some(fetch);
            }
            Err(why) => {
                println!("NETTOOLS:FETCH:FAIL");
                self.w.fetch_result.set_text(&why);
            }
        }
    }

    fn poll_fetch(&mut self) {
        let Some(outcome) = self.fetch.as_ref().and_then(Fetch::take) else {
            return;
        };
        self.fetch = None;
        match outcome {
            Ok(done) => {
                let s = &done.summary;
                println!(
                    "NETTOOLS:FETCH:PASS code={} bytes={}",
                    s.code.unwrap_or(0),
                    done.bytes
                );
                self.w.fetch_result.set_text(&format!(
                    "{} · {} body bytes, {} headers · {} ms",
                    s.status, s.body_bytes, s.headers, done.millis
                ));
                let rows: Vec<&str> = s.preview.iter().map(String::as_str).collect();
                self.w.preview.set_items(&rows);
            }
            Err(why) => {
                println!("NETTOOLS:FETCH:FAIL");
                self.w.fetch_result.set_text(&format::clip(&why, 90));
            }
        }
    }

    fn toggle_server(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
            self.stopping = Some(server);
            self.w.server_status.set_text("Stopped.");
            self.w.server_button.set_text("Start");
            self.shown_hits = None;
            return;
        }
        if self.stopping.is_some() {
            // The old worker still holds the port: start once it is gone.
            self.start_pending = !self.start_pending;
            let (status, button) = if self.start_pending {
                ("Waiting for the previous server to stop...", "Cancel")
            } else {
                ("Stopped.", "Start")
            };
            self.w.server_status.set_text(status);
            self.w.server_button.set_text(button);
            return;
        }
        self.start_server();
    }

    fn start_server(&mut self) {
        match Server::start(SERVER_PORT) {
            Ok(server) => {
                self.server = Some(server);
                self.w.server_button.set_text("Stop");
                self.refresh_status();
            }
            Err(why) => self.w.server_status.set_text(&why),
        }
    }

    /// Show the server's state when it changed.
    fn show_server(&mut self) {
        let Some(server) = &self.server else { return };
        let state = server.state();
        let text = match &state.listening {
            None => String::from("Starting..."),
            Some(Err(why)) => why.clone(),
            Some(Ok(port)) => {
                let plural = if state.hits == 1 { "" } else { "s" };
                format!(
                    "Port {port}, {} request{plural}. From the host: http://localhost:{port}",
                    state.hits
                )
            }
        };
        if self.w.server_status.text() != text {
            self.w.server_status.set_text(&text);
        }
        let shown = (state.hits, state.logged);
        if self.shown_hits != Some(shown) {
            let rows: Vec<&str> = state.log.iter().map(String::as_str).collect();
            self.w.server_log.set_items(&rows);
            self.shown_hits = Some(shown);
        }
    }
}

fn main() -> std::process::ExitCode {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("NETTOOLS:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("NETTOOLS:UP:PASS"));
    let spec = PlatformSpec::new("Net Tools").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_themed(&backend, spec, |ui| {
        let widgets = match Widgets::build(ui) {
            Ok(widgets) => widgets,
            Err(error) => {
                println!("NETTOOLS:BUILD:FAIL:{error}");
                std::process::exit(1);
            }
        };
        let app = NetTools::new(widgets);
        ui.on_timer(|_| Some(Msg::Tick));
        ui.set_timer(TICK_MILLIS);
        ui.on_close(|| Some(Msg::Close));
        app
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("NETTOOLS:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
