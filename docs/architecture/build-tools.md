# Build, tools & CI

**What it is.** The verification and codegen tooling that wraps the build (see
[boot.md](boot.md) for the pipeline itself) plus the CI workflows that run it.

**Tools**

| Tool | Key files | Purpose | CI |
|---|---|---|---|
| IDL compiler | `tools/midlc/midlc.py`, `idl/echo.midl`, `libs/generated/src/lib.rs`, `docs/idl/`, `idl/manifest.json` | Generate typed Rust wire helpers, per-interface docs and a manifest; `--check` fails when stubs are stale; method ids are stable hashes | `.github/workflows/midlc.yml` |
| Linux ABI bench | `tools/abi/{build,run,coverage}.py`, `tools/abi/fixtures/src/*` | Build musl fixtures, boot each as `INIT.ELF`, parse serial `ABI:*` lines into `docs/compat/matrix.md` + `compat.json`; coverage lists `ENOSYS` numbers | `.github/workflows/abi-compat.yml` |
| Kernel tests | `tools/test/run.py`, `kernel/src/tests/` | Boot a `LAZYOS_TESTS=1` image headless, parse `TEST:*` lines, write `docs/test/report.md` + `.json`; exits non-zero on failure/missing summary | `.github/workflows/kernel-tests.yml` |
| Screenshots | `tools/screenshot/qemu_shot.py`, `qemu_session.py`, `qemu_qmp.py`, `pngstats.py`, `tools/screenshot/examples/*.json` | Headless screenshot capture, scripted input injection (QMP `input-send-event`), programmatic PNG assertions | `.github/workflows/screenshots.yml` |
| Parcel codec tests | `libs/messenger/src/lib.rs`, `libs/generated/tests/echo.rs` | Host cargo tests (round-trip, limits, fuzz; generated-stub round trips) | `.github/workflows/messenger.yml`, `midlc.yml` |
| Demo runner | `tools/run_demo.py`, `src/main.rs` | One command to build and boot the image (`--headless`, `--no-build`, `--accel none`, `--cpu max`) | - |
| xui app build | `tools/xui/build.py`, `xui-app/` | Build the static-musl xui binaries (`m0`, `counter`, `client`, `sysmon`, `fabricmon`) for `LAZYOS_XUI_APP` | `.github/workflows/xui.yml` |
| MCP debug bridge | `tools/mcp/debug_bridge.py`, `test_debug_bridge.py` | Host MCP server that drives `messengerctl stats-json`/`tasks-json` over QMP + serial and parses the `MCP:<NAME>:` JSON lines ([design](../mcp-debug-bridge.md)) | `.github/workflows/mcp-bridge.yml` |
| Service evidence | `tools/services/evidence.py` | Grep the `LAZYOS_SERVICES=1` serial log for the service/health/login markers | `screenshots.yml` |

**Conventions**

- One machine-parseable line per result, so tooling is CI-able: `ABI:<name>:PASS`
  / `ABI:FAIL:<reason>`, `TEST:<name>:PASS|FAIL:<detail>` ending with
  `TEST:SUMMARY:PASS=<n> FAIL=<n>`.
- The ABI bench isolates one fixture per boot through the `LAZYOS_INIT` hook
  (embedded as `INIT.ELF`); BusyBox uses the `LAZYOS_BUSYBOX` hook.
- Screenshot tooling discovers QEMU from `--qemu`, then `PATH`, then
  `C:\Program Files\qemu` on Windows; input injection works headless via QMP.
- Generated output is git-ignored (`.gitignore`): `shots/`, `docs/compat/`,
  `docs/test/`; CI publishes the artifacts (and the screenshots branch / wiki
  copies).

**CI workflows** (`.github/workflows/`)

| Workflow | Jobs |
|---|---|
| `kernel-tests.yml` | `tools/test/run.py` (KVM when usable); appends the report to the job summary; uploads `docs/test/**`, `shots/kernel-tests/**` |
| `abi-compat.yml` | Builds fixtures, runs the bench, uploads `docs/compat/**` + `shots/abi/**`, publishes the matrix/coverage to the wiki, comments on PRs |
| `screenshots.yml` | Captures, verifies with `pngstats.py`, uploads artifacts, publishes to the `screenshots` branch, comments images on PRs |
| `messenger.yml` | `cargo test -p libmessenger` |
| `midlc.yml` | `test_midlc.py`, `midlc.py --check`, `cargo test -p messenger-generated` |
| `clippy.yml` | `cargo clippy -p kernel` with `-D clippy::undocumented_unsafe_blocks` (issue #124 gate) |
| `xui.yml` | Builds the xui apps, boots the owner (`m0`, `counter`, `sysmon`, `fabricmon` over `LAZYOS_SERVICES=1`) and client sessions headless, checks `XUIAPP:*`/`SYSMON:*`/`FABMON:*` markers and pixels |
| `mcp-bridge.yml` | `tools/mcp/test_debug_bridge.py` (serial-line matcher; no QEMU) |

**Invariants**

- CI boots QEMU with `--accel auto`: every QEMU workflow runs
  `tools/ci/enable_kvm.sh` (opens the hosted runner's `root:kvm` `/dev/kvm` to
  the runner user), and `auto` picks KVM only if a probe start succeeds,
  otherwise TCG. `--accel none` still forces TCG for reproduction that must not
  depend on host virtualization.
- Tools are Python 3 with no third-party hard requirement except where noted in
  their READMEs (`tools/*/README.md`).
- The ABI bench and kernel suite are regression gates for changes to memory,
  scheduling and the syscall surface.

**Status.** All eight workflows exist; `docs/test/` and `docs/compat/` are
generated, so only the tools and (in the repository) the scripts are tracked.
