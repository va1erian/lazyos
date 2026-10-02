# Application packages (`.lzp`)

A LazyOS application ships as a single zip archive with the `.lzp` extension.
It holds a manifest, one or more binaries, the shell icons, and optional
interface definitions, documentation and resources. The installer hands the
whole archive to `libs/lazypkg` as `&[u8]`; the reader validates it and extracts
one entry at a time, and `tools/pkg/build.py` builds archives that the reader
accepts. **The two implementations must agree on every rule below, and both
have tests for them** (`cargo test -p lazypkg --all-features`,
`python tools/pkg/test_build.py`).

Companion to [`platform-plan.md`](platform-plan.md) and
[`security-model.md`](security-model.md). The reader is pure `no_std` + `alloc`
and touches no syscall, Messenger interface or filesystem.

---

## 1. Archive layout

```text
manifest.toml            required
bin/<name>.elf           one or more binaries, referenced from the manifest
icons/app-16.png         required
icons/app-32.png         required
icons/app-128.png        required
icons/<slug>-16.png      optional, icons for handled file types (also -32 and -128)
idl/*.midl               optional, the app's own Messenger interface definitions
docs/*.md                optional, Markdown documentation (docs/README.md is the entry point)
resources/**             optional, arbitrary app files
```

Any other top-level path is rejected. Files under `bin/` must end in `.elf`,
under `icons/` in `.png`, under `idl/` in `.midl`, under `docs/` in `.md`;
`resources/` is unconstrained. The three `app-*.png` icons must start with the
8-byte PNG signature (`89 50 4E 47 0D 0A 1A 0A`); the reader does not decode
them further.

### Compression

Only two zip compression methods are accepted: **0 (stored)** and **8
(deflate)**. Deflate streams are inflated with an output cap equal to the
declared uncompressed size, and a stream that produces more *or fewer* bytes
than declared is rejected. The builder deflates everything except `.png`
(already compressed), which it stores.

### Container rules

The reader parses the zip container itself and refuses anything it cannot prove
safe:

* zip64 (EOCD sentinels, the zip64 locator, or any `0x0001` extra field) is
  **not supported** and is rejected, never misparsed;
* multi-disk archives are rejected;
* encryption (general purpose flag bit 0) and data descriptors (bit 3) are
  rejected;
* every local header must agree with its central directory record on the name,
  method, sizes and CRC-32, and its data must lie inside the archive and before
  the central directory;
* the end-of-central-directory record is found by scanning backwards over at
  most **64 KiB + 22 bytes** of trailing comment, and only a record whose
  declared comment length reaches the end of the archive is accepted.

### Path safety

Entry names are relative, `/`-separated UTF-8 paths. A name is rejected if it
is empty, longer than 255 bytes, non-UTF-8, contains a NUL or any control
character, contains a backslash, has a drive letter (`C:`), is absolute
(leading `/`), or has a `.` or `..` component. A single trailing `/` marks a
plain directory, which must carry no data. Exact duplicate names are rejected,
and so are two names that differ only in case (the target filesystem may fold
case).

---

## 2. Limits

Mirrored by the public constants in `libs/lazypkg/src/lib.rs`; every one is
checked before any allocation.

| Constant | Value | Meaning |
|---|---|---|
| `MAX_ENTRIES` | 1024 | most entries in a package |
| `MAX_TOTAL_UNCOMPRESSED` | 64 MiB | most bytes all entries may expand to |
| `MAX_ENTRY_UNCOMPRESSED` | 16 MiB | most bytes one entry may expand to |
| `MAX_NAME_LEN` | 255 | longest entry name, in bytes |
| `MAX_MANIFEST` | 1 MiB | largest `manifest.toml` |

A zip bomb or a claimed 4 GiB entry is therefore refused cheaply: the declared
sizes in the central directory are checked before any data is read.

---

## 3. `manifest.toml`

The manifest is parsed with the `toml` crate into `#[serde(deny_unknown_fields)]`
structs, so a typo is an error rather than a silently ignored field. Parsing
tolerates a leading UTF-8 BOM and CRLF line endings, and an empty file is an
error (never a panic). The document must be valid UTF-8 and at most 1 MiB.

