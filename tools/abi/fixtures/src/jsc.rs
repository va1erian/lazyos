//! `jsc` — run JavaScriptCore's shells and the C++ proof that
//! `tools/jsc/build.py` builds (an image built with `LAZYOS_JSC=1` puts
//! `jsc`, `jsc-jit` and `cxxproof` in `/system/bin`; docs/webkit-port-plan.md
//! W1, W2).
//!
//! The markers are the evidence and go to the serial log, one per line:
//! everything `cxxproof` prints (`JSC:W1:*`), then for the C-loop shell
//! `JSC:UP:PASS` once it answers, `JSC:START:<ms>` (how long `jsc -e` took),
//! `JSC:TIME:<name>:<ms>` per script and `JSC:RUN:PASS:<name>` or
//! `JSC:RUN:FAIL:<name>:<reason>` for each: arithmetic, a script that
//! allocates enough to force collections with concurrent marker threads on,
//! one with classes, Promises, async generators and Intl (ICU), a hot numeric
//! one, and four SharedArrayBuffer agents (threads) under the collector. The
//! JIT shell runs the same with a `JSC:JIT:` prefix, and each shell then runs
//! the stress subset (`jsc_stress.rs`: `<prefix>STRESS:<pass>/<total>`). The
//! fixture ends with `ABI:jsc:PASS` when every check passed;
//! `tools/jsc/run.py` judges the log (`tools/jsc/judge.py`).

mod common;
mod jsc_scripts;
mod jsc_stress;

use jsc_scripts::*;
use std::fs;
use std::process::{Command, Stdio};
use std::time::Instant;

const BIN: &str = "/system/bin";
const SCRATCH: &str = "/tmp/jsc";

/// A shell under test and the prefix its markers carry.
struct Shell {
    program: &'static str,
    prefix: &'static str,
    /// Whether the DFG is expected to compile the hot function.
    jit: bool,
}

const SHELLS: [Shell; 2] = [
    Shell {
        program: "jsc",
        prefix: "JSC:",
        jit: false,
    },
    Shell {
        program: "jsc-jit",
        prefix: "JSC:JIT:",
        jit: true,
    },
];

/// Run `program args` with the collector's thread count forced up; `(ok, stdout, stderr)`.
fn run(program: &str, args: &[&str]) -> Result<(bool, String, String), String> {
    let output = Command::new(format!("{BIN}/{program}"))
        .args(args)
        .env("JSC_useConcurrentGC", "true")
        .env("JSC_numberOfGCMarkers", "4")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("spawn: {error}"))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// One-line, colon-free text for a marker's reason.
fn tidy(text: &str) -> String {
    text.trim()
        .replace('\n', "\\n")
        .replace(':', ";")
        .chars()
        .take(160)
        .collect()
}

impl Shell {
    /// Run a script and report it as `<prefix>RUN:PASS|FAIL:<name>`.
    fn check(&self, name: &str, args: &[&str], want: &str) -> bool {
        let prefix = self.prefix;
        let began = Instant::now();
        let result = run(self.program, args);
        let ms = began.elapsed().as_millis();
        match result {
            Ok((true, out, _)) if out == want => {
                println!("{prefix}RUN:PASS:{name}");
                println!("{prefix}TIME:{name}:{ms}");
                true
            }
            Ok((true, out, _)) => {
                println!("{prefix}RUN:FAIL:{name}:got {}", tidy(&out));
                false
            }
            Ok((false, out, err)) => {
                println!(
                    "{prefix}RUN:FAIL:{name}:exit failure {} {}",
                    tidy(&out),
                    tidy(&err)
                );
                false
            }
            Err(error) => {
                println!("{prefix}RUN:FAIL:{name}:{}", tidy(&error));
                false
            }
        }
    }

    /// The whole W2 sequence for this shell; whether every check passed.
    fn exercise(&self) -> bool {
        let prefix = self.prefix;
        let mut ok = true;
        // The first run is also the startup measurement.
        let began = Instant::now();
        let first = run(self.program, &["-e", "print(1+1)"]);
        let ms = began.elapsed().as_millis();
        match first {
            Ok((true, out, _)) if out == "2\n" => {
                println!("{prefix}UP:PASS");
                println!("{prefix}START:{ms}");
                println!("{prefix}RUN:PASS:arith");
            }
            Ok((status, out, err)) => {
                println!("{prefix}UP:FAIL:{status} {} {}", tidy(&out), tidy(&err));
                return false;
            }
            Err(error) => {
                println!("{prefix}UP:FAIL:{}", tidy(&error));
                return false;
            }
        }
        let tier = if self.jit { "tier:dfg" } else { "tier:none" };
        let hot_want = format!("{HOT_COMMON}{tier}\n");
        for (name, body, want) in [
            ("gc", GC_SCRIPT, GC_WANT),
            ("promise", PROMISE_SCRIPT, PROMISE_WANT),
            ("hot", HOT_SCRIPT, hot_want.as_str()),
            ("agents", AGENTS_SCRIPT, AGENTS_WANT),
        ] {
            let path = script(&format!("{}-{name}", self.program), body);
            ok &= self.check(name, &[&path], want);
        }
        ok &= jsc_stress::run(self.program, prefix);
        ok
    }
}

fn script(name: &str, body: &str) -> String {
    let path = format!("{SCRATCH}/{name}.js");
    if let Err(error) = fs::write(&path, body) {
        common::fail("jsc", &format!("write {path}: {error}"));
    }
    path
}

fn main() {
    if let Err(error) = fs::create_dir_all(SCRATCH) {
        common::fail("jsc", &format!("scratch dir: {error}"));
    }
    let mut ok = true;

    // W1: the C++ toolchain and ICU.
    match run("cxxproof", &[]) {
        Ok((status, out, err)) => {
            print!("{out}");
            if !status || !out.contains("JSC:W1:PASS\n") {
                println!("JSC:W1:FAIL:cxxproof:{}", tidy(&err));
                ok = false;
            }
        }
        Err(error) => {
            println!("JSC:W1:FAIL:cxxproof:{}", tidy(&error));
            ok = false;
        }
    }

    // W2: the shells. A shell that was not built is a failure, not a skip.
    for shell in &SHELLS {
        ok &= shell.exercise();
    }

    if ok {
        common::pass("jsc");
    } else {
        common::fail("jsc", "see the JSC: lines");
    }
}
