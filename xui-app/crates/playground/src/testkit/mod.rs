//! Test support: a fabric in memory, an engine over it, services for the
//! gallery and a small seeded random source. Compiled for tests only.

pub mod broker;
pub mod bus;
pub mod services;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rhai::Engine;
use rhai_lazy::{build_engine, eval_source, Config, DirEntry, Host, HostError, Outcome, Scope};

pub use bus::FakeBus;

/// A process with a fabric, captured output and no files.
pub struct TestHost {
    bus: Rc<FakeBus>,
    pub stdout: RefCell<String>,
    pub stderr: RefCell<String>,
    pub slept: RefCell<Vec<u64>>,
    clock: Cell<f64>,
}

impl Host for TestHost {
    fn args(&self) -> Vec<String> {
        Vec::new()
    }

    fn env_var(&self, _key: &str) -> Option<String> {
        None
    }

    fn env_vars(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn now_secs(&self) -> f64 {
        self.clock.get()
    }

    fn sleep_ms(&self, ms: u64) {
        self.slept.borrow_mut().push(ms);
        self.clock.set(self.clock.get() + ms as f64 / 1000.0);
    }

    fn read_file(&self, _path: &str, _max: usize) -> Result<Vec<u8>, HostError> {
        Err(HostError::new("No such file or directory (os error 2)"))
    }

    fn write_file(&self, _path: &str, _data: &[u8]) -> Result<(), HostError> {
        Err(HostError::new("Permission denied (os error 13)"))
    }

    fn list_dir(&self, _path: &str, _max: usize) -> Result<Vec<DirEntry>, HostError> {
        Err(HostError::new("No such file or directory (os error 2)"))
    }

    fn read_stdin(&self, _max: usize) -> Result<Vec<u8>, HostError> {
        Ok(Vec::new())
    }

    fn write_out(&self, text: &str) -> Result<(), HostError> {
        self.stdout.borrow_mut().push_str(text);
        Ok(())
    }

    fn write_err(&self, text: &str) {
        self.stderr.borrow_mut().push_str(text);
    }

    fn bus(&self) -> Option<Rc<dyn rhai_lazy::msg::Bus>> {
        let bus: Rc<dyn rhai_lazy::msg::Bus> = self.bus.clone();
        Some(bus)
    }
}

/// What a script printed and how it ended.
pub struct Ran {
    pub outcome: Outcome,
    pub lines: Vec<String>,
}

impl Ran {
    /// The failure text, panicking if the script did not fail.
    pub fn failure(&self) -> &str {
        match &self.outcome {
            Outcome::Failed(message) => message,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// Panic unless the script finished.
    pub fn ok(&self) -> &Self {
        match &self.outcome {
            Outcome::Value(_) | Outcome::Exit(0) => self,
            other => panic!(
                "script failed: {other:?}\noutput:\n{}",
                self.lines.join("\n")
            ),
        }
    }
}

/// An engine over a fabric, reusable across scripts (building one compiles
/// every generated `sys` module, which is the expensive part).
pub struct Lab {
    pub bus: Rc<FakeBus>,
    pub host: Rc<TestHost>,
    pub engine: Engine,
}

impl Lab {
    pub fn new() -> Self {
        Self::over(FakeBus::new())
    }

    pub fn over(bus: Rc<FakeBus>) -> Self {
        let host = Rc::new(TestHost {
            bus: bus.clone(),
            stdout: RefCell::default(),
            stderr: RefCell::default(),
            slept: RefCell::default(),
            clock: Cell::new(0.0),
        });
        let engine = build_engine(host.clone(), &Config::default());
        Lab { bus, host, engine }
    }

    /// Run `source` with a fresh scope; output is collected per call.
    pub fn run(&self, source: &str) -> Ran {
        self.host.stdout.borrow_mut().clear();
        let outcome = eval_source(&self.engine, &mut Scope::new(), source);
        let text = self.host.stdout.borrow().clone();
        Ran {
            outcome,
            lines: text.lines().map(str::to_string).collect(),
        }
    }

    /// Whether `source` compiles; the parse error otherwise.
    pub fn compile(&self, source: &str) -> Result<(), String> {
        self.engine
            .compile(rhai_lazy::outcome::strip_shebang(source))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// A small seeded generator (xorshift64*), so a failing fuzz case replays.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// The seed a fuzz test uses: `FUZZ_SEED` (decimal or `0x` hex) or a default.
pub fn seed(default: u64) -> u64 {
    std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|s| {
            s.strip_prefix("0x")
                .map_or_else(|| s.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
        })
        .unwrap_or(default)
}

/// How many cases a fuzz test runs: `FUZZ_CASES` or a default.
pub fn cases(default: u64) -> u64 {
    std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}
