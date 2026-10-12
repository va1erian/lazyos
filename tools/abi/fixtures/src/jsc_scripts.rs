//! The JavaScript the `jsc` fixture runs, and the output each script must print
//! (`tools/abi/fixtures/src/jsc.rs`).

/// Allocates a million and a half objects, keeps one in twenty, and asks
/// for collections: the concurrent marker threads suspend the mutator with
/// signals (per-thread signals, docs/webkit-port-plan.md 4.2).
pub const GC_SCRIPT: &str = r#"
let keep = [];
let total = 0;
for (let i = 0; i < 1500000; i++) {
  let o = { a: i, b: [i, i + 1, i + 2], s: "str" + i };
  if (i % 20 == 0) keep.push(o);
  total += o.b.length;
  if (i % 250000 == 0) gc(); else if (i % 50000 == 0) edenGC();
}
fullGC();
let sum = 0;
for (const o of keep) sum += o.a;
print("gc-ok:" + total + ":" + keep.length + ":" + sum);
"#;

pub const GC_WANT: &str = "gc-ok:4500000:75000:56249250000\n";

pub const PROMISE_SCRIPT: &str = r#"
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

pub const PROMISE_WANT: &str = "Rex makes a sound (woof)|6|boom|a|b|6|2|1,234,567.891|STRASSE\n";

/// Hot numeric code: a recursive function, a long loop, a sort with a
/// comparator and string building. Run by both shells, so the time
/// difference is the JIT's. Its last line says whether the DFG compiled
/// `fib` (the C-loop shell reports a sentinel of a million compiles), after
/// giving a compile that has not finished yet time to (the JIT compiles on
/// another thread, and the guest has one CPU).
pub const HOT_SCRIPT: &str = r#"
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
const f = fib(27);
let sum = 0;
for (let i = 0; i < 6000000; i++) sum = (sum + i * 3) % 1000003;
const arr = [];
let x = 12345;
for (let i = 0; i < 60000; i++) { x = (x * 1103515245 + 12345) & 0x7fffffff; arr.push(x); }
arr.sort((a, b) => a - b);
let s = "";
for (let i = 0; i < 20000; i++) s += String.fromCharCode(97 + (i % 26));
print("hot-ok:" + f + ":" + sum + ":" + arr[0] + ":" + arr[arr.length - 1] + ":" + s.length);
for (let k = 0; k < 4000 && numberOfDFGCompiles(fib) == 0; k++) fib(15);
const dfg = numberOfDFGCompiles(fib);
print(dfg > 0 && dfg < 1000000 ? "tier:dfg" : "tier:none");
"#;

pub const HOT_COMMON: &str = "hot-ok:196418:513:20736:2147470080:20000\n";

/// Four SharedArrayBuffer agents (real threads, each its own VM) allocate in
/// a loop and park in `Atomics.wait` while the main thread allocates, wakes
/// them with `Atomics.notify` and collects: threads that stop and start
/// under the collector (docs/webkit-port-plan.md 4.2).
pub const AGENTS_SCRIPT: &str = r#"
const AGENTS = 4;
const ROUNDS = 40;
const agentSource = `
  $.agent.receiveBroadcast(function(sab) {
    const ia = new Int32Array(sab);
    let keep = [];
    let seen = 0;
    for (let round = 0; round < ${ROUNDS}; round++) {
      for (let i = 0; i < 15000; i++) {
        const o = { a: i, s: "agent" + i, b: [i, i + 1] };
        if (i % 40 == 0) keep.push(o);
      }
      if (keep.length > 3000) keep = keep.slice(-50);
      Atomics.add(ia, 1, 1);
      Atomics.wait(ia, 0, seen, 20);
      seen = Atomics.load(ia, 0);
    }
    Atomics.add(ia, 2, 1);
    $.agent.report("done:" + keep.length);
    $.agent.leaving();
  });
`;
for (let i = 0; i < AGENTS; i++) $.agent.start(agentSource);
const sab = new SharedArrayBuffer(64);
const ia = new Int32Array(sab);
$.agent.broadcast(sab);
let keep = [];
for (let round = 0; round < ROUNDS; round++) {
  for (let i = 0; i < 30000; i++) {
    const o = { a: i, s: "main" + i, b: [i] };
    if (i % 30 == 0) keep.push(o);
  }
  if (keep.length > 5000) keep = keep.slice(-100);
  Atomics.add(ia, 0, 1);
  Atomics.notify(ia, 0);
  if (round % 4 == 0) fullGC(); else edenGC();
}
let reports = 0;
const deadline = $.agent.monotonicNow() + 120000;
while (reports < AGENTS && $.agent.monotonicNow() < deadline) {
  if ($.agent.getReport() === null) $.agent.sleep(10); else reports++;
}
fullGC();
print("agents-ok:" + reports + ":" + Atomics.load(ia, 2) + ":" + (Atomics.load(ia, 1) >= AGENTS * ROUNDS));
"#;

pub const AGENTS_WANT: &str = "agents-ok:4:4:true\n";
