# LazyVM on LazyOS

Companion to [`AGENTS.md`](../../AGENTS.md); covers: LazyVM, the Java 8
bytecode VM that runs on LazyOS and Windows, and the LazyOS-side work it
needs (the `LAZYOS_LAZYVM` build switch, the test hook, the display grant, the
`midlc` Java backend and app packaging).

LazyVM lives in its own repository (`lazyvm`). Its plan is `docs/plan.md` and
`docs/plan-2.md` there; the LazyOS-side items are `LOS-01` to `LOS-04` at the
end of `plan-2.md`. On LazyOS it is a static `x86_64-unknown-linux-musl` `std`
program run through the Linux ABI, like `xui-app`: windows through xuid's
`os.lazy.display.v1` client mode, Messenger through `lazyos-sys`, and one
`wait` over `WAIT_CALLS`, endpoints and an epoll `WAIT_FD` for its event loop
([`messenger.md`](../messenger.md) sections 6, 7 and 15,
[`architecture/call-completion.md`](../architecture/call-completion.md),
[`architecture/endpoint-fd.md`](../architecture/endpoint-fd.md)).

Work in this repo for LazyVM follows this repo's `AGENTS.md` (build switch,
core package, both front ends, evidence by pixels and serial markers) **and**
the shared LazyVM rules below. The block is an identical copy of the one in
LazyVM's `AGENTS.md`, which is the master; LazyVM's `tools/check_rules.py
--lazyos <path>` compares them.

<!-- lazyvm-rules:begin -->
## Shared rules for LazyVM work

**1. What you may read and copy (clean room).**
- You may read: the JVMS SE 8, the JLS SE 8 and the Java SE 8 API Javadoc
  (docs.oracle.com/javase/specs and docs.oracle.com/javase/8/docs/api); class
  files you compiled yourself, and `javap` output of them; the observable
  behaviour of a JVM running our tests (stdout, exceptions, timing); this repo;
  LazyOS; xui; and the documentation of crates we depend on.
- You must not open, read, search, decompile or copy: OpenJDK or any other
  JDK's source (`src.zip`, github.com/openjdk, mirrors, code search results),
  decompiled or disassembled JDK classes (`rt.jar`, `jmods`), or the source of
  another class library or JVM (GNU Classpath, Apache Harmony, Android
  libcore, J9, GraalVM). This applies to snippets in web results and to code
  "from memory". Write it from the spec instead. If a spec is unclear, write
  a test, bless it on the oracle JVM and match its output.
- Do not paste spec or Javadoc prose into the repo. Write doc comments in your
  own words, and cite the section (`// JVMS 5.4.3.3`) instead.

**2. Namespace and naming.**
- Add no `public` or `protected` type, field or method to `java.*` or
  `javax.*` beyond what the Java SE 8 API declares, with its exact signature.
  Helpers are package-private or go in `lazy.internal.*`. Our API is `lazy.*`.
- Never call the product "Java" or "Java compatible", and never use Oracle
  marks in names. It is "LazyVM".

**3. What counts as done (evidence over assertion).**
- A work item is done only when its **Check** in `docs/plan.md` (or `plan-2.md`) passes,
  run by you, on every platform it names. Do not claim behaviour from reading
  the source or from a build that merely compiles.
- `java.*` behaviour is judged by differential tests: `tests/java/...` goldens
  blessed on the oracle JVM with `tools/golden.py --bless`. Never hand-edit a
  `java.*` golden to make a test pass. `lazy.*` goldens are hand-written and
  reviewed.
- From S3 onward, "both" means Windows and LazyOS (`tools/lazyos_test.py`). UI
  work is judged by pixels (goldens or screenshots), not by log lines.
- Say plainly what you could not check, for example an IntelliJ step that
  needs the owner.

**4. Small, clean, one way.**
- Files stay under 500 lines; functions are small and single-purpose;
  comments say why. There is one way to do each thing: reuse the existing
  helper, and do not add a second.
- Every `unsafe` block is minimal, lives in `crates/vm/src/heap` or
  `crates/vm/src/interp` unless the plan says otherwise, and has a
  `// SAFETY:` comment.
- Malformed class files, jars and network input are errors, never panics.
- New dependencies must have a permissive license (MIT, Apache-2.0, BSD, Zlib,
  ISC), be recorded in `THIRD_PARTY.md`, and pass `python tools/licenses.py`.
  Never add `ring`, `aws-lc` or a GPL crate outside `crates/lazyvm-lazyos`.
<!-- lazyvm-rules:end -->
