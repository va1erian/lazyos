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
| Link under the pointer, pointer shape | `lzw_ui_hover`, `lzw_ui_cursor` (patch `0016` teaches WebKit's PlayStation view the I-beam, wait and resize arrows) |
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

**A second lock.** The engine's gesture flag is one boolean from a large engine, and a click may authorise more than
one thing (a timer chain, a redirect), so a hand-off to another program also needs real input the window itself
forwarded to the page within the last three seconds, and uses it up (`xui_webkit::gesture`, `lzw_ui_navigate`): one
click, one hand-off. A hand-off without it is refused and reported like a script's.

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
same way. A download needs no gesture (an attachment is a response, not a click), so a page can start them: a
session is capped at 256 downloads, a name that is another download's `.part` is never reused, a file that
appeared at the final name meanwhile is never replaced (the download fails instead), invisible direction
characters are stripped from names, and with no home folder nothing is saved (the manifest lets the network
process write the Downloads folder of a home only). Each download also asks for a toast (`os.lazy.notify`);
see *Known limits*.

## Dialogs

`alert`, `confirm` and `prompt` arrive on the UI client's V6 callbacks with a result listener. The UI process
keeps the listener, tells the window (`lzw_ui_dialog`, which returns at once: the frame mailbox is never
blocked) and answers when the window calls `lzw_engine_dialog_reply`, or abandons it when the page goes away.
The window draws them with xui's `Dialog::message/confirm/prompt` ("theoldnet.com says"), modal to the page
area.

## The profile

The data store is built by `createDataStore` in `webkit/port/app/engine.cpp` (#836): every directory of it
below one profile folder, which the window names (`lzw_ui_profile_dir`: `$HOME/.apps/os.lazy.lazywebplus`;
the W5 window gets `$HOME/.apps/os.lazy.webkit`). Cookies (`cookies/cookie.jar.db`), the HTTP cache and site
storage live there, so a second run of the app finds the first one's cookie and history (the harness checks
both, from the OS volume and from the second boot). The folder and `$HOME/Downloads` exist before a child that
may only write inside them looks for them.

**The cookie jar needed a change to the SQLite build.** WebKit opens its databases with write-ahead logging,
whose index is a mapped `-shm` file (`unixShmMap`); LazyOS has no file-backed `mmap`, so every open failed
with `SQLITE_IOERR_SHMMAP` ("disk I/O error"), `CookieJarDB` gave up and *no cookie was ever stored*, not even
for the session (found by logging the open from the NetworkProcess). The bundle's SQLite is now built with
`SQLITE_OMIT_WAL` (`tools/webkit/recipes_text.py`): a rollback journal on plain `pread`/`pwrite` and `fcntl`
locks. Drop it when the kernel can map a file shared (PR #837).

## `mimed`: which browser opens a link

`mimed` keeps one app per `(type, verb)` and the last to register wins, so with LazyWeb and LazyWeb+ both
registering `x-scheme-handler/http`, `x-scheme-handler/https` and `text/html`, the one installed last would
open every link. There was no preference, so one was added: the confd key `user/<uid>/mimed/default/<type>`
(a string holding an app id; `libs/deskmenu/src/defaults.rs`), written by **Settings > Default apps** (all three
types at once) and honoured by `mimed` for the uid the kernel stamped on the `Open`, only while that app still
holds a registration for the type, so a stale choice (an uninstalled browser) falls back to the registry.
`confd` lets a uid touch only `user/<uid>/**`, so one account's choice never moves another's. Evidence:
`MIME:PREFER:PASS` in `mimed`'s selftest, host tests in `deskmenu`. With both browsers installed and no choice made, `mimed`
keeps its old rule (the app registered last wins, which is install order), so LazyWeb+ may open links until the
user picks LazyWeb in Settings; the lookup of the choice is made only when two apps handle the type.

## Permissions

The app: `os.lazy.display.v1`, `os.lazy.display.cursor.v1`, `os.lazy.input.v1`, `os.lazy.mimed.v1`,
`os.lazy.notify.v1`, outbound network, `write:$HOME/.apps/os.lazy.lazywebplus` and `write:$HOME/Downloads`
and the reads the children need, derived from a `--label-trace` run of the harness. The two engine processes
run **sandboxed** like W5's ([`architecture/process-sandbox.md`](architecture/process-sandbox.md), #836): the
manifest's `[processes.webprocess]` and `[processes.networkprocess]` tables give `/system/bin/webprocess` and
`/system/bin/networkprocess` the labels `app:os.lazy.lazywebplus/<name>`, which the kernel enforces for files,
sockets, process creation, signals and inherited descriptors. The page process reads fonts and nothing else;
the network process reads the resolver, the host table and the CA bundle, uses outbound sockets and writes the
profile folder and `$HOME/Downloads` (the `.part` file of a download is its to write; the window renames it).
Anything the UI process passes to the children is a socket. The UI process is an ordinary app: its file and
network entries are consent text, not enforced. A bug in the window, or in what the page process sends it, is
still a way out, and the children run as the user. LazyWeb stays the browser LazyOS ships.

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

* **Back to an older internal page.** The window remembers the newest `data:` URL of each of its own pages, so Back
  to an older History or Downloads page is no longer "its own": its buttons answer "Blocked: the page tried to open
  another app" (safe, but useless) until the page is opened again.
* **Dialogs** are not rate-limited (a page that loops `alert()` keeps the user behind modals) and are titled with
  the top-level site, not the frame that asked.
* **Evidence lines** are `println!`s (the `Webplus` trace channel exists and is not used yet; the ones that name a page,
  an address or a file print only in a `LAZYOS_UI_PROBE=1` image).
* **`history.tsv` is in the folder the network process may write** (the profile): the window reads only the last 2 MiB
  of it, cuts titles at 512 characters and drops addresses over 2048, but a compromised network process can still
  forge entries.
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

### The sandbox attack harness

`python tools/webkit/attack_run.py --browser --app lazywebplus` (issue #832's harness, parametrised) boots a
desktop image with `LAZYOS_LAZYWEBPLUS=1 LAZYOS_WEBKIT_ATTACK=1`, in which the two engine programs are a
stand-in that attacks from inside the labels `app:os.lazy.lazywebplus/webprocess` and `.../networkprocess`
(the real UI process starts them), and `python tools/webkit/attack_run.py --kernel` is the kernel-only mode.
The judge's table (`tools/webkit/attackjudge.py`) is the same for both browsers but one row: LazyWeb+'s
network process may write the Downloads folder (the probe `write_downloads`), WebKit's and either page
process may not; `test_attack_judge.py` checks that LazyWeb+'s manifest tables are WebKit's plus that grant.

## Results

`python tools/webplus/run.py --label-trace` (WHPX, 4 GiB guest, a desktop image with networking and the UI
probe, the 8G user memory quota the launchers set): `WEBPLUS:HARNESS:PASS` with every check green: both
boots ran to their end, every page loaded with its title, the three dialogs were answered from the keyboard
and the answers reached the page, the script's `mailto:`, `x-lazyweb:clear-history` and `window.open` with no
click were refused (`LAUNCH:...:BLOCKED`), the click on the `mailto:` link reached `mimed`, `target=_blank` and
a clicked `window.open` opened in the one view, both attachments (300 KiB, and 200 KiB sent over ten seconds)
were saved whole, the host saw the pages, the six resources over HTTPS with SNI, the form's query and the
cookie on the second boot, the profile (history, `Downloads`, `cookies/cookie.jar.db`) was on the OS volume
and Clear history emptied it, the wheel moved the static bands exactly 200 px, the page was laid out again
when the window grew and painted to the right edge when maximized, and both boots closed the window and
reaped both children (`CHILD:web:<pid>:GONE`, `CHILD:network:<pid>:GONE`, `QUIT:PASS`).

| | |
|---|---|
| Load (ms, `WEBPLUS:INFO:TIME`) | 17 navigations: 1 to 262 ms from request to the last byte of the document for most pages (example.com 62, theoldnet.com with its pictures 262, a form 24 to 45); 3.25 s for the script page, which waits for three dialogs; first paint 3 to 119 ms after the load |
| Programs | `lazywebplus-app` **108.0 MiB** stripped static-PIE (113,235,512 bytes); `WebProcess` and `NetworkProcess` 107.4 MiB each; the window's static library is 161 MiB of archive and costs about what W5's does (the seam is shared). The package `os.lazy.lazywebplus-0.1.0.lzp` is **44.7 MB** (deflate); the first boot installs it in about 4.9 s (`PKGD:PROVISION:INSTALL ... ticks=495`) |
| Memory | free RAM of the 4 GiB guest after the first page 3,479 MiB, after 19 navigations 3,404 MiB (`INFO:MEM:used_since_first_load=75`) |
| Labels | `LABEL:ENTER` for both children; **no `LABEL:DENY` beyond the nine optional ones #836 documents** (stat of `/etc`, `/etc/localtime`, `/usr/share/zoneinfo`; `/etc/ssl/openssl.cnf`; fontconfig's baked-in `conf.avail` and its cache writes under `/tmp` and `/system/share/fonts`); the judge (`tools/webplus/denies.py`) fails on any other |
| Sandbox attack harness | `attack_run.py --kernel`: 60 probes as expected; `attack_run.py --browser --app lazywebplus` (the real UI process starts the stand-ins under `app:os.lazy.lazywebplus/<name>`): **60 probes, every one as expected** (every refusal `EPERM` with its `LABEL:DENY`, the network process's `write_downloads` allowed, the page process's refused) |

## Not verified, and known limits of what was

* **Download progress while a download runs.** The status bar showed "0 bytes of 200.0 KB" four seconds into a
  ten-second download: the engine's `didWriteData` did not arrive before the end on a slow attachment, so the
  progress bar jumps from 0 to done (the file, its size and the rename are verified).
* **Toasts** (see *Known limits*): the shell refuses them from the download thread.
* Not tried: HiDPI, a WebProcess crash in this window (W5's *Failures* path is shared), IME, the pasteboard,
  Settings > Default apps by a session (the preference is covered by `deskmenu` host tests and `mimed`'s
  boot self-test), a real PC.
* The harness's stand-in sites are LazyWeb's (`tools/web/sites.py`): nothing was loaded from the internet.

## `os.lazy.webkit` is not retired

The issue allows retiring the W5 browser once this harness passes everything W5's did. It was not done: LazyWeb+
is the same engine with LazyWeb's window, W5's `app_run.py` harness has not been run against it page for page,
and the W5 package is the smaller program for anyone who wants the engine with no chrome. The two stay; the
retirement (the package, `LAZYOS_WEBKIT`, `app.py` without `--plus`, `xui-app/crates/webkit`'s window feature)
is a follow-up once someone decides that.
