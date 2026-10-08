# Plan: *Inside LazyOS*, a technical tour (ebook)

A book-length guided tour of LazyOS for a reader who knows Rust and has
read an operating-systems textbook, but has never opened this repository.
It follows one idea through the whole system: **a small kernel and a
capability-checked message fabric, with everything else built on top in
userspace.** Output is a typeset PDF built from the repository, so it can be
rebuilt, and checked, whenever the code changes.

Status: plan only. Nothing under `book/` exists yet.

---

## 1. Goals and non-goals

**Goals**

1. **True to the code on `main`.** The source of truth is the code, not the
   plan documents. The plans in `docs/` drift: `platform-plan.md` §11 still
   says LazyWeb runs on NetSurf, but #655 moved it to Blitz. Every chapter is
   written from the source and checked against it (§5).
2. **Pleasant to read.** Each chapter is a story about one problem, not a
   tour of a directory listing. Short code excerpts, a diagram where the
   mechanism is spatial, a screenshot where the result is visual, and an
   honest "what is missing" ending.
3. **Reproducible.** One command builds the PDF. The code excerpts are pulled
   from the tree at build time, so they cannot silently go stale.
4. **A nicely typeset PDF**: proper book layout, real ligatures and kerning,
   syntax-highlighted listings, vector diagrams, a table of contents and an
   index of paths and names.

**Non-goals**

- Not an API reference. `docs/architecture/*.md` already is one; the book
  points at it.
- Not a roadmap. Missing features are named once, at the end of the chapter
  they belong to, not designed.
- Not complete. Sound, USB, networking internals, the filesystem journal,
  printing, packages and the label policy get a page or a sidebar each, not
  a chapter (a second volume could take them).

## 2. Audience and voice

- Reader: a systems programmer. Explain LazyOS's choices, not what a page
  table is. One-line reminders are fine ("a TLB shootdown is ...") when a
  choice depends on them.
- Voice: present tense, active, concrete. "`xuid` composites only the damaged
  rectangles" rather than "damage tracking is employed". Same house style as
  the repo's docs: say what a thing does and why, no marketing.
- Each chapter opens with a **scene**: something the reader can watch happen
  (a key press reaching the Terminal, `ls` running as a Linux binary, a Rhai
  one-liner changing the theme) and then explains how it happened.
- Each chapter ends with **"Where it stops"** (honest gaps, from the code)
  and **"Read next"** (3-6 files to open, with what to look for).

## 3. Shape of the book

About 220-250 pages at B5 trim, eight parts. Page estimates are for
prose plus figures, listings capped at about a fifth of each chapter. The
size of the code behind each part (non-test lines, measured 2026-10-07) is
there to keep the space roughly proportional to the substance.

| Part | Chapters | Pages | Code behind it |
|---|---|---|---|
| 0 Front matter | Preface, how to read, building the book and the OS | 10 | |
| I The machine | 1-4 | 50 | `kernel/` ~72k lines (+74k of tests) |
| II Running Linux programs | 5 | 18 | `kernel/src/process/linux/` ~10.7k |
| III Messenger | 6-9 | 56 | `kernel/src/ipc/` ~11k, `libs/messenger`, `idl/` (30 interfaces), `tools/midlc` |
| IV Services | 10 | 14 | `user/src/bin/` ~63k |
| V Pixels | 11-12 | 34 | `user/src/bin/xuid/` (37 files), `xui-app/src/backend/` |
| VI The desktop | 13 | 16 | `xui-app/crates/shell`, `xui-app/src/shell` |
| VII Scripting | 14 | 14 | `rhai-host/`, `libs/rhai-lazy/` |
| VIII Two applications | 15-16 | 34 | `lazyrad-os/`, `kernel/src/ipc/devspawn.rs`, `xui-app/web`, `nettls/` |
| Back matter | Epilogue, glossary, path index, colophon | 10 | |

### Front matter

- **Preface.** What LazyOS is for, the principles in `platform-plan.md` §1
  restated in the book's own words, and how the project is built (the test
  harnesses and the "verdict is the pixels / the recording / the pcap"
  culture: a theme the book returns to).
