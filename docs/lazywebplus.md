# LazyWeb+, LazyWeb's browser on WebKit

`os.lazy.lazywebplus` ("LazyWeb+" in the menu) is LazyWeb's window, menus, history and downloads on the
WebKit engine of [`webkit-port-plan.md`](webkit-port-plan.md): JavaScript, forms, cookies, the modern web, in
place of Blitz. It is a second browser, not a replacement: LazyWeb (Blitz, no script engine, 12 MB of
memory, in every `LAZYOS_LAZYWEB=1` image) stays untouched and stays the browser an image ships by
default. Issue #835.

```bash
python tools/run_demo.py --lazywebplus    # desktop + networking + HTTPS + the browser (builds WebKit first: Docker)
python tools/webplus/run.py               # build, browse the stand-in sites twice, judge (tools/webplus/README.md)
cd xui-app && cargo test -p xui-webplus -p xui-webkit   # the window and the seam, offscreen, host
python tools/webplus/test_judge.py        # the judge fails when it should; the sessions are current
```

## What it is

One C++ program, the UI process of the W5 browser (`webkit/port/app/`), with LazyWeb's window as the Rust
static library it links (`xui-app/webplus`, crate `xui-webplus`): `lazywebplus-app`, 108 MiB stripped, which
starts `/system/bin/webprocess` and `/system/bin/networkprocess`. The package is
`xui-app/packages/lazywebplus` (core package, `LAZYOS_LAZYWEBPLUS=1`, which needs `LAZYOS_DESKTOP=1` and
`LAZYOS_NETD=1`; the OS volume defaults to 1 GiB). The W5 browser (`os.lazy.webkit`, `LAZYOS_WEBKIT=1`) is
the same engine with a bare window; both programs come from one C++ source, and
`lzw_ui_features()` tells the UI process which of the new behaviours the window it links wants (the W5
window asks for none, so it behaves as before).

The window is a fork of `xui-app/web`'s LazyWeb on the engine seam of `xui-app/crates/webkit` (`xui-webkit`:
the `Engine` trait, the mailbox between the two threads, the generic page view, the pure ABI helpers).
`xui-webkit` is built with its `window` feature for the W5 browser and without it for LazyWeb+, which
defines the per-window symbols the C++ side calls itself (`lzw_ui_main`, `lzw_ui_features`,
`lzw_ui_marker_prefix`, `lzw_ui_profile_dir`, `lzw_ui_navigate`, `lzw_ui_download_start`, ...).

LazyWeb's look is kept: the menu bar (File, View, History, Downloads, Help), the toolbar (Back, Forward,
Reload which turns into Stop, Home, the address field, an "open in the OS" button and the throbber), the
status bar (hover link, what the engine is doing, running downloads with a progress bar, the padlock) and the
same keys (Ctrl+L, F5, Alt+Left, Alt+Right, Alt+Home, Ctrl+H, Ctrl+J, Ctrl+S, Ctrl+W, Esc to stop while a page
loads: Enter and Esc are the address field's only while it has the focus, because a real engine has pages
that use them). The window title is `<page title> - LazyWeb+`.

## The ABI the engine needed

Everything from W5's sixteen functions is kept; the additions are what an engine with scripts needs and Blitz
did not (all in `webkit/port/app/lazyos_ui.h`, each mirrored in `xui-app/crates/webkit/src/{abi,ffi,events}.rs`
with host tests and in the recording engine; the table with the WebKit side is in
[`webkit/README.md`](../webkit/README.md)):

