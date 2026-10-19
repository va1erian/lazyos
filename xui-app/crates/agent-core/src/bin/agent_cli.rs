//! Headless LazyAgent: chat with a Fireworks model through the core, with no
//! UI (docs/ai-agent-plan.md P1). Proves the core runs on its own, and is the
//! P0 spike program: it prints the time to the first chunk and the
//! throughput of every reply.
//!
//! ```text
//! FIREWORKS_API_KEY=... \
//!     cargo run -p lazyagent-core --bin agent_cli --features native-roots
//! ```
//!
//! The key is read from the environment only (never an argument, so it is not
//! in a process list) and is never printed or stored. `--once "text"` sends
//! one message and exits; without it, each line of stdin is a message and
//! `/regen`, `/edit TEXT`, `/new`, `/model ID`, `/effort LEVEL` and `/quit`
//! are commands. `SSL_CERT_FILE` names a CA bundle, else the machine's store
//! is used (feature `native-roots`). `--dir DIR` keeps conversations in DIR
//! (default: a temp folder), `--resume ID` continues one.
//! `--test-endpoint HOST:PORT` talks to a harness server instead of Fireworks.
//! `--probe HOST:PORT` reads a long streamed reply from a harness server whose
//! records stall in the middle and prints a checksum (P0 item 5: the 250 ms
//! read timeout under LazyOS sockets, no key needed).

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use lazyagent_core::model::DEFAULT_MODEL;
use lazyagent_core::{
    ChatError, Completion, Effort, Endpoint, Fireworks, Persona, Platform, Roots, Secret, Session,
    Sink, Store, TurnError,
};

struct Terminal {
    dir: PathBuf,
}

impl Platform for Terminal {
    fn ca_roots(&self) -> Roots {
        match std::env::var_os("SSL_CERT_FILE").filter(|v| !v.is_empty()) {
            Some(path) => Roots::Bundle(PathBuf::from(path)),
            None => Roots::Native,
        }
    }

    fn app_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    fn log(&self, line: &str) {
        println!("{line}");
    }
}

/// Prints text as it streams; reasoning goes to stderr when asked for.
struct Printer {
    reasoning: bool,
}

impl Sink for Printer {
    fn text(&mut self, text: &str) {
        print!("{text}");
        io::stdout().flush().ok();
    }

    fn reasoning(&mut self, text: &str) {
        if self.reasoning {
            eprint!("\x1b[2m{text}\x1b[0m");
        }
    }

    fn retrying(&mut self, attempt: u32, delay: Duration, why: &str) {
        eprintln!("[retry {attempt} in {:.1}s: {why}]", delay.as_secs_f32());
    }

    fn compacted(&mut self, turns: usize) {
        eprintln!("[the oldest {turns} turns were summarised]");
    }
}