- **How to read this book.** Conventions: `path:line` references pinned to a
  commit printed on the copyright page; the margin icons for *sidebar*,
  *try it* and *where it stops*.
- **A first boot.** `python tools/run_demo.py`, the login screen, the
  desktop. Screenshot. Then a one-page map of the system (the figure the
  whole book hangs from): hardware, kernel, the fabric, services, `xuid`,
  apps, with each later chapter's number on its box.

### Part I: The machine (kernel and drivers)

**1. From power-on to the first task** (12 pp.)
Scene: the serial log of a boot, annotated. The MBR image with its three
partitions (`build_support/os_*.rs`, `/boot` FAT with the kernel and
`lazyos.cfg`, the ext2 OS volume at LBA 131072); the bootloader handing over;
the higher-half kernel at `0xffff_8000_0000_0000` (`kernel/src/mem/layout.rs`);
GDT/IDT, the PIC in front of the bootstrap APIC in virtual-wire mode
(`kernel/src/arch/lapic.rs`), the timer, `limits.rs` deriving ceilings from
RAM, and the hand-off to `init`. Sidebar: how `build.rs` builds the image
and updates it in place.

**2. Memory** (12 pp.)
Frames with refcounts and free lists, page tables, the VMA list, demand-zero
pages, copy-on-write fork, `mmap`/`mremap`, page-table reclaim; the kernel
heap that grows on demand and the slab with owner accounting; per-uid quotas.
Figure: a fork, before and after the first write. Key files:
`kernel/src/mem/{frames,vma,cow,heap,slab}.rs`, `kernel/src/quota/`.

**3. Tasks, time and waiting** (13 pp.)
256 task slots, the strict-class stride scheduler with weights, the context
switch (`task/switch.rs`), per-task FPU state, wait queues and `wait_any`,
signals with Linux `rt_sigframe` delivery, the tty line discipline and ptys.
The "interrupts off in syscalls" design and its poll points
(`arch/irq_window.rs`, `IRQOFF:MAX`), told as a design trade-off with the
latency harness's numbers (`docs/perf/report.md`). Single CPU, stated plainly.

**4. Devices, drivers and storage** (13 pp.)
Two driver worlds. In the kernel: ATA and virtio-blk, the VFS (mount flags,
permissions, dentry/inode caches), the ext2 library shared with the host
build (`libs/ext2fs`), its write-back block cache with crash-safe commit
order, the journal as a sidebar. In userspace: the device core
(`kernel/src/dev/`: PCI enumeration, typed resources, claims, DMA and IRQ
grants under per-class policy) and the drivers that use it (`sndd`, `usbd`,
`netdrv`), started by `devd` through `init`. Worked example: one interrupt
from a virtio-sound card to `audiod`. Figure: the claim/grant handshake.

### Part II: Running Linux programs

**5. A Linux-shaped hole in the kernel** (18 pp.)
Scene: `sqlite3` and `rg` running unmodified in the Terminal. The syscall
gate (`arch/linux.rs`, `process/gate.rs`), the dispatch in
`kernel/src/process/linux/`, and how the shim maps Linux concepts onto native
ones: fd tables, `fork`/`execve` with `#!`, futexes and threads, epoll,
`AF_UNIX`, `AF_INET` forwarded to `netd` over Messenger, ptys. Why it is a
*guest* ABI, not the native one. The conformance bench (`tools/abi/`) as the chapter's evidence, with its
matrix printed as a table: `tools/abi/run.py` writes `docs/compat/compat.json`
(not checked in), so the book build runs the bench or takes the JSON from
the CI run of the pinned commit. Ends on the gap list from
`tools/abi/coverage.py` (dynamic linking, `timerfd`, `signalfd`, ...).

### Part III: Messenger

Messenger is what makes LazyOS LazyOS, and several of its rules are not
what a reader expects from other IPC systems (handles *move* rather than
copy, buffers are *shared* rather than moved, replies carry no objects, a
request may carry only what its `.midl` method declares). This part gets
more room, a stricter standard of precision than the rest of the book
(§5.7), and four chapters instead of three.