| Need | ABI |
|---|---|
| Features, marker prefix, profile directory | `lzw_ui_features`, `lzw_ui_marker_prefix`, `lzw_ui_profile_dir` |
| Committed URL, the padlock, how a load ended | `lzw_ui_state` grows `committed_url`, `secure`, `finished` |
| Link under the pointer, pointer shape | `lzw_ui_hover`, `lzw_ui_cursor` (patch `0015` teaches WebKit's PlayStation view the I-beam, wait and resize arrows) |
| Navigation policy | `lzw_ui_navigate(url, kind, user_gesture, committed_url)` |
| Downloads | `lzw_ui_download_start`, `_progress`, `_done`, `_fail`; `lzw_engine_download`, `lzw_engine_download_cancel` |
| `alert`, `confirm`, `prompt` | `lzw_ui_dialog` (returns at once), `lzw_engine_dialog_reply` |
| `window.open`, `target=_blank` | `lzw_ui_new_window` |

## Navigation policy: a script never starts anything

LazyWeb could take it for granted that every launch it reports is the user's, because Blitz has no scripts.
WebKit cannot. Every navigation, redirect and window request goes through `decidePolicyForNavigationAction`
to `policy::decide(url, kind, user_gesture, from_own_page)` (`xui-app/webplus/src/policy.rs`, pure and
host-tested), with the gesture flag from `WKNavigationActionHasUnconsumedUserGesture` (a click or a key, not
a timer, a load event or a redirect):

| URL | Decision |
|---|---|
| `http:`, `https:`, `file:`, `data:`, `blob:`, `about:` | the engine loads it |
| `x-lazyweb:` (the browser's own commands) | runs only from one of the browser's own pages, for a click on a link there, and never goes to another app; anything else is refused |
| `mailto:`, `tel:`, any other scheme | handed to `mimed` only when a user gesture started it; otherwise refused and reported (`WEBPLUS:LAUNCH:<url>:BLOCKED`) |

An address the user types is opened by the window itself (`handoff.rs`), so a navigation the UI process
started to `mailto:` with no gesture (kind `Other`) is a script's, a redirect's or a bug's, and is refused.
The browser's own pages are `data:` URLs built in the window; "on one of the browser's own pages" is an exact
comparison with the page the window built (`Internal::name_for`), so a web page that looks like
`about:history` and links to `x-lazyweb:clear-history` is not one. `window.open` and `target=_blank` load in
the one view, and only when a gesture started the request (`WEBPLUS:POPUP:<url>`); a script that opens a
window on its own gets nothing. The harness page `webplus/script.html` tries a `mailto:`, an
`x-lazyweb:clear-history` and a `window.open` with no click, and the judge requires all three to be refused.

## Pages

`about:history`, `about:downloads` and `about:lazyweb` and the start page are HTML built in the window and
shown as `data:` URLs (so there is one rendering path), reached by the menu, Ctrl+H and Ctrl+J. History is
`$HOME/.apps/os.lazy.lazywebplus/history.tsv` (the newest 1000 pages that finished loading over `http:`,
`https:` or `file:`), cleared from `about:history`.

## Downloads

A response the view cannot show, or one sent as an attachment, is downloaded: the response policy asks for a
download (`WKFramePolicyListenerDownload`), the window picks the destination (`Saver`: the name made safe,
unique among the files there and the downloads under way, ` (1)`, ` (2)`; the folder is made; nothing is
created in advance, because WebKit does not overwrite) and the NetworkProcess writes
`$HOME/Downloads/<name>.part`, renamed when the download completes and removed when it fails or is cancelled.
The size reported is the file's. The status bar shows the progress while it runs and `about:downloads` lists
the session's downloads with open and cancel links. File > Save Page As Download saves the current page the
same way. Each download also asks for a toast (`os.lazy.notify`); see *Known limits*.

## Dialogs

`alert`, `confirm` and `prompt` arrive on the UI client's V6 callbacks with a result listener. The UI process
keeps the listener, tells the window (`lzw_ui_dialog`, which returns at once: the frame mailbox is never
blocked) and answers when the window calls `lzw_engine_dialog_reply`, or abandons it when the page goes away.
The window draws them with xui's `Dialog::message/confirm/prompt` ("theoldnet.com says"), modal to the page
area.

## The profile

Cookies (`WKWebsiteDataStoreConfigurationSetCookieStorageFile`) and local storage live under
`$HOME/.apps/os.lazy.lazywebplus`, so a second run of the app finds the first one's cookie and history
(the harness checks both, from the OS volume and from the second boot). The network cache stays off: it maps
its files, and LazyOS has no file-backed `mmap`.

## `mimed`: which browser opens a link

`mimed` keeps one app per `(type, verb)` and the last to register wins, so with LazyWeb and LazyWeb+ both
registering `x-scheme-handler/http`, `x-scheme-handler/https` and `text/html`, the one installed last would
open every link. There was no preference, so one was added: the confd key `user/<uid>/mimed/default/<type>`
(a string holding an app id; `libs/deskmenu/src/defaults.rs`), written by **Settings > Default apps** (all three
types at once) and honoured by `mimed` for the uid the kernel stamped on the `Open`, only while that app still
holds a registration for the type, so a stale choice (an uninstalled browser) falls back to the registry.
`confd` lets a uid touch only `user/<uid>/**`, so one account's choice never moves another's. Evidence:
`MIME:PREFER:PASS` in `mimed`'s selftest, host tests in `deskmenu`. LazyWeb stays the default of an image that
ships both: LazyWeb+ is opt-in.

## Permissions

`os.lazy.display.v1`, `os.lazy.display.cursor.v1`, `os.lazy.input.v1`, `os.lazy.mimed.v1`,
`os.lazy.notify.v1`, outbound network, and `write:$HOME/.apps/os.lazy.lazywebplus` and
`write:$HOME/Downloads`, derived from a `--label-trace` run of the harness with no `LABEL:DENY`. The
WebProcess and NetworkProcess are children and run under the app's label. **The WebProcess is not sandboxed**
(W5's *Security* section stands): code that escapes the engine is not held to the label. Open only pages you
trust; LazyWeb is the browser LazyOS ships.

## Licence

`xui-webplus` and `xui-webkit` are declared `GPL-2.0-only OR GPL-3.0-or-later`. LazyWeb is `GPL-2.0-only`
(the licence NetSurf required), but this program links OpenSSL (Apache-2.0, through WebKit's curl), which a
GPL-2.0-only program cannot carry; offered under GPL-3.0-or-later as well, the combined work is distributed as
GPL-3.0-or-later and the window crates stay usable in LazyWeb's own tree. `python tools/nettls/licenses.py
--lazywebplus` checks the window's Cargo tree for GPLv2 compatibility with one waiver, `unicode-linebreak`
(Apache-2.0 only, in every xui app through `xui-canvas`), and `python tools/webkit/licenses.py` checks the
WebKit bundle's rows and that the window crates keep both licences and name no OpenSSL crate. `THIRD_PARTY.md`
has the rows. Choosing GPL-3.0-or-later for the program is a decision for the maintainer to confirm.

## Known limits

* **Toasts from the download thread are refused.** The shell's notification service names an app by the task
  id of the sender, and a message from a thread other than the process's first carries the thread's id, not the
  app's, so `SHELL:NOTIFY:DENY ... why=not-an-app` (the status bar and `about:downloads` carry the result; a
  toast is best effort, so nothing else is affected). A fix is in the shell (map a thread to its process), not
  in this app.
* The window and WebKit run on two threads of one process; the page area is drawn at device scale 1 (small
  text at 2x).
* No tabs, no find in page, no context menu, no pasteboard beyond WebKit's own, no IME, no WebGL, video or
  WebAssembly.
* The prompt dialog does not seed the page's default value.

## Testing

`tools/webplus/run.py` ([`tools/webplus/README.md`](../tools/webplus/README.md)) builds the image, serves
LazyWeb's two stand-in sites and the pages of `tools/web/fixtures/theoldnet.com/webplus/` from the host, boots
twice and judges the serial markers, the requests the host saw, the profile on the OS volume and the pixels.
The first boot browses, answers the three dialogs, tries the blocked launches, follows a `mailto:` link, a
`target=_blank` link and a `window.open` button, downloads two files, opens `about:history` and
`about:downloads` by key, scrolls, submits a form, stops a load, goes Back, maximizes the window and quits;
the second boot finds the cookie and the history, clears the history and quits clean.

RESULTS
