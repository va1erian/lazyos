# AGENTS.md

Guidance for AI agents working in this repository.

## Visual verification workflow

LazyOS renders to a framebuffer, so correctness is often visual. Do not claim a
graphics change works from source alone — capture and inspect real pixels.

1. **Ensure QEMU is available.** On Windows, `C:\Program Files\qemu` must be on
   `PATH`, or pass `--qemu "C:\Program Files\qemu\qemu-system-x86_64.exe"`.

2. **Capture a screenshot** (headless; no window needed):

   ```bash
   python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image target/lazyos.img
   ```

   Omit `--image` to boot firmware only and validate the capture pipeline.

3. **Inspect it visually.** Read the PNG in the workspace (e.g. `shots/shot_10s.png`)
   with the Read tool to actually see the rendered output.

4. **Assert programmatically** so the check is reproducible and CI-able:

   ```bash
   python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
   ```

   Add `--expect-width`, `--expect-height`, `--min-colors`, or `--max-mean` as needed.

5. **Run CI** for a full headless pass: `.github/workflows/screenshots.yml`
   captures, verifies, uploads artifacts, publishes to the `screenshots` branch,
   and comments images on pull requests.

See `tools/screenshot/README.md` for full options.

## Driving the guest (input injection)

To interact with LazyOS — type commands, click, scroll — script it with
`qemu_session.py` and capture the resulting pixels:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/session --script tools/screenshot/examples/type_and_shot.json
```

Then Read the resulting `shots/session/shot_*.png`. For custom agent loops,
import `tools/screenshot/qemu_qmp.py` and call `type_text`, `press_key`,
`mouse_move`, `mouse_click`, `mouse_scroll`, `mouse_abs`, and `screenshot`.
Input is delivered via QMP `input-send-event`, so it works headless; the guest
only reacts once it has a keyboard/mouse driver (Phase 3+).

## Running the demo

LazyOS is a preemptive, single-address-space-per-task kernel with a simple
terminal multiplexer. Boot it with one command:

```bash
python tools/run_demo.py
```

Two windows appear, each running a different ring-3 program concurrently
(`hello` and the `sh` interpreter). **Tab** moves keyboard focus (the focused
window has a green border); typed input goes to the focused program. A mouse
cursor sprite follows the PS/2 mouse.

The boot disk is MBR + a **FAT12/FAT16** partition. Files are added at build
time in `build.rs` via `DiskImageBuilder::set_file_contents` / `set_file`; the
kernel reads them with the ATA PIO driver (`block::ata`) and a read-only FAT
reader (`fs`). `HELLO.ELF` and `SH.ELF` are real ring-3 programs (`user/`),
loaded by `process::load_image` into each task's own address space at
`0x400000`. `user/src/lib.rs` is the shared ring-3 runtime: `sys` (the
`int 0x80` wrappers) and a bump heap allocator backed by the `sbrk` syscall
(number 4), so user programs can use `alloc`. Syscalls: `exit` (0), `write` (1),
`read_char` (2), `read_file` (3), `sbrk` (4). `SH.ELF` is a small Dyon-inspired
interpreter (`user/src/lang/`: `lexer`, `parser`, `interp`, `value`) supporting
`f64` numbers, booleans, strings, arrays, `let`, `print`, `if`/`else`,
arithmetic, comparisons and indexing.

### How multitasking works

- `mem` can build a fresh address space (`new_user_table`) sharing the kernel's
  higher-half mappings; each task has its own PML4, kernel stack, heap break,
  terminal buffer and input queue.
- `task` runs a round-robin scheduler. The timer ISR (`task::switch::timer_isr`,
  a naked stub) saves the GP registers, calls `task::schedule`, and resumes the
  returned stack — switching address space (`CR3`) and the ring0 stack (TSS
  `RSP0`) as needed.
- `mux` is the kernel task (slot 0): it repaints the windows when output or
  focus changes. `Tab` is handled in the keyboard IRQ.

QEMU hardware acceleration (WHPX on Windows, KVM on Linux) is auto-detected and
makes rendering several times faster than TCG; force it off with `--accel none`.
`run_demo.py` builds `target/lazyos.img` if needed (`--no-build`, `--headless`,
`-- --cpu max` are supported). For scripted visual verification, capture a
session script instead:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/demo --script tools/screenshot/examples/multitask_demo.json
```

## Project conventions

- The OS is `no_std`; target `x86_64-unknown-none`; built via the root crate's
  `build.rs` + artifact dependency (see the roadmap issues).
- Do not commit generated screenshots (`shots/` is git-ignored); CI publishes
  them to the dedicated `screenshots` branch.
- Prefer verifying with the existing scripts over ad-hoc commands so results are
  comparable across runs.

## Commands

```bash
python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image <img>
python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
```
