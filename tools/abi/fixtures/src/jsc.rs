//! `jsc` — run JavaScriptCore's shell and the C++ proof that
//! `tools/jsc/build.py` builds (an image built with `LAZYOS_JSC=1` puts
//! `jsc` and `cxxproof` in `/system/bin`; docs/webkit-port-plan.md W1, W2).
//!
//! The markers are the evidence and go to the serial log, one per line:
//! everything `cxxproof` prints (`JSC:W1:*`), then `JSC:UP:PASS` once `jsc`
//! answers, `JSC:START:<ms>` (how long `jsc -e` took), and `JSC:RUN:PASS:<name>` or
//! `JSC:RUN:FAIL:<name>:<reason>` for each script: arithmetic, a script that
//! allocates enough to force collections with concurrent marker threads on,
//! and one with classes, Promises, async generators and Intl (ICU). The
//! fixture ends with `ABI:jsc:PASS` when every one passed; `tools/jsc/run.py`
//! judges the log (`tools/jsc/judge.py`).

mod common;

use std::fs;
use std::process::{Command, Stdio};
use std::time::Instant;

const BIN: &str = "/system/bin";
const SCRATCH: &str = "/tmp/jsc";

/// Allocates a few hundred thousand objects, keeps one in a hundred, and asks
/// for collections: the concurrent marker threads suspend the mutator with
/// signals (per-thread signals, docs/webkit-port-plan.md 4.2).
const GC_SCRIPT: &str = r#"
let keep = [];
let total = 0;
for (let i = 0; i < 300000; i++) {
  let o = { a: i, b: [i, i + 1, i + 2], s: "str" + i };
  if (i % 100 == 0) keep.push(o);
  total += o.b.length;
  if (i % 50000 == 0) gc();
}
fullGC();
let sum = 0;
for (const o of keep) sum += o.a;
print("gc-ok:" + total + ":" + keep.length + ":" + sum);
"#;

const GC_WANT: &str = "gc-ok:900000:3000:449850000\n";

const PROMISE_SCRIPT: &str = r#"
class Animal {
  #n;
  constructor(n) { this.#n = n; }
  get name() { return this.#n; }
  speak() { return `${this.name} makes a sound`; }
  static make(n) { return new this(n); }
}
class Dog extends Animal { speak() { return super.speak() + " (woof)"; } }
const out = [];
const later = (v) => new Promise((r) => Promise.resolve().then(() => r(v)));
async function main() {
  out.push(Dog.make("Rex").speak());
  const vals = await Promise.all([later(1), later(2), later(3)]);
  out.push(vals.reduce((a, b) => a + b));
  try { await Promise.reject(new Error("boom")); } catch (e) { out.push(e.message); }
  for await (const x of (async function* () { yield "a"; yield "b"; })()) out.push(x);
  out.push([..."héllo".normalize("NFD")].length);
  out.push("İ".toLowerCase().length);
  out.push(new Intl.NumberFormat("en-US").format(1234567.891));
  out.push("straße".toUpperCase());
  print(out.join("|"));
}
main().catch((e) => print("error: " + e));
"#;

const PROMISE_WANT: &str = "Rex makes a sound (woof)|6|boom|a|b|6|2|1,234,567.891|STRASSE\n";

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
    text.trim().replace('\n', "\\n").replace(':', ";").chars().take(160).collect()
}

/// Run a `jsc` check and report it as `JSC:RUN:PASS|FAIL:<name>`.
fn check(name: &str, args: &[&str], want: &str) -> bool {
    match run("jsc", args) {
        Ok((true, out, _)) if out == want => {
            println!("JSC:RUN:PASS:{name}");
            true
        }
        Ok((true, out, _)) => {
            println!("JSC:RUN:FAIL:{name}:got {}", tidy(&out));
            false
        }
        Ok((false, out, err)) => {
            println!("JSC:RUN:FAIL:{name}:exit failure {} {}", tidy(&out), tidy(&err));
            false
        }
        Err(error) => {
            println!("JSC:RUN:FAIL:{name}:{}", tidy(&error));
            false
        }
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

    // W2: the shell. The first run is also the startup measurement.
    let began = Instant::now();
    let first = run("jsc", &["-e", "print(1+1)"]);
    let ms = began.elapsed().as_millis();
    match first {
        Ok((true, out, _)) if out == "2\n" => {
            println!("JSC:UP:PASS");
            println!("JSC:START:{ms}");
            println!("JSC:RUN:PASS:arith");
        }
        Ok((status, out, err)) => {
            println!("JSC:UP:FAIL:{status} {} {}", tidy(&out), tidy(&err));
            ok = false;
        }
        Err(error) => {
            println!("JSC:UP:FAIL:{}", tidy(&error));
            ok = false;
        }
    }
    let gc = script("gc", GC_SCRIPT);
    ok &= check("gc", &[&gc], GC_WANT);
    let promise = script("promise", PROMISE_SCRIPT);
    ok &= check("promise", &[&promise], PROMISE_WANT);

    if ok {
        common::pass("jsc");
    } else {
        common::fail("jsc", "see the JSC: lines");
    }
}
