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
version = "1.2.0"
description = "A tiny raster painter"   # optional, at most 1024 chars

[entry]
binary = "bin/paint.elf"             # must exist in the archive
args = []                            # optional list of strings, each at most 256 bytes, at most 16 of them

[[mime]]                             # zero or more
type = "image/png"
verbs = ["open", "edit"]             # non-empty, each verb [a-z]+ of at most 16 chars
icon = "icons/png"                   # optional prefix: icons/png-16.png, -32 and -128 must then all exist

[permissions]
interfaces = ["os.lazy.clipboard.v1", "os.lazy.fs.reader.v1"]
topics = ["publish:app/org.lazy.paint/#", "subscribe:system/events/open/+"]
files = ["read:/data/home/*/pictures", "write:/data/home/*/pictures"]
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
* **`version`** — `MAJOR.MINOR.PATCH`, each part unsigned decimal below 65536.
* **MIME `type`** — `type/subtype` using `[a-z0-9.+-]` only.
* **`interfaces`** — each matches `[a-z0-9]+(\.[a-z0-9]+)*\.v[0-9]+`.
* **`topics`** — `publish:` or `subscribe:` followed by `/`-separated segments
  of `[a-z0-9_.-]+`, `+`, or a final `#`.
* **`files`** — `read:` or `write:` followed by an absolute path whose segments
  are `[A-Za-z0-9_.-]+` or `*`, with no `..`.
* **`network`** — empty or exactly `["outbound"]`.
* **`entry.binary`** and every MIME `icon` prefix must resolve to files in the
  archive.

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
```

`Package::open` returns either a fully validated package or an error; no
partially filled state is exposed. `read` verifies the entry's size and CRC-32
against the central directory.

`install_dir` is `<system_name>/<version>-<first 8 lowercase hex chars of
digest>`, the install directory relative to `/data/apps`.

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
manifest: app.system_name "Bad" is not a reverse-DNS name; app.version "1" must be ...
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