**6. Messages: parcels, channels and transactions** (14 pp.)
Scene: `confctl get` and what crosses the kernel. The vocabulary first, in
one table (handle, endpoint, channel, parcel, transaction, topic), then the
mechanics:

- *The parcel* (`libs/messenger/src/{parcel,envelope,tlv}.rs`): header with
  `(interface_id, method)`, the TLV body, the handle and buffer vectors.
  Figure: a parcel drawn to scale, byte offsets from the code.
- *Channels and endpoints* (`kernel/src/ipc/channels/`): queues, one-way
  `send`, `recv` and wait sets (`recv/waitset.rs`).
- *Synchronous transactions* (`channels/call.rs`, `txn.rs`): `begin_call`,
  `await_reply`, `reply`, `cancel`; deadlines, and the subtle rule that a
  callee gets its whole service turn before a deadline is enforced
  (`expire_served_polls`). State diagram of a transaction with every
  transition labelled by the function that takes it.
- *Copies*: the `_owned` paths copy a parcel in once and out once; the
  figure counts them.
- *`Connect`* (`channels/connect.rs`): a per-connection channel to a named
  service.

**7. Moving things between processes: handles and buffers** (16 pp.)
The chapter the reader is most likely to get wrong, so it is told as a
step-by-step trace before any generalisation:

- *The handle table and rights* (`kernel/src/ipc/handles.rs`): kinds
  (`Endpoint`, `Channel`, `Object`, `Buffer`, `Device`), rights (`CALL`,
  `DUPLICATE`, `TRANSFER`, `MONITOR`, `CONTROL`), and why a device claim is
  never transferable.
- *A handle transfer, frame by frame*: the sender's table, the message in
  flight, the receiver's table, at each step of `send` -> queue -> `recv`.
  The handle **moves**: it needs `TRANSFER`, the sender's slot closes once
  the message is queued, delivery opens a new receiver-local handle number.
  What happens on each failure path (refused, queue full, receiver gone,
  queue discarded) and who holds the object at that moment.
