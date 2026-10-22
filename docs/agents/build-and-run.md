# Build, run and the disk image

Moved out of [`AGENTS.md`](../../AGENTS.md) to keep it short. Running the demo, software suites, login and elevation, the OS volume layout, and the documentation site.

## Running the demo

LazyOS is built around **Messenger**, a kernel-mediated, capability-based
IPC/pub-sub fabric, with userspace system services (`messengerd`, `init`,
`logd`, `healthd`, `keyd`, `accounts`, `clipboardd`, the `xuid` display
compositor, and more) running over a preemptive multitasking kernel with a
VFS (an ext2 read/write OS volume at `/`, a read-only FAT `/boot`, ramfs
`/transient` and `/tmp`, an optional ext2 home volume). The authoritative description of the
current architecture and the staged roadmap (S0–S9) is
[`docs/platform-plan.md`](../platform-plan.md), with per-subsystem detail in
[`docs/architecture/`](../architecture) (boot, memory, tasks, filesystem,
IPC, processes, display, etc.) and focused plans for
[`messenger.md`](../messenger.md), [`security-model.md`](../security-model.md),
[`linux-abi-plan.md`](../linux-abi-plan.md), and [`xui-plan.md`](../xui-plan.md).
Read those for anything about kernel internals, syscall numbers, or
window/task management rather than assuming from comments elsewhere.

Before touching IPC, read the six concepts and the cheat sheet in
[`docs/messenger.md`](../messenger.md) (sections 1 and 5): a message is a
header, a TLV body and an object list; an interface's objects are parameters
of type `Channel<I>` (one end of a channel, which moves) or `Buffer` (shared
pages with a byte range, which is shared), declared in `.midl` like any other
field and refused in a reply, a topic, an `Option` or an `Array`. The index
rule and fixed cardinality apply: an object field's index is its position in
the method's declared order, the kernel refuses a request whose object list
is not exactly the declared kinds, and the generated `decode_<m>_args`
refuses any other index. `midlc` refuses the old `transfers (...)` clause;
there is no other way to carry a handle.

Boot it with one command:

```bash
python tools/run_demo.py
```