```toml
[app]
name = "Paint"                       # display name, 1..64 chars, no control chars
system_name = "org.lazy.paint"       # reverse-DNS id; the install dir and security label derive from it
author = "Valerian"                  # plain string, unverified, 1..128 chars
version = "1.2.0"                    # see "Versions" below
description = "A tiny raster painter"   # optional, at most 1024 chars
category = "graphics"                # optional menu group, default "accessories"

[entry]
binary = "bin/paint.elf"             # must exist in the archive
args = []                            # optional list of strings, each at most 256 bytes, at most 16 of them
abi = "native"                       # optional: "native" (default, a LazyOS program) or "linux" (a static musl program)
autostart = false                    # optional: start the app when the user logs in (default false)

[[mime]]                             # zero or more
type = "image/png"
verbs = ["open", "edit"]             # non-empty, each verb [a-z]+ of at most 16 chars
icon = "icons/png"                   # optional prefix: icons/png-16.png, -32 and -128 must then all exist

[permissions]
interfaces = ["os.lazy.clipboard.v1", "os.lazy.fs.reader.v1"]
topics = ["publish:app/org.lazy.paint/#", "subscribe:system/events/open/+"]
files = ["read:$HOME/Pictures/*", "write:$HOME/.apps/org.lazy.paint/*"]
network = []                         # v1: empty or ["outbound"]
```

`[app]`, `[entry]` and their required fields must be present. `[[mime]]` and
`[permissions]` are optional and default to empty. Semantic validation runs in
a separate pass that returns **every** problem as a list, so an installer can
show them all at once.

### Field grammar

* **`system_name`** — reverse-DNS: lowercase ASCII letters, digits and `-`
  inside labels, labels separated by `.`, at least three labels, no label
  starting or ending with `-`, at most 128 bytes.