- *The declared-transfer gate* (`channels/declared.rs`, issue #516): the
  check against `midlc`'s `DECLARED_TRANSFERS` runs *before* any handle is
  resolved, so a refused request leaves the sender's table untouched; fewer
  than declared passes the gate, servers still demand an exact match
  (`Message::carries`); replies refuse every transfer (`reply_owned`).
- *Shared buffers* (`kernel/src/ipc/shared/`): buffers **share**, not move.
  The refcount is handles + in-flight messages + mappings (`retain`,
  `attach`, `release`), the mapping window at `SHARED_WINDOW_BASE`,
  `SHARE_ONLY`, the per-process quota from `limits.rs`. Same frame-by-frame
  trace, now showing the refcount.
- *Ordering without fences*: the kernel has no fence (issue #677); the
  worked example is a real one, an xui client presenting a frame to `xuid`
  and waiting for the `Present` reply (chapter 11 picks it up from there).
- A summary table at the end: for each object kind, whether it moves,
  shares or is refused, in requests, replies and topics.

**8. Who may call whom** (12 pp.)
Kernel-stamped credentials, the default-deny ACL in `ipc::authorize`,
labels compiled from package manifests, the hash-chained audit ring, the
name registry and reserved namespaces, topics and their policy hook, and
`messengerd` as registry and central broker. Told honestly: no uid policy
is loaded yet, so unlabelled tasks run under bootstrap-allow, and installed
apps are where confinement is real today. Includes a traced `LABEL:DENY`
run as a worked example.

**9. Interfaces as code: MIDL and `midlc`** (14 pp.)
Scene: adding a method to a toy interface (`idl/echo.midl`) and watching the
generated client, server and Rhai module change. The language, the
generator (`tools/midlc/`), the conformance suite, `idl/manifest.json`, the
topic catalogue. Why the project forbids hand-written protocol code.

### Part IV: Services

**10. `init` and the cast of daemons** (14 pp.)
`init` as service supervisor, app launcher and the only one allowed to power
off (`user/src/bin/init/`: supervision, stop order, sessions, residents,
failures and the "stopped unexpectedly" notice via `libs/svcpolicy`).
`messengerd` (registry and topic broker), `confd`, `logd`, `accountsd`,
`logind`, `keyd` (Argon2id), `clipboardd`, `mimed`, `pkgd`. A table of every
service, its uid, its interface and its chapter. The login sequence as the
worked example, ending with the session running as `user` with no capability.

### Part V: Pixels

**11. `xuid`, the compositor** (18 pp.)
The display grant (syscall 12), surfaces as shared buffers, the pipelined
Present with damage-only compositing, window management (stacking, resize,
maximize, minimize, animations), input from `inputd` with grabs and focus,
drag and drop through `clipboardd` tokens, the shell protocol (desktop and
panel roles, window-list events, hotkeys), live themes from `confd`, HiDPI
at integer 2x. Figure: one frame, from a client's `Present` to scanout.
Screenshots: the same desktop at 1x and 2x.

**12. xui on LazyOS** (16 pp.)
xui is a separate toolkit (`va1erian/xui`, pinned in `xui-app/Cargo.toml`);
this chapter is about the seam. The `LazyOSBackend`
(`xui-app/src/backend/`): event loop, painter on tiny-skia, focus, pointer,
z-order, drag gestures. Apps as static-musl ELFs in their own workspace.
Writing one: `tools/xui/new_app.py` and the evidence markers (`UP`, `QUIT`).
Blitz views (`xui-blitz`) and `webfonts` as the bridge to HTML/CSS content.

### Part VI: The desktop

**13. LazyShell** (16 pp.)
The desktop, taskbar, LazyOS menu, tray (`os.lazy.shell.tray`) and notices
(`xui-app/crates/shell`, `xui-app/src/shell`); how the shell gets its app
list from `init` and `pkgd`, launches through `init.Launch`, and logs out.
The Terminal and BusyBox `sh` on a pty as the shell's companion. Packages
and core apps (`.lzp`, `pkgd`, `/apps`, manifests to labels) as a sidebar.
UI probe (`LAZYOS_UI_PROBE=1`) as the bridge to the test sessions.

### Part VII: Scripting

Scripting comes before the two apps because LazyRAD is built on it.

**14. Rhai, the system's scripting language** (14 pp.)
Scene: one `rhai` line that reads a `confd` key and flips the theme. The
`rhai` command (`rhai-host/`), the bindings (`libs/rhai-lazy/`): the `msg`
module driven by the table `midlc --schema` generates, the documented
per-interface modules (`sys::confd::get(...)`, `libs/rhai-lazy/api/`), the
engine limits (`libs/rhai-lazy/src/limits.rs`), and the fact that a script
gets no authority of its own: it calls services as the task running it, so
the label and ACL rules of chapter 8 apply unchanged. Tested against an
in-memory fabric (`libs/rhai-lazy/src/mock.rs`). "Rhai first for small apps"
as the project's stated rule, and why.

### Part VIII: Two applications

Two apps that, between them, reach every layer of the book: one is about
the permission system, the other about the network stack.

**15. LazyRAD, an app that makes apps** (18 pp.)
Scene: design a form, write its script, press Play, then Make LazyOS App
and install the result. The interesting part is permissions. LazyRAD is
itself a core package (`xui-app/packages/lazyrad/manifest.toml`) confined to
its label, so it has to produce other confined apps without being able to
grant anything:

- *The player and the forms*: `.lfm` plus Rhai on `lrplay`, events delivered
  by the form's window (`lazyrad-os/src/messenger.rs`, `platform.rs`).
- *Make LazyOS App*: `lrplay.elf` is copied into an `.lzp` with the project
  and a manifest; the IDE never calls `pkgd` (whose answer to a labelled
  caller is no) and hands the file to the Installer through `mimed` and
  `init` instead (`lazyrad-os/src/handoff/`). The user's consent in the
  Installer is what turns a manifest into kernel label rules.
- *Play under the project's own permissions*: a child inherits its
  creator's label, so a naive Play would run the project with the IDE's
  rights. `develop = true` in the IDE's manifest, the `dev:<system_name>`
  label, the development consent in the Installer, and the narrow spawn
  rule in `kernel/src/ipc/devspawn.rs` (only `dev:` targets, only labels
  `pkgd` already loaded with approved rules, uid and session kept,
  capabilities a subset). Figure: the three labels (`app:os.lazy.lazyrad`,
  `dev:<name>`, `app:<name>`) and who creates each one.
- *Evidence*: `lazyrad_devplay.json` must show no `LABEL:DENY`; the MOD
  player (`lazyrad-os/src/tracker`, `libs/modplay`, sound through `audiod`)
  as the showcase of what a form app can do, judged by its recording.

**16. LazyWeb, a browser in userspace** (16 pp.)
Blitz for layout and paint, the fetcher with its cookie jar, HTTPS on the
in-tree stack (`nettls/`: rustls, a pure-Rust provider, the GPLv2
constraint), sockets through `netd` and smoltcp, the virtio-net driver at
the bottom. `mimed` starting the browser from a URL and handing `mailto:`
back to the OS. The harness that serves stand-in sites and judges the
requests and the screenshots (`tools/web/run.py`). The chapter that crosses
the whole stack, from a click to a packet capture and back to pixels.

Other apps (LazyWriter and printing, the PDF Viewer, the Archiver) appear as
short sidebars where they illustrate a mechanism: drag and drop, worker
threads, the print spooler.

### Back matter

- **Epilogue: where LazyOS goes next.** One page per open front (SMP, a real
  session security model, a uid policy, real hardware), each pointing at
  the plan doc that owns it.
- **Glossary** (Messenger, label, capability, core package, resident app ...).
- **Path index**: every `path` cited, with the pages that cite it,
  generated.
- **Colophon**: commit, build date, toolchain versions, fonts.

## 4. Typesetting and build

**Toolchain: Typst**, pinned and installed with `pip install typst==<pin>`
(the Python wheel bundles the compiler), the same pattern as zig. It is a
single dependency, fast enough to rebuild on every edit, does proper
microtypography, and scripts the parts that must be generated. LaTeX is the
fallback if Typst falls short, but it is a far heavier install on Windows
and in CI.

- **Layout:** B5 (176 x 250 mm), two-sided, 10.5 pt body, generous inner
  margin; chapter openers with the scene set in a side column; listings in a
  tinted block with the source path and line range as a caption.
- **Fonts** (all open licences, vendored under `book/fonts/` with
  provenance lines like `assets/manifest.txt`): a text serif (Source Serif 4
  or Libertinus Serif), a sans for headings and figures (Source Sans 3), a
  mono for code (JetBrains Mono or Iosevka). Final pick after setting one
  sample chapter in two candidates.
- **Diagrams:** drawn in Typst (`cetz`/`fletcher`, versions pinned and the
  packages vendored so the build works offline), in one style: same palette,
  stroke weights, font. About 25 figures across the book.
- **Screenshots:** produced by the existing session scripts
  (`tools/screenshot/qemu_session.py`) at a fixed commit, never by hand; a
  `book/shots.toml` names each figure's session, image build switches and
  frame. Embedded at 2x where the page shows them small.
- **Syntax highlighting:** Typst's built-in highlighter for Rust, Python and
  shell; a small `.sublime-syntax` for MIDL, Rhai and `.lfm`.
- **Generated tables:** the ABI matrix (the bench's `compat.json`), the
  service table (from `init`'s manifest), the interface list
  (`idl/manifest.json`), the line counts in §3. A build step writes them as
  JSON that Typst reads; nothing is copied in by hand.

**Navigation (the PDF is read on a tablet).** The PDF is the only output
for now, so it has to navigate well without a printed index finger:

- A **table of contents** at the front (parts, chapters, sections), every
  entry a link to its page.
- A **PDF outline** (bookmarks) with the same three levels, so a reader's
  sidebar shows the book's structure; Typst writes it from the headings.
- **Every cross-reference is a link**: "chapter 7", "figure 7.3", "§5.7",
  the path index entries, and each listing's caption back to its source
  path (and to the repository on GitHub at the pinned commit).
- **Page labels** that match the printed numbers (roman for the front
  matter), so "go to page 120" in a reader lands on page 120.
- Running heads with the chapter title, and a page size and 10.5 pt body
  that read comfortably at a tablet's full-page zoom.

`check.py` opens the built PDF (`pypdf`) and fails if the outline is
missing, if an outline or TOC entry points at the wrong page, or if a
cross-reference is unlinked.

Proposed layout:

```text
book/
  book.typ            # entry point: parts, chapters, styles
  style/              # page, headings, listings, figures, margin icons
  chapters/NN-*.typ   # one file per chapter, prose and figure calls
  figures/*.typ       # diagrams
  shots.toml          # screenshot recipes
  fonts/              # vendored fonts + MANIFEST
  vendor/             # pinned Typst packages
tools/book/
  build.py            # gather -> check -> typst compile -> target/book/inside-lazyos.pdf
  excerpt.py          # pull listings from the tree by anchor
  gather.py           # generated tables and the path index data
  check.py            # the fact checks of section 5
  shots.py            # run the screenshot recipes
  test_*.py           # the tools' own tests
```

`python tools/book/build.py` builds the PDF from the current tree;
`--shots` also re-captures screenshots (slow, needs QEMU); `--check-only`
runs §5 without typesetting. Every `.py` stays under 500 lines, like the
rest of `tools/`.

## 5. Keeping it true

The book's credibility rests on this section; it is part of the build, not
a review pass.

1. **Excerpts are pulled, never pasted.** A listing names a file and an
   anchor: a function or item name (`excerpt.py` finds `fn present` /
   `struct Handle` and its body), or a `// book:<name>` marker pair added to
   the source where an item boundary will not do. Elision (`// ...`) is
   applied by rules, so a reader can always find the full text. A missing
   anchor fails the build.
2. **Every cited path exists.** `check.py` collects `path` and `path:line`
   references from the chapters and fails on a missing file or a line past
   the end. Line references are used only for stable anchors, as in
   `docs/architecture.md`; the anchor form is preferred.
3. **Numbers come from the code.** Constants quoted in prose (`MAX_TASKS`,
   the kernel base, syscall numbers, default limits, the OS volume offset)
   are written as `#fact("task::MAX_TASKS")`, resolved by `gather.py` from
   the source; a fact that cannot be resolved fails the build.
4. **Claims are traced.** While drafting, every non-trivial claim carries a
   source comment in the `.typ` file (`// src: kernel/src/ipc/mod.rs authorize`).
   A chapter is not done until each claim was checked against that source by
   someone other than its writer (person or agent, reading the code, not the
   docs). The docs in `docs/` are leads, never sources.
5. **Pinned to a commit.** The copyright page prints the commit the book
   was checked against. Rebuilding on a later commit re-runs 1-3; prose
   drift is caught by re-review of the chapters whose cited files changed
   (`check.py --since <commit>` lists them).
6. **Run what it says.** Every "try it" box is a command from `AGENTS.md`
   or a harness; `check.py` verifies the script and flag names exist, and a
   release of the book runs the boxes once (the screenshot sessions cover
   most of them).
7. **Messenger is held to a stricter standard** (Part III). It is the
   system's core and its rules are easy to state almost right, which is
   worse than wrong. So for chapters 6-9:
   - *Every rule is backed by a test.* Each statement of behaviour ("the
     sender's slot closes once the message is queued", "a refused request
     leaves the sender's table untouched", "replies carry no transfers")
     cites the kernel test that proves it (`tests/ipc_channel_suite/`,
     `ipc_shared_suite/`, `transfer_gate_suite.rs`, `topics_gate_suite.rs`,
     `registry_suite/`). Where no test exists, the claim is either dropped
     or a test is added to the kernel suite first, so the book never
     asserts more than the code is held to.
   - *Traces are recorded, not drawn from memory.* The frame-by-frame
     figures of chapter 7 (handle tables, message in flight, buffer
     refcounts) are generated from a test-build run: a kernel test that
     performs the transfer and prints the table and refcount states as
     `BOOK:` lines, which `gather.py` turns into the figure data. If the
     semantics change, the figure changes or the build fails.
   - *Every edge case is answered.* For each object kind and each path
     (request, reply, one-way, topic), the chapter states what happens on
     success and on each failure (refused by rights, by the declared gate,
     by quota, queue full, peer gone, cancelled, deadline passed), and who
     owns the object afterwards. The summary table at the end of chapter 7
     is checked cell by cell against the code.
   - *Spec vs code.* Where `docs/messenger.md` describes something the code
     does not do yet (reply-borne handle transfers, for one), the book
     follows the code and says so in "Where it stops".
   - *Two reviewers.* Chapters 6 and 7 get a second, independent fact
     review that only reads the code and the tests, with the draft's
     claims as a checklist.

## 6. How it gets written

Per chapter, in this order:

1. **Brief** (half a page): the scene, the 3-5 points, the figures, the
   files to read, the "where it stops" list. Reviewed before drafting.
2. **Research** from the code: read the files in the brief, run the
   relevant harness, note facts with their sources.
3. **Draft** prose, figures and excerpt anchors.
4. **Fact review** (§5.4) and a **read-aloud pass** for flow: cut every
   sentence that only restates a file listing.
5. **Typeset check**: page breaks, widows, figure placement, listing
   lengths.

Milestones:

| # | Deliverable | Done when |
|---|---|---|
| B0 | Tooling: `book/` skeleton, `tools/book/{build,excerpt,check}.py` with tests, fonts and style; one dummy chapter with a listing, a figure and a screenshot | `python tools/book/build.py` writes a PDF with a linked TOC and outline; a renamed function breaks the build |
| B1 | Pilot: chapters 6 and 7 (messages and transfers) and the system-map figure | the style is settled on real content; review notes folded into §2 and §4 |
| B2 | Part I and Part II | chapters 1-5 pass §5 |
| B3 | Parts III-IV | chapters 8-10, and chapters 6-7 re-checked against §5.7 |
| B4 | Parts V-VI | chapters 11-13, screenshots from sessions |
| B5 | Parts VII-VIII | chapters 14-16 |
| B6 | Front and back matter, index, full read-through, CI job, the book in the guest (§7.4) | `.github/workflows/book.yml` builds and checks the book on PRs touching `book/` or cited files and uploads the PDF |

The pilot is Messenger rather than chapter 1 because it is the most
distinctive part of LazyOS and the hardest to explain well; if the style
works there, it works everywhere.

## 7. Decisions

1. **The two apps are LazyRAD and LazyWeb** (Part VIII). LazyRAD for how a
   confined app produces and runs other confined apps; LazyWeb for the
   whole network stack. LazyWeb just changed engine (#655), so its chapter
   is written last.
2. **Licence:** the prose and figures are CC BY-SA 4.0; code excerpts keep
   the repository's licence, stated in the colophon.
3. **PDF only** for now, built for tablet reading (§4, navigation). EPUB or
   HTML may come later from the same sources.
4. **The book ships in the guest**, opened by the PDF Viewer from
   `/system/share/docs/inside-lazyos.pdf`, behind a build switch so ordinary
   images stay small. Following the project's rule for optional image
   features, that means: `LAZYOS_BOOK=1` in the image build (copying
   `target/book/inside-lazyos.pdf`, path from `libs/fhs`), a
   `run_demo.py --book` flag that builds the PDF and sets it, a control on
   the launcher's Advanced tab wired through `tools/lazygui/catalog.py` with
   tests, and a Docs-app or menu entry that opens it through `mimed`. Lands
   with B6.