QEMU hardware acceleration (WHPX on Windows, KVM on Linux) is auto-detected and
makes rendering several times faster than TCG; force it off with `--accel none`.
`run_demo.py` builds `target/lazyos.img` if needed (`--no-build`, `--headless`,
`-- --cpu max` are supported; `--smp N` boots with N vCPUs, whose APs stay
parked in firmware until the kernel's SMP bring-up — docs/smp-plan.md). It also
creates `target/home.img` (the `/home`
volume, label `lazyhome`) when missing and attaches it as a second virtio-blk
disk (`--home-disk PATH`, `--no-home-disk`, `--reset-home`; `--reset-os` rebuilds
with `LAZYOS_RESET_OS=1`; `--data-disk` is opt-in). The screenshot tools attach
none unless given `--home-disk PATH`. For scripted visual verification, capture
a session script instead:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/demo --script tools/screenshot/examples/multitask_demo.json
```

### Software suites

`python tools/run_demo.py --suite base|full [--with srb2kart]` turns on a named
set of the flags below (`tools/suites.toml`, read through `tools/suites.py`), so
each flag's own prerequisite build runs and the OS volume is sized for it;
explicit flags still add on top. `base` is the desktop, `full` adds every
optional app (WebKit browsers, games and all), `srb2kart` is an option (its
data is downloaded at build time: never publish it). `--dry-run` prints the
flags, build steps and environment without building. The launcher's Simple tab
and `tools/boot/stick_gui.py` have the same *Software* choice. A new optional
app joins `full`, an option or the file's `test_only` list
(`python tools/test_suites.py` fails otherwise).

### Login and the session user

A desktop image boots to a login screen (`greeter`, run as the `_greeter`
uid); `user`/`lazy` (uid 1000) and `admin`/`nimda` (uid 1001, in the `admin`
group) are the development accounts, kept with their Argon2id hashes in the
account database `/accounts/db` (owned by `accountsd`'s `_accounts`;
`/system/etc/passwd` and `group` are views). Nobody is root. LazyShell, the
Terminal and every app run as the logged-in user with no capability, and the
LazyOS menu's "Log out..." row ends the session (docs/accounts-plan.md U0, #623).
`LAZYOS_SETUP=1` (`run_demo.py --setup`) builds a desktop with no account:
the login screen creates the owner (U1, #624). A privileged change (an
account, a `sys/**` setting, the clock, a core app, restarting a service)
goes through `elevd`: `xuid` shows the trusted prompt and an administrator
types their name and password (U2, #625; in a session script, type `admin`,
Tab, `nimda`, Enter, or Escape to cancel; from the Terminal,
`rhai -e 'sys::elevd::request("service.restart",["inputd"])'`).
`LAZYOS_AUTOLOGIN=<name>` (`run_demo.py --autologin NAME`, the GUI's
"Log in automatically") logs straight in; unset, an image that opens apps at
login (`LAZYOS_XUI_AUTOSTART`, every session-script image) logs `user` in, so
the example sessions run unchanged. A first-boot setup image
(`LAZYOS_SETUP=1`, `run_demo.py --setup`, which implies `--desktop`) never
logs anyone in. Services authorize privileged calls by
`CAP_SETUID` or by `elevd`'s kernel-stamped identity, never by uid 0: a
session cannot write `sys/**` keys, `/conf` or `/system`. The attack harness is `python tools/accounts/run.py`; the
session `tools/screenshot/examples/login_logout.json` (an image built with
`LAZYOS_AUTOLOGIN=none LAZYOS_UI_PROBE=1`) types a wrong and a right password,
logs out from the LazyOS menu ("Log out...", then "Log out now") and logs in again;
`accounts_setup.json` (an image built with `LAZYOS_DESKTOP=1 LAZYOS_SETUP=1
LAZYOS_AUTOLOGIN=none LAZYOS_XUI_AUTOSTART=term,settings LAZYOS_UI_PROBE=1`)
creates the owner, adds an account in Settings through the prompt, cancels
another prompt, changes a password and logs in as the new account, and
`accounts_setup_reboot.json` boots that image again (old password refused).
Session scripts work as `user` (`/home/user`, `user/1000/**` confd keys); a
step that must touch a system service asks `elevd` and approves the prompt as
`admin` (uid 1001, like any account; `xuid_pointer_restart.json` restarts
`inputd` with `service.restart`).

### The disk image

`cargo build` writes `target/lazyos.img` as an MBR disk with three partitions:
the bootloader's stage 2, a FAT `/boot` (only the kernel and a generated
`lazyos.cfg`), and an ext2 OS volume at LBA 131072 (64 MiB; `LAZYOS_OS_SIZE`,
default `512M`, minimum `128M`) that holds every other file: programs at their
real names in `/system/bin` (`/system/bin/init`, `/system/bin/busybox`), data in
`/system/etc` and `/system/share` (`/system/etc/passwd`), the docs in `/docs/os`,
and each service's state at its place in the tree (F4): `confd`'s store in
`/conf` (0700 root; an F3 image's `/data/confd` is merged in once), `logd`'s
journals and `pkgd`'s `pkg.log` in `/logs` (0750 root), installed apps in
`/apps` and their docs in `/docs/apps`, and a 0700 home per passwd account in
`/home/<name>` (hidden by the home volume when one is mounted). Nothing new is
written under `/data`; no regular file sits at the root (docs/filesystem-plan.md
F3). Binary data (samples, wallpapers, a demo's module files) goes in
`assets/` with one `path | licence | install | provenance` line per file in
`assets/manifest.txt` and lands at `/system/share/<path>`; `LAZYOS_ASSETS=<dir>`
(`run_demo.py --assets DIR`, the GUI's *Asset dirs*) adds a tree of your own
with the same manifest (`build_support/assets_embed.rs`, issue #454). The volume is written by `libs/ext2fs`, the code the kernel mounts it with
(`build_support/os_*.rs`); `cargo run -q -p ext2fs --example osread -- target/lazyos.img
cat /logs/service.log` reads it from the host (`/logs` is root-only in the guest).
A rebuild **updates the OS volume in place**: installed apps, settings, logs
and your own files survive, and only paths listed in `/system/.image-manifest`
are replaced or deleted. `LAZYOS_RESET_OS=1 cargo build` (or
`python tools/run_demo.py --reset-os`) recreates it with a new UUID; so does an
image that fails validation, with a `cargo:warning=` giving the reason. An
update also checks a volume that was not cleanly unmounted (a closed QEMU
window) and marks it clean once it has repaired what a crash can leave
(leaked blocks and inodes, link counts, counters; data-holding orphans go to
`/lost+found`) and the independent checker finds nothing wrong; the kernel
never does (it has no fsck), so until a rebuild every boot of such an image
prints `ext2: ... was not cleanly unmounted`. Damage no crash leaves fails the
build before anything is written (copy your files off, then reset);
`LAZYOS_UPDATE_DAMAGED_OS=1` updates such a volume anyway, at the risk of an
updated file reusing a damaged user file's block. An update that runs out of
room first removes the files an interrupted build left in `/system/{bin,lib,share,packages}`
and `/docs/os` (no manifest lists them; `build_support/os_update.rs`), then
fails with the sizes and the remedy. The default size is `512M` plus `512M` for
SRB2Kart and `512M` for the WebKit browsers (`1536M` with both). Changing
`LAZYOS_OS_SIZE` on an existing image needs the reset. A build that changed
nothing does not rewrite the image (`build_support/os_track.rs`: a missing
optional output is watched through its directory, never as a missing file, which
cargo takes for "always changed"), and the build tools write an output only
when its bytes changed (`tools/outfile.py`); keep both when adding an
embed or a tool. Do not rebuild while QEMU
has the image open (the build fails with a message). CI sets `LAZYOS_RESET_OS=1`
everywhere. ext2 is case-sensitive: look names up exactly as stored, through
`libs/fhs`. Host tests: `cargo test -p build-support-tests`.

## Documentation site (GitHub Pages)

The docs are published as a website, `https://va1erian.github.io/lazyos/`, built
from `docs/**/*.md`, `README.md` and `CONTRIBUTING.md` by `tools/site/build.py`
(the GitHub wiki is retired: do not write to it). Keep writing docs for GitHub's
file browser: relative links between `.md` files and `#fragments` are rewritten
for the site, a link to code outside `docs/` becomes a GitHub link, and the first
`# heading` is the page title. A new doc needs no registration (a top-level
`*-plan.md` lands under "Plans"; `tools/site/site.toml` lists the rest). After
renaming or adding docs, check links the way CI does:

```bash
pip install -r tools/site/requirements.txt
python tools/site/build.py --strict      # target/site; fails on a broken link or fragment
python tools/site/test_site.py
python -m http.server -d target/site     # look at it; Read a screenshot for layout changes
```

`.github/workflows/pages.yml` builds and deploys it on pushes to `main` that touch
the docs, and after every `abi-compat` run so the ABI matrix and syscall coverage
(CI output, git-ignored) are published. Plan and layout: `tools/site/README.md`.