struct Options {
    once: Option<String>,
    model: String,
    effort: Option<Effort>,
    dir: PathBuf,
    resume: Option<String>,
    reasoning: bool,
    test_endpoint: Option<String>,
    probe: Option<String>,
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options {
        once: None,
        model: DEFAULT_MODEL.to_string(),
        effort: None,
        dir: std::env::temp_dir().join("lazyagent"),
        resume: None,
        reasoning: false,
        test_endpoint: None,
        probe: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--once" => options.once = Some(value("--once")?),
            "--model" => options.model = value("--model")?,
            "--effort" => {
                let v = value("--effort")?;
                options.effort =
                    Some(Effort::parse(&v).ok_or("effort is none, low, medium or high")?);
            }
            "--dir" => options.dir = PathBuf::from(value("--dir")?),
            "--resume" => options.resume = Some(value("--resume")?),
            "--test-endpoint" => options.test_endpoint = Some(value("--test-endpoint")?),
            "--probe" => options.probe = Some(value("--probe")?),
            "--show-reasoning" => options.reasoning = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

fn main() {
    let options = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("agent_cli: {e}");
            std::process::exit(2);
        }
    };
    if let Some(host_port) = &options.probe {
        lazyhttps_init();
        std::process::exit(probe(host_port));
    }
    let Some(key) = std::env::var("FIREWORKS_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    else {
        eprintln!("agent_cli: set FIREWORKS_API_KEY in the environment");
        std::process::exit(2);
    };
    lazyhttps_init();
    let platform = Terminal {
        dir: options.dir.clone(),
    };
    let endpoint = match &options.test_endpoint {
        Some(host_port) => Endpoint::for_testing(host_port),
        None => Endpoint::fireworks(),
    };
    let client = Fireworks::new(platform.ca_roots(), Secret::new(key), endpoint);
    let store = Store::new(&platform.app_dir());
    let conversation = match &options.resume {
        Some(id) => store.load(id),
        None => store.create(platform.now(), &options.model, Persona::Chat, false, None),
    };
    let conversation = match conversation {
        Ok(c) => c,
        Err(e) => {
            eprintln!("agent_cli: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "conversation {} in {}",
        conversation.id,
        options.dir.display()
    );
    let mut session = Session::new(
        conversation,
        &store,
        &client,
        "You are LazyAgent, a helpful assistant inside the LazyOS desktop. Be brief.",
    );
    session.params.reasoning_effort = options.effort;
    let stop = Arc::new(AtomicBool::new(false));
    let mut printer = Printer {
        reasoning: options.reasoning,
    };

    if let Some(text) = options.once {
        let ok = report(session.send(&text, &stop, &mut printer));
        session.name(&stop);
        std::process::exit(if ok { 0 } else { 1 });
    }
    for line in io::stdin().lock().lines().map_while(Result::ok) {
        let line = line.trim().to_string();
        let result = match line.as_str() {
            "" => continue,
            "/quit" => break,
            "/regen" => session.regenerate(&stop, &mut printer),
            "/new" => {
                match store.create(
                    platform.now(),
                    &session.conversation.model,
                    Persona::Chat,
                    false,
                    None,
                ) {
                    Ok(c) => {
                        eprintln!("conversation {}", c.id);
                        session.conversation = c;
                    }
                    Err(e) => eprintln!("agent_cli: {e}"),
                }
                continue;
            }
            l if l.starts_with("/model ") => {
                let model = l[7..].trim().to_string();
                let _ = session.record(lazyagent_core::Event::Model { model });
                continue;
            }
            l if l.starts_with("/effort ") => {
                session.params.reasoning_effort = Effort::parse(l[8..].trim());
                continue;
            }
            l if l.starts_with("/edit ") => session.edit_last(&l[6..], &stop, &mut printer),
            text => session.send(text, &stop, &mut printer),
        };
        report(result);
        if let Some(title) = session.name(&stop) {
            eprintln!("[titled: {title}]");
        }
    }
}

/// Fireworks' certificates come from the chosen roots; the crypto provider is
/// the GPL-compatible one, installed once before any thread starts.
fn lazyhttps_init() {
    lazyhttps::install_tls_provider();
}

fn report(result: Result<Completion, TurnError>) -> bool {
    match result {
        Ok(done) => {
            println!();
            eprintln!("{}", stats(&done));
            true
        }
        Err(TurnError::Chat(error)) => {
            println!();
            eprintln!("agent_cli: {error} [{}]", error.status());
            if let ChatError::Interrupted { partial, .. } | ChatError::Stopped { partial } = &error
            {
                eprintln!("{}", stats(partial));
            }
            false
        }
        Err(e) => {
            eprintln!("agent_cli: {e}");
            false
        }
    }
}

/// P0 item 5: streams `https://HOST:PORT/probe` with the same 250 ms wake-ups
/// the Fireworks client uses and checksums the body (FNV-1a), so a byte lost or
/// repeated after a read timed out in the middle of a TLS record shows.
fn probe(host_port: &str) -> i32 {
    use lazyhttps::{Http, HttpOptions, Policy, Watch};
    use std::io::Read;
    use std::sync::atomic::Ordering;
    let roots = Terminal {
        dir: PathBuf::new(),
    }
    .ca_roots();
    let options = HttpOptions {
        policy: Policy::https_hosts(&[host_port]),
        ..HttpOptions::default()
    };
    let http = Http::new(roots, options);
    let mut watch = Watch::new(Arc::new(AtomicBool::new(false)));
    watch.idle = Duration::from_secs(20);
    let wakes = watch.wakes.clone();
    let started = std::time::Instant::now();
    let mut response =
        match http.post_stream(&format!("https://{host_port}/probe"), &[], b"probe", watch) {
            Ok(r) => r,
            Err(e) => {
                println!("probe failed: {e}");
                return 1;
            }
        };
    let (mut bytes, mut hash) = (0u64, 0xcbf2_9ce4_8422_2325u64);
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        match response.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                bytes += n as u64;
                for &b in &buffer[..n] {
                    hash = (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            Err(e) => {
                println!("probe failed after {bytes} bytes: {e}");
                return 1;
            }
        }
    }
    println!(
        "probe done status={} bytes={bytes} fnv={hash:016x} wakes={} ms={}",
        response.status,
        wakes.load(Ordering::Relaxed),
        started.elapsed().as_millis()
    );
    0
}

/// The P0 numbers: time to the first chunk, throughput, tokens.
fn stats(done: &Completion) -> String {
    let usage = done.usage.unwrap_or_default();
    let streaming = done
        .total_ms
        .saturating_sub(done.first_chunk_ms.unwrap_or(0))
        .max(1);
    format!(
        "first chunk {} ms (server {} ms), total {} ms, {} in / {} out ({} reasoning), {:.0} tokens/s, finish {}",
        done.first_chunk_ms.map_or("-".into(), |v| v.to_string()),
        done.server_ttft_ms.map_or("-".into(), |v| v.to_string()),
        done.total_ms,
        usage.prompt,
        usage.completion,
        usage.reasoning,
        usage.completion as f64 * 1000.0 / streaming as f64,
        done.finish_reason.as_deref().unwrap_or("-"),
    )
}
