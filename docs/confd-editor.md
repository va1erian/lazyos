# Config app (confd editor)

A generic, registry-style editor for the `confd` configuration space: browse the
whole key tree, then view, edit, create or delete any key. It is the counterpart
to the fixed-schema [Settings app](settings-app-plan.md): Settings knows its
`sys/ui/*` and `sys/input/*` keys and presents them as purpose-built pages,
while Config knows nothing about any key and works from the tree `List` returns.

## Scope

| Concern | Mechanism |
|---|---|
| Scope | the user's own keys (`List("user/<uid>")`) until **Elevate** (docs/accounts-plan.md U2): `elevd` asks an administrator on the trusted prompt, then every key is listed, read and written through `elevd` (`conf.*` operations; the approval stands five minutes) |
| Browse | `List(<scope>)` fills a folder tree; a leaf's value is `Get` lazily when selected |
| Edit | kind picker (bool, i64, u64, string, bytes) + a value field; `Set` on Apply |
| Create | path + kind + value, validated with `confd::validate_path` |
| Delete | `Delete`, behind a two-click confirmation |
| Errors | the `CONFD_*` codes shown verbatim; `DENIED` becomes a read-only state |
| Persistence | `Info()`; a non-persistent store shows a visible banner |

Paths, the `sys/`/`user/<uid>` access rules, the 256-byte path / 4 KiB value /
1 MiB store limits and the typed `Value` are all `confd`'s contract; the app
reuses `libs/confd` (`validate_path`, the limits, `Value`) client-side instead of
re-implementing them. A missing key is an absent value (`Ok(None)`), not an
error: `confd` seeds nothing, so a fresh store shows an empty tree and the app
offers **New key**.

## Architecture

`xui-app/crates/confd-editor` (`xui-confd-editor`) is a portable,
`#![forbid(unsafe_code)]` crate; `xui-app/src/bin/confd_editor.rs` is the thin
binary (`xui-confd`) that supplies the LazyOS platform, and
`xui-app/src/platform/confd_store.rs` implements the crate's `ConfStore` trait
over `os.lazy.confd` with the generated `messenger-generated` stubs (no
hand-written method ids or TLV encoders).

| Module | Responsibility |
|---|---|
| `store.rs` | `ConfStore` trait, `StoreError` (the `CONFD_*` codes + `Transport`), `StoreInfo`, and the `MemStore` fake |
| `tree.rs` | pure flat-paths → folder-tree projection, expand state keyed by full path, filter + flattened rows |
| `value_edit.rs` | pure per-kind parse/format/preview; the 4 KiB limit is enforced in **bytes** |
| `sections/mod.rs` (tests in `sections/tests.rs`) | the right-pane state machines: `KeyEditor` (lazy load, dirty, two-step delete, external-change) and `NewKeyEditor` (validated create, two-step overwrite) |
| `app.rs` | the ~720×500 window: left `ListView` tree + filter, right kind/value/buttons |

**Selection is path-tied.** The edit buffer is only ever loaded for the selected
path and is reset when the selection changes, so a buffer can never be applied
to a different key. **Delete and overwrite are two-click.** The first click arms
the button ("Confirm delete"/"Confirm create"); any other action cancels it.
**Kind changes re-parse.** Changing a key's kind re-parses the current text and
keeps the old kind and value if it does not fit. **Revert** restores exactly the
stored value. **Refresh** re-lists the tree (keeping the previous one on a
`List` error) and re-reads the selected key; if the store changed under an
unsaved buffer it flags "changed externally" and leaves the edits alone until
**Reload** is clicked deliberately.

## Live refresh

`confd` announces committed `sys/` changes on the
`system/confd/changed/sys/#` topic, but the xui event loop has no topic
subscription path (the app is request/reply only), so this round implements
**manual Refresh only**; it does not half-wire a subscription. A future round
could add a broker-backed receive loop and feed the same `KeyEditor::refresh`
path.

## Registration

`xui-app/Cargo.toml` (`[[bin]] name = "xui-confd"`, workspace member and
dependency), `tools/xui/build.py` (`xui-confd.elf`), `build.rs` (`confd` →
`XCONFD`, in `DOCUMENT_XUI_APPS`, so it ships on demand and is not
autostarted), `user/src/bin/init/apps.rs` (`xui_app("confd", "Config",
"/system/bin/confd-editor")`); the desktop menu's built-in list is `deskmenu::defaults()`
(`libs/deskmenu`), where `("confd", "Config")` goes last so the rows the
screenshot sessions click by coordinate keep their positions.

## Verification

- Host: `cargo test --manifest-path xui-app/Cargo.toml --workspace --lib` (tree
  build and filter, expand survival, per-kind parse edge cases including
  overflow/hex/odd-length hex/byte-vs-char limits/non-UTF-8, kind-change keep,
  apply/delete failure atomicity, read-only `DENIED`, external change vs dirty,
  new-key validation/clobber, delete cancel, list-error tree retention).
- Kernel/confd: `cargo test -p confd` is unaffected (no `libs/confd` change).
- Visual: `tools/screenshot/examples/xui_confd.json` (an image built with
  `LAZYOS_DESKTOP=1 LAZYOS_AUTOLOGIN=user LAZYOS_UI_PROBE=1` and no app at
  boot) — opens Config from the desktop menu (by name, `UI:RECT` probes),
  which lists only `user/1000/**`; creates the user key `user/1000/demo`;
  clicks **Elevate**, types `admin`/`nimda` on the trusted prompt
  (`CONFDED:ELEVATE:PASS`); creates `sys/ui/demo` (a scratch key that sorts
  before the seeded `sys/ui/menu`, so the session never edits a real
  setting) through `elevd`; filters the tree down to it, selects the key,
  edits the value to `light` and applies it, then closes. Serial markers:
  `CONFDED:UP:PASS`, `CONFDED:MSG:<Msg>`, `CONFDED:ELEVATE:PASS`,
  `CONFDED:CLOSE:PASS`, `ELEVD:REQUEST op=conf.* ... outcome=granted`. Its
  window offsets assume Config is the only open window.
- CI: clippy `-D warnings`, `cargo fmt`.

## Open risks

- No topic subscription yet (see above), so an external change is only noticed
  on Refresh.
- The create pane is a single path field; it does not pre-fill from the current
  selection, and there is no inline validation feedback beyond the status line.
- `user/<uid>/**` paths are only editable by their owner (or a system
  service), and `sys/**` only by a system service (`CAP_SETUID`, issue #623).
  The app runs in the user's desktop session, so it edits that user's keys and
  shows the rest as read-only (`DENIED`).
- The tree draws no icons and has no keyboard navigation; it relies on the
  `ListView`'s own selection and scrolling.
- The window is fixed at 720×500; long paths are ellipsised by the label.