* **`version`** — a [version](#versions): two to four dot-separated numbers,
  optionally followed by `-` and a pre-release (`1.2.0`, `1.2`, `2.0.0-rc.1`).
* **`category`** — absent or one of `accessories`, `development`, `graphics`,
  `internet`, `office`, `system`, `utilities` (lowercase). Absent means
  `accessories`; anything else is an error. The menu groups apps by it.
* **MIME `type`** — `type/subtype` using `[a-z0-9.+-]` only.
* **`interfaces`** — each matches `[a-z0-9]+(\.[a-z0-9]+)*\.v[0-9]+`.
* **`topics`** — `publish:` or `subscribe:` followed by `/`-separated segments
  of `[a-z0-9_.-]+`, `+`, or a final `#`.
* **`files`** — `read:` or `write:` followed by a path whose segments are
  `[A-Za-z0-9_.-]+` or `*`, with no `..`. The path is absolute (`/...`) or
  starts with `$HOME/`, the home directory of the user running the app.
  `$HOME` may only be the **first** segment: `read:$HOME/Documents/*` is fine,
  `read:/x/$HOME/y`, `read:$HOME/$HOME/y` and a bare `read:$HOME` are errors.
  An app's own per-user folder is `$HOME/.apps/<system_name>/`. Absolute paths
  under a home directory (`/home/...`) are still accepted;
  the F5 cleanup turns them into errors pointing at `$HOME` (one switch,
  `REJECT_ABSOLUTE_HOME`, in `libs/lazypkg/src/files.rs` and
  `tools/pkg/pkgmanifest.py`).
* **`network`** — empty or exactly `["outbound"]`.
* **`entry.abi`** — absent, `native` or `linux`. The ELF header cannot tell a LazyOS
  program from a static musl one (both are static x86_64 executables), so the
  package says which personality `init` must start it under (`spawnv`'s Linux personality).
* **`entry.autostart`** — absent or a boolean, default `false`. A user package
  that sets it shows "starts when you log in" on the consent screen and is
  started only after consent.
* **`entry.binary`** and every MIME `icon` prefix must resolve to files in the
  archive.

### Versions

`lazypkg::Version` (`libs/lazypkg/src/version.rs`) parses and orders versions;
`tools/pkg/pkgmanifest.py` has the same rules.

```text
version  = core [ "-" pre ]
core     = number ( "." number ){1,3}        two to four numbers
number   = "0" | [1-9][0-9]*                 below 65536, no leading zero
pre      = ident ( "." ident )*
ident    = [0-9A-Za-z-]+                     an all-digit ident has no leading zero
```

A version is at most 64 bytes. There is no `+build` part. The ordering is
semver's:

* numbers compare as numbers: `1.10 > 1.9`;
* a missing number counts as `0`, so **`1.0` and `1.0.0` are the same
  version** (neither is an upgrade of the other), although the install
  directory keeps the text as written;
* a pre-release comes before its release: `1.0.0-rc1 < 1.0.0`;
* two pre-releases compare identifier by identifier (semver §11): numbers as
  numbers, other identifiers in ASCII order, numbers before other identifiers,
  and a shorter list before a longer one that starts with it
  (`1.0.0-alpha < 1.0.0-alpha.1 < 1.0.0-beta < 1.0.0-beta.2 < 1.0.0-beta.11 < 1.0.0-rc.1`).

The manifest and version cases both validators must agree on live in
`libs/lazypkg/tests/cases/manifest.toml`; `cargo test -p lazypkg` and
`python tools/pkg/test_build.py` both run them. A new rule gets a case there.

---

## 4. The public API

```rust
pub struct Package<'a> { /* validated archive plus parsed manifest */ }
pub struct Manifest { pub app: App, pub entry: Entry, pub mime: Vec<MimeHandler>, pub permissions: Permissions }
pub struct EntryInfo<'a> { pub name: &'a str, pub size: u32, pub compressed_size: u32, pub crc32: u32, pub is_dir: bool }

impl<'a> Package<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Package<'a>, OpenError>;
    pub fn manifest(&self) -> &Manifest;
    pub fn entries(&self) -> impl Iterator<Item = EntryInfo<'a>> + '_;
    pub fn read(&self, name: &str) -> Result<alloc::vec::Vec<u8>, ReadError>;
    pub fn digest(&self) -> [u8; 32];        // SHA-256 of the whole archive
    pub fn install_dir(&self) -> alloc::string::String;
}

/// Parse and validate a stored `manifest.toml` outside an archive (file
/// existence is not judged); `pkgd` uses it to rebuild an installed app's
/// policy and MIME registrations at boot.
pub fn parse_manifest(text: &str) -> Result<Manifest, ManifestError>;

impl App {
    pub fn category(&self) -> Category;              // absent: Category::Accessories
    pub fn parsed_version(&self) -> Option<Version>; // Some for every validated manifest
}
// `Entry::autostart: bool` is a plain field (absent: false).

pub enum Category { Accessories, Development, Graphics, Internet, Office, System, Utilities }
pub struct Version { /* parsed text; Ord/Eq as in "Versions" above */ }
impl Version {
    pub fn parse(text: &str) -> Result<Version, VersionError>;
    pub fn as_str(&self) -> &str;
    pub fn prerelease(&self) -> Option<&str>;
}
```

`Package::open` returns either a fully validated package or an error; no
partially filled state is exposed. `read` verifies the entry's size and CRC-32
against the central directory.

`install_dir` is `<system_name>/<version>-<first 8 lowercase hex chars of
digest>`, the install directory relative to `/apps`.

### Errors

Every error is an `enum` with a one-line `Display`, because the installer shows
it to a user. Examples:

```text
not a zip archive: no end-of-central-directory record
zip64 archives are not supported
entry "bin/../x" contains a `.` or `..` component
entry "bin/app.elf" differs only in case from another entry
too many entries: 1025 (maximum 1024)
the package expands to 67108864 bytes (maximum 67108864)
manifest: app.system_name "Bad" is not a reverse-DNS name; app.version "1" must have two to four numbers
```

Manifest problems are collected in `OpenError::Manifest(ManifestError)`, whose
`problems()` lists each one.

---

## 5. Building a package

```bash
python tools/pkg/build.py path/to/tree --out dist
# -> dist/org.lazy.paint-1.2.0.lzp
```

The source tree is exactly the layout in §1. The builder runs the same
structural and manifest checks as the reader and fails with a list of every
problem it found, so mistakes are caught before the OS ever sees the archive.
See [`../tools/pkg/README.md`](../tools/pkg/README.md).

## 6. Fuzzing

`libs/lazypkg` exposes `fuzz::run` behind the `fuzz` feature: it opens the input
as a package and, on success, reads every entry and calls `digest` and
`install_dir`. It must never panic. The seeded tests (mutated valid archives,
random bytes, and every truncation of a valid archive) run under plain
`cargo test`; `fuzz/fuzz_targets/lazypkg.rs` is the libFuzzer target and
`fuzz/seeds/lazypkg/` the checked-in corpus. See
[`../tools/pkg/README.md`](../tools/pkg/README.md).

---

## 7. Installing: `pkgd`

`pkgd` (`user/src/bin/pkgd.rs`, `/system/bin/pkgd`) is the only task that installs and
removes applications. It serves `os.lazy.pkgd.v1` ([`idl/pkgd.midl`](../idl/pkgd.midl),
[reference](idl/os.lazy.pkgd.v1.md)) and `init` starts it after `confd` and
`mimed`. Its pure logic (policy compilation, the permission explanation table,
the audit chain, install paths, who may ask for what) is `libs/pkgstore`, which
has host tests; the service is the thin syscall layer on top.

`pkgctl` (`/system/bin/pkgctl`) is its command line, and makes the same calls a GUI
installer does:

```text
pkgctl inspect /system/share/samples/pkgdemo.lzp     # what it declares and asks for; changes nothing
pkgctl install /system/share/samples/pkgdemo.lzp
pkgctl remove org.lazy.counter
pkgctl list
```

### Where things live

| What | Where |
|---|---|
| The extracted package | `/apps/<system_name>/<version>-<digest8>/` (the `install_dir`), `manifest.toml` included as received |
| Its documentation | `/docs/apps/<system_name>/`: the package's `docs/**.md`, listed by the Docs app next to `/docs/os` |
| One record per installed app | `confd` key `sys/apps/<system_name>`: the generated `Installed` record, encoded once |
| The app's policy | the kernel, label `app:<system_name>` (`acl_load`); memory only, so replayed at startup |
| The app's file types | `mimed`, registered with the app id `<system_name>` |
| The audit trail | `/logs/pkg.log`, plus `system/events/pkg/<op>` events; `logd`'s rotation and budget leave it alone |

All three directories are on the ext2 OS volume (`docs/architecture/filesystem.md`):
`/apps` and `/docs/apps` are 0755 root, written only by `pkgd`; `/logs` is 0750
root. At startup `pkgd` probes that it can create and write in each (as `confd`
probes `/conf`); when it cannot (a recovery boot with a read-only `/`) it still
answers `Inspect` and `List` and refuses `Install` with "Applications cannot be
installed: <why>" (`PKGD:STORE:ABSENT reason="<why>"` on serial). Nothing is
written under `/data` any more; apps an F3 image installed in `/data/apps` are
not listed until F7 migrates them.

The documentation is copied whole: on install `pkgd` writes it to
`/docs/apps/<system_name>~new` together with the extraction, and only once the
app is active does it replace the live directory (live → `<system_name>~old`,
`~new` → live, then the old tree is deleted), so the pages never describe a
half-upgrade. `~` and not `.new`, because `.new` is a valid `system_name` label.
A stop between those steps is repaired at the next start
(`PKGD:DOCS:REPAIRED n=<k>`): a complete `~new` copy becomes live, a partial one
is deleted. Removal deletes the directory; every deletion is confined to
strictly below `/apps` or `/docs/apps` (`pkgstore::tree::deletable`). The same
`pkgstore::tree` code runs in `pkgd`, in the host tests (an in-memory tree, and
1 000 install/upgrade/remove cycles on `libs/ext2fs` with its fsck checker) and
in the kernel suite (`ext2_suite::pkg_tree`, the same soak through the VFS).

### `Inspect` and `Install`

`Inspect(path)` reads the whole file (at most **8 MiB**: the kernel reads a file
into its 16 MiB heap to serve the read; the package may still expand to
`MAX_TOTAL_UNCOMPRESSED`), opens it with `lazypkg`, and fills `PackageInfo`. A
package that fails validation is *not* an error: every problem is in
`PackageInfo.problems`, so an installer can list them all. Permissions are
expanded through the explanation table (`pkgstore::explain`, keyed by MIDL
interface name; a test fails when `idl/` declares an interface the table does
not know): one entry per interface, topic, file rule and `network` entry, each
with a risk (`low`/`medium`/`high`) and a one-sentence explanation. An interface
the table does not know is `high`: "An interface this system does not know:
<name>". A file rule's risk depends on where it points: the app's own
`$HOME/.apps/<system_name>/` folder is `low` to read or write; other personal
data (anything else under `$HOME`, or under `/home`) is `low` to read and
`medium` to write; anywhere else is `medium` to read and `high` to write. At most 24 permission entries are allowed, so the consent screen can
list every one of them.

`Install(path)` never trusts an earlier `Inspect`: it re-checks the caller,
re-reads and re-validates the package, then **builds everything before
switching**:

1. extract every entry under the new install directory (directories first, each
   file written, its size verified and its mode set with the native `chmod`,
   syscall 31: `0755` for files under `bin/`, `0644` for everything else,
   because native spawn needs an `x` bit, root included; see
   `pkgstore::layout::file_mode`);
2. record the `Installed` row in `confd`;
3. register every `[[mime]]` verb with `mimed` (`Register(mime, <system_name>, verb)`);
4. compile and load the policy (`load_label("app:<system_name>", rules)`);
5. append the audit record and publish `system/events/pkg/install`.

A failure at any step undoes the earlier ones and says which step failed
("Installing failed while writing bin/app.elf: no space left"). Installing the
same `system_name` at the same digest is refused; a different digest is an
**upgrade**: the new directory is installed beside the old one, the row and
policy switch, and only then is the old directory deleted (and the file types the
new version no longer handles are withdrawn). `Remove(system_name)` asks `init`
to stop every running instance (`init.Stop`), withdraws the file types
(`mimed.Unregister`, which hands a type back to the handler it replaced), revokes
the policy (an empty `load_label`), deletes the install directory and the record,
and audits it. User data under `/home` is never touched.

Files larger than the 1 MiB a single `write_file` takes are written as one
`write_file` plus `append_file` (native syscall 28) per further MiB.

### Privilege and who may ask

`pkgd` is spawned by `init` with `init`'s identity: **root with every capability
but raw input**. The privilege is needed and is the reason this is one small
service: `CAP_IPC_CONTROL` for the kernel's `acl_load`, uid 0 for `/apps`,
`/docs/apps`, `/logs/pkg.log` and the `sys/` part of `confd`, and uid 0 for `mimed`'s `Register`/`Unregister`.
Because it is root, it checks every request against the kernel-stamped identity
of the sender (`pkgstore::access`):

* only root or the owner of a login session may `Install` or `Remove`; a task
  carrying an app label never may;
* it reads a package *as root*, so for an unprivileged caller it only accepts
  paths that are readable by design:

  ```text
  allowed = under(path, /transient) || under(path, caller_home) || caller_uid == 0
  ```

  `caller_home` is the caller's home from `accountsd`'s `Lookup`. The path is
  normalised first (`//`, `.` and `..` folded; ext2 has no symlinks) and that
  normalised path is the one read, so `/home/user/../admin/x.lzp` is judged as
  `/home/admin/x.lzp`. Anything else is refused with "packages can only be
  installed from /transient or your home folder"; root may name any absolute
  path (`pkgctl install /system/share/samples/pkgdemo.lzp` as root works, a
  user copies the sample to `/transient` first). There is no "open as uid"
  call, and without this a user could install, and so copy out into
  world-readable `/apps`, a package they cannot read;
* refusals are answered with a structured error (errno-style code plus a
  friendly sentence) and audited as `denied`.

### What a manifest compiles to

`pkgstore::rules::compile` is the one function from manifest to kernel rules, so
the consent screen and the enforcement come from the same data:

* an `interfaces` entry allows every method of the interface and the
  *resolution* of each name it is served under (the interface name, the name
  without `.vN`, and the service name where that differs, e.g.
  `os.lazy.accounts.v1` is served as `os.lazy.accountsd`);
* a `publish:`/`subscribe:` topic allows each *segment* of the pattern for that
  direction (the kernel authorizes a topic segment by segment, so segments
  granted for different topics combine), plus the topics broker's name and the
  methods the direction needs; the app's own `app/<system_name>/` namespace needs
  no rule;
* `network = ["outbound"]` allows the socket interface of the network stack;
* `files` rules are **consent only** today: there is no file sandbox in the
  kernel yet, so they are recorded and shown but compile to nothing.

A label carries at most 256 rules; a manifest that needs more is refused.

### Audit

Every install, removal and refusal is one line of `/logs/pkg.log`:

```text
<seq> <prev_hash_hex> <hex of the PkgEvent bytes> <sha256_hex over prev_hash||event>
```

`seq` starts at 1 and the first `prev_hash` is 32 zero bytes, so removing,
reordering or editing a line breaks the chain from there on. At startup `pkgd`
verifies the chain and prints `PKGD:AUDIT:PASS n=<count>`; a broken log prints
`PKGD:AUDIT:FAIL <why>`, is kept aside as `pkg.log.bad-<ticks>`, and a new chain
starts with a record that says so. Every event is also published on
`system/events/pkg/<op>` (`install`, `remove`, `denied`), which `logd` retains
independently of the file. The chain is tamper-*evident*: someone who can rewrite
the whole file can rebuild it, which is what the published copy is for.

Every record is appended before its request is answered, and `pkgd` serves
`os.lazy.lifecycle.v1` (`docs/shutdown.md`): on `init`'s `Shutdown` it fsyncs
`pkg.log`, prints `PKGD:STOP sync=<ok|none|errno>` and exits, so a power-off
never loses the tail of the chain to `SIGTERM`. It is stopped before `confd`
and `mimed`, which it depends on.

### Boot, `init` and the menu

The kernel's policy and `mimed`'s registrations live in memory, so at startup
`pkgd` replays every installed app (its stored `manifest.toml`, parsed with
`lazypkg::parse_manifest`) through the same activation an install uses
(`PKGD:RECONCILE:PASS`).

`init` lists installed apps from `confd`, not through `pkgd`: `Remove` calls
`init.Stop`, and `init` is a single task, so `init` waiting on `pkgd` while `pkgd`
waits on `init` would deadlock. They follow the built-ins in `ListApps`
(`AppInfo.installed`; the id is the `system_name`), re-read on every call, and
`Launch(<system_name>)` spawns `/apps/<install_dir>/<binary>` under the
label `app:<system_name>`, with the manifest's `entry.args`, in the Linux
personality when `entry.abi = "linux"`, as the launching session's user with no
capabilities. A restarted app is stamped again, so a crash does not launder the
sandbox. `init` prints `PKGD:LAUNCH:LABEL app:<system_name> pid=<n>`, the label
read back from the kernel. The desktop right-click menu appends the installed
apps after its configured entries, re-read each time it opens.

### Sample package and end-to-end check

`tools/pkg/samples/counter/` is the Counter demo as a package
(`system_name = "org.lazy.counter"`, `abi = "linux"`, requesting exactly the two
interfaces the app resolves, `os.lazy.display.v1` and `os.lazy.input.v1`).
`python tools/pkg/build_samples.py` (run by `tools/xui/build.py`) builds it into
`target/pkg/PKGDEMO.LZP`, which the root `build.rs` embeds as
`/system/share/samples/pkgdemo.lzp`.
`tools/pkg/make_icons.py` generates its icons. The visual check is
`tools/screenshot/examples/pkg_install.json`.

### Limits worth knowing

* The package file limit is 8 MiB and the user heap never returns blocks over
  64 KiB, so `pkgd` restarts itself (`PKGD:RECYCLE`, `init` starts a new one) once
  its heap has grown by 32 MiB and it is idle. A client that connects during that
  moment retries (`pkgctl` does).
* Topic permissions are per segment (see above) and file permissions are not
  enforced; both are the kernel's current granularity, not the compiler's.
* An upgrade does not stop running instances; they keep running from memory and
  the next launch uses the new version.

## 8. The installer app

The GUI installer (`xui-installer`, `xui-app/src/bin/installer.rs`) is an
unprivileged `pkgd` client. It opens on the installed list: one row per app
(name, version, system name) with a `Remove` button, an "open a package" text
field and an `Inspect` button (there is no file picker in v1). Opening a `.lzp`
from Files or the desktop passes its path as the app's argument, so the consent
screen appears immediately.

The consent screen is the whole point: it shows the package's name, version and
`Author (unverified)`, its description, the MIME types it handles, and its
requested permissions **grouped by risk (high first)** with the friendly
explanation `pkgd` supplied. The install directory and the short archive digest
are shown too, so the same archive can be recognised later. `Install` forwards
the user's yes to `pkgd`; `Cancel` (or `Esc`) returns to the list. When
`PackageInfo.problems` is non-empty the package cannot be installed, so the
screen lists every problem and offers only `Close`.

Everything the package declares is untrusted: the installer strips control
characters and elides long names, explanations and problems before showing
them, and it never treats an `Inspect`/`Install` reply as trusted. A missing
`pkgd` is a friendly banner, not a crash, and a package is only ever shown as
installed after `pkgd`'s `Install` reply confirms it.

Keyboard and mouse both reach every action: `Tab` cycles focus, `Enter`
activates, `Esc` cancels/backs out, and `q` quits from the list. A screenshot
session can follow the flow from the serial markers, one per line:

```text
INSTALLER:UP:PASS
INSTALLER:LIST:PASS count=<n>            INSTALLER:LIST:FAIL <reason>
INSTALLER:INSPECT:PASS <system_name>     INSTALLER:INSPECT:FAIL <reason>
INSTALLER:CONSENT:SHOWN perms=<n> problems=<n>
INSTALLER:INSTALL:PASS <system_name>     INSTALLER:INSTALL:FAIL <reason>
INSTALLER:REMOVE:PASS <system_name>      INSTALLER:REMOVE:FAIL <reason>
```

`tools/screenshot/examples/xui_installer.json` waits for `INSTALLER:UP:PASS`,
captures the list and quits; the install flow is asserted once `pkgd` ships.
