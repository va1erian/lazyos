# LazyWeb plan: a daily-driver browser on webd

**Status:** plan, 2026-10-22. It takes the owner's draft "lazyweb+ Improvement Plan" (2026-10-20) and
rewrites it for the architecture that landed two days later: LazyWeb is a **thin client of webd**
([`lazyweb.md`](lazyweb.md), [`webd.md`](webd.md), [`webd-plan.md`](webd-plan.md) "P4 as built"). The
goal and the four phases are the draft's. What changed is where each piece lives, what webd has to grow for it,
and which of the draft's problems are already solved. Section 1 lists the differences.

LazyWeb becomes a browser to use every day in four phases. B1 is the shell (tabs, windows). B2 is the profile
store (the omnibar, history, bookmarks, control over stored data). B3 is the password vault and private windows.
B4 is reporting an issue through LazyAgent. Targets are the Alder Lake boxes with 8 GB and 32 GB of RAM. Out of
scope: sync across devices, extensions, and engine work beyond what these features need.

**Progress (2026-10-23).** B1's webd half is started and host-tested: `ViewConfig.process_key`, the memory-gated
ceiling of 16 web processes, 64 views for the browser scope, `DetachSlots` (45), `RecoverGroup` (44), and a user
agent the person can change per tab (`ViewConfig.user_agent` 16, `SetUserAgent` 46), with `FullWebView` calls for
each ([`webd.md`](webd.md) "Tabs and the user agent"). The engine's C++ half (`webd_engine_recover_group`,
`webd_engine_set_user_agent`, `webd_view_config.user_agent`) is written but was **not built or run** (no WebKit build
volume in that session). Not done: the parked-view pool for full views, `webd_engine_view_pid`, spike S1, the
window's `tabs` module and strip, session restore and the discard numbers. The user agent is not in the draft: it
is what a tab needs for "request the desktop site", and it takes field id 16, so the credentials flag moves to 17
and icons to 18 in section 8.

## 0. Principles

* **The window owns the person's data; webd owns the engine's.** Bookmarks, history, sessions, passwords,
  the notification choices and the ad-block lists sit in the browser's folder,
  `$HOME/.apps/os.lazy.lazyweb/`. No engine process can write it, and the window is the only program that
  reads it. Cookies, the HTTP cache and site storage sit in webd's `browser` profile
  (`$HOME/.apps/os.lazy.webd/profiles/browser`) and are reached only through webd calls. "Clear data" is one
  call per kind on each side, behind one function in the window.
* **The shell comes before the features.** Tabs and the omnibar are the surfaces the other features hang
  off (the bookmark star, the padlock, the report button), so they land first.
* **Every engine capability is a webd interface addition.** It is append-only in `idl/webd.midl` and gated by
  the browser scope (`os.lazy.webd.browser.v1`) or by a `ViewConfig` flag that only that scope may set. Mail,
  Docs and other webd clients gain nothing by accident.
* **Local and inspectable.** All user data stays on disk, is listed on a page of the browser's own, and can be
  exported or wiped.
* **The browser never talks to GitHub.** Reports go to LazyAgent over Messenger. LazyAgent holds the token,
  the redaction and the one path that posts.

## 1. What changed since the draft

| The draft assumed | Today (main, 2026-10-22) | Consequence for this plan |
|---|---|---|
| LazyWeb (Blitz) is still there and is removed in B1; Blitz stays for Mail and Docs | LazyWeb, Blitz and `webfonts` were removed in webd P3. Mail and Docs use light views of webd. LazyWeb is the only browser and the `mimed` default | Dropped from B1. The open question "which parts of `xui-app/web` move" is answered: none are left |
| Touch points `xui-app/web/src/chrome.rs`, `webkit/port/app/engine.cpp` (one `WKPageRef` per tab in the browser's UI process) | The window is `xui-app/lazyweb` and has no WebKit. A page is a **full view** of webd through `xui_webd_client::FullWebView`, wrapped as `RemoteEngine` (`xui-app/crates/webkit-remote`). The C++ host is webd's (`webkit/port/app`, `webkit/port/webd`) | A tab is a view. The window manages views. Engine work goes into webd (`xui-app/crates/webd`, `webkit/port/app/*`) and `idl/webd.midl` |
| "Each tab gets its own WebProcess" | webd puts every view of one (client label, profile) **group** on one web process, at most 4 web processes per user (`WEB_PROCESSES_PER_USER`) and 8 views per client (`VIEWS_PER_CLIENT`). `Recover` refuses a process that other views share | Per-tab or per-site processes need a webd change (section 2.3). Ten tabs need the view cap raised |
| A WebProcess is a 108 MB program read whole into RAM, so 20 tabs cost about 2 GB | Program text is shared through the page cache (#942). A held view with its web and network processes measured **+28.7 MiB** on a small page (`webd.md` "Measured"). Real pages add their own heap | The memory guard is about page heaps, not program images. Measure real pages before picking discard numbers (section 2.5) |
| `history.tsv` is in a folder the sandboxed network process can write, so a compromised network process can forge history; move user data to `os.lazy.lazyweb.user/` | The network process is webd's (`app:os.lazy.webd/networkprocess`). It writes webd's folder and `$HOME/Downloads` only, never the browser's folder | The folder move is not needed. B2's gate becomes an attack-harness row instead (section 9, B2) |
| The ad-blocker store needs the same move | The compiled store is webd's (`<profile>/contentextensions`). The lists are the client's, in `lists/`, written only by the `listupdate` child | Nothing to do |
| `target=_blank` and `window.open` reuse the one view | They still do. webd reports `NewWindowRequested(url, gesture)` and nothing opens on its own | B1 opens a tab for a request that has a gesture. The opener link (`window.opener`) is lost (section 2.2) |
| Startup crash at first resize (#853) is a B1 risk | #853 is closed (a kernel COW/mremap bug) | Removed from the risks |
| Crash and hang handling (#861, #862) | #862 is closed: webd's watchdog, `Responsive`, `Recover`, `ViewLost`. #861 (freeze forensics) is open | The crashed-tab page is built on `ViewLost` |
| The vault key comes from `keyd.appsecret` | `os.lazy.keyd.appsecret.v1` is on main (P2b): 16 secrets per app and user, 1 KiB each, scoped to the uid and `app:os.lazy.lazyweb` | As in the draft: one secret for the vault key, one for a wrapped key. Two of sixteen is enough |
| LazyAgent's review screen exists | LazyAgent (`docs/ai-agent-plan.md`) is at P1. Its P2 (window and package) and P6 (diagnosis and GitHub) are not built | B4's browser half can be built against a stub. The full gate waits for agent P6, not P2 |
| Evidence lines | Most `LAZYWEB:*` lines already go through `xui_app::trace` (`marker()` in `lib.rs`, `Channel::LazyWeb`). Four `println!`s remain in `app/events.rs` (`STOPPED`, `FAIL`, `DIALOG`), plus the host-only fallback in `lib.rs` | B0 moves the four and keeps the host fallback out of the LazyOS build. B4 reads the window's own trace ring |

## 2. B1: tabs and windows

### 2.1 Model

One LazyWeb process owns a list of windows. Each window owns an ordered list of tabs. A **tab** is a
`RemoteEngine` with its own `FullWebView`: back/forward list, title, favicon (B2), load state, padlock, hang
state and blocked count. A **discarded** tab keeps its URL, title and favicon and has no view. The page view and
the mailbox of `xui-webkit` stay one per window and are pointed at the active tab's engine. A background tab is
`SetVisible(false)` and gets no paints, which is webd's paint scheduler working as designed (webd-plan 3.2).

The window code splits along this line. `app.rs` and `app/*` keep the window and the chrome. A new `tabs`
module (host-tested, no engine) holds the order, the active tab, the closed-tab stack and the discard
decisions. `app/webd.rs` makes and ends views per tab instead of once. `chrome.rs` gains a tab strip (favicon,
title, spinner while loading, close button). The window title follows the active tab.

**Several windows, one process.** `Ctrl+N` opens a second window in the same process, so there is one
`session.json` writer and one connection to webd. That needs xui's LazyOS backend to drive more than one
window per process. The Terminal opens a new process for `TERM:NEWWIN`, so this is a **spike (S1)**. If xui
cannot do it in time, B1 ships one window with tabs, and `Ctrl+N` and tear-off wait. Tear-off (dragging a tab
out to a window of its own) moves the tab's view to the other window. The view id does not change, so nothing
reloads.

### 2.2 Behaviour

* `Ctrl+T` new tab, `Ctrl+W` close tab (the last tab closes the window), `Ctrl+Shift+T` reopen a closed tab
  (a stack of 10), `Ctrl+Tab` and `Ctrl+Shift+Tab` cycle, `Ctrl+1..8` jump, `Ctrl+9` goes to the last tab.
  Today `Ctrl+W` closes the window; with tabs it closes the tab.
* `target=_blank` and `window.open`: `NewWindowRequested(url, gesture)` with `gesture` and the client's own
  real-input lock (`xui_webkit::gesture`) opens a tab next to the opener. Without them nothing opens and
  `LAZYWEB:POPUP:<url>:BLOCKED` is printed. The rule is the same as today. **Known loss:** the new tab is a
  separate `Load`, so the opener's `window.opener` is `null` and `postMessage` between the two cannot work
  (sign-in pop-ups that report back to their opener break). Keeping the opener link needs WebKit's
  `createNewPage` to return a page webd then hands to the client (`NewWindowRequested` held like a navigation,
  answered with a new view). That is a later webd item (the last row of section 8), not B1.
* Middle-click or `Ctrl+click` on a link opens it in a background tab. webd does not need to change: the window
  knows the link under the pointer from `Hover(url)`, and the click is the person's own input in the window, so
  the window opens the tab itself and does not forward that click to the page. A link whose `href` is
  `javascript:` (the `Hover` address) is forwarded as a plain click.
* A right-click menu on a link (Open in new tab, Copy address) is drawn by the window from the same `Hover`
  address. Pages still get no context menu of their own (a known limit today).

### 2.3 Processes: what webd must change

Today all tabs would share one web process, because they would all be one group: LazyWeb's label and the
`browser` profile. Three consequences follow. One hung or crashed page takes every tab down. `Recover` refuses to end a
process that other views share (`WEBPROCESS:RECOVER:SHARED`). And the draft's gate "kill one tab's WebProcess,
the other nine keep working" cannot pass.

**Decision: one web process per site, chosen by the client inside its own group.** `ViewConfig` gains
`process_key: String = 15` (at most 64 bytes of printable ASCII, opaque to webd). LazyWeb sets it to the
**host** of the tab's first address. The registrable domain (`example.com` for `www.example.com`) would group
better, but it needs a public-suffix list the window does not have; that is a later refinement. Tabs of one host
share a process and other hosts are isolated, which is the draft's "reuse one process for tabs of the same
site". A page that navigates to another site stays in its process (WebKit's process swapping is off,
`webd-plan.md` 3.3). The key is a placement hint, not a security boundary. Light views ignore the field. **Review of 2026-10-23:** a key
is accepted only with the `browser` profile kept on disk, because the engine shares an in-memory store only through a
live group mate: keyed tabs of a private window would each get a store of their own. Private windows therefore stay
in one process until the engine can key a memory store by owner and store kind without the group. Downloads are
counted under the client's default group, not the key's group.

**This amends open question 7 of the webd plan**, which says the group is "never from a wire field". The
amendment is narrow. The label still comes from the kernel, and the key can only split the caller's **own**
group further, never join another client's, at the cost of its own user's memory. Today a group is one number
per client (`Groups` in `xui-app/crates/webd/src/ident.rs`: `ClientKey` to `i32`, numbers never reused), the
engine keeps profiles and store kinds apart when it matches group-mates (`webkit/port/app/engine.cpp`), and
`service/create.rs` counts processes as (client, profile, no network). With the key:

* `Groups` maps `(ClientKey, process_key)` to a number. A number is still never given to another client, and is
  **released** when the last view of its group ends, so the table does not grow with every host a client ever
  visited. A client holds at most 32 keys at once (`EDQUOT` past that); the window then puts the tab in the
  largest existing group.
* The process count in `create.rs` becomes (client, profile, no network, key).

That needs, in webd:

* **Limits by memory, not by count.** `WEB_PROCESSES_PER_USER` goes from 4 to a ceiling of 16. Below the ceiling
  the gate is the user's memory charge (`lzw_ui_user_memory`, already read before a process is made). A refusal
  is `EDQUOT` on `CreateView`, as the code does today (webd-plan §8 and open question 7 still say
  `ViewLost(no_memory)`; that text is updated with the amendment). The window then puts the tab in the largest
  existing group, or discards the oldest background tab and tries again. `VIEWS_PER_CLIENT` (8) becomes 64 for
  a client holding the browser scope. The "8 views" in webd-plan §5's security table and in `limits.rs`'s
  comments change with it. The per-client slot budget (`SLOT_BYTES_PER_CLIENT`, 512 MiB) is unchanged. It is
  why the window attaches slots **only to the visible tab's view** and lets go of them on a tab switch.
  `ReleaseSlot` only says the app has read a slot. Today only `Resize` detaches every slot, and a same-size
  `Resize` may cost the page a layout, so B1 adds `DetachSlots(view)` (method 45). A tab coming to the front
  attaches its slots again and asks for a whole frame (`Refresh`).
* **The parked-view pool** (webd-plan "What P5 still needs"). Every closed view leaves a parked proxy today, so
  opening and closing tabs grows webd. The pool is reused by the same group. B1's gate depends on it, so it
  moves from P5 into B1.
* **Hang recovery of a shared group** (also P5). Tabs of one site share a process. A hung site ends that
  process (`Recover` with a new `group` flag), webd reports `ViewLost(Other)` for each of the group's views,
  and each tab shows the crashed-tab page.
* **`webd_engine_view_pid`** (also P5), so the harness and the window's task manager row can tell which process
  a tab runs in.

A crashed tab (`ViewLost(Crashed|OutOfMemory)`) shows "This tab stopped (reason). Reload". It is a page of the
browser's own, so it uses no network. The tab keeps its URL, and Reload makes a new view.

### 2.4 Session restore

On every change to a tab or a window, the window writes `session.json` (windows, tabs, URLs, active index, the
closed-tab stack) to the browser's folder with an atomic rename, at most once a second. It never writes in a
private window. A `running` file written at start and removed at a clean quit tells the next start that the
last run did not end cleanly. In that case the window offers a bar, "Restore previous session", or restores
without asking when the setting says so (`confd`, `user/<uid>/lazyweb/session/restore`). Restored tabs are
**discarded** until focused, so a start never loads twenty pages, and a page that crashed the last run does
not crash this one before it is asked for.

### 2.5 Memory guard

* Tabs of a site share a process (2.3), and background tabs get no paints.
* **Discard** background tabs when the user's free memory drops below a threshold, and always past a count
  limit. The oldest-used goes first, never the active tab of any window, never a tab with a dialog open or a
  download running. The window reads free memory from `sysmond`'s `system/stats/memory` topic (the browser
  lists the interface in its manifest). The numbers are **not picked here**. B1 measures per-tab cost on real
  pages first (Ars Technica, a news site, a web mail, a video page) on both boxes, from the engine's
  `INFO:MEM` lines (`webkit/port/app/clients.cpp`, through `WEBD:NOTE`) and `sysmond`'s memory topic, and the
  thresholds follow from that table.
* What sharing program text (#942) does not save: each web process's own heap, JIT and decoded images, which
  is the unknown the measurement answers.

### 2.6 B1 tests

* Host: the `tabs` model (order, close, reopen, cycle, discard choice, restore) in `xui-app/lazyweb/tests/tabs.rs`
  against the recording engine. One `RemoteEngine` per tab in `xui-webkit-remote`'s `tests/remote.rs` against
  `xui-webd`'s in-memory fabric. webd's group key, limits and pool in `xui-webd`'s host tests, with a seeded
  create/destroy soak judging slots, groups and the pool.
* Engine: `port.py --run-views` gains a `webkit-views sites` case (two keys, two processes; kill one, the
  other paints).
* Guest: `tools/lazyweb/run.py` gains a tab boot that opens 10 tabs over 3 stand-in sites, follows a
  `target=_blank` link (a tab opens) and a script `window.open` (refused), kills one site's web process from
  the harness and judges that the other sites' tabs still paint and that the killed site's tabs show the
  crashed page. A second boot restores the session lazily and judges that only the active tab loaded.
  `--label-trace` adds no `LABEL:DENY`.

## 3. B2: the omnibar

The address field becomes one box for addresses and searches, with a list of suggestions as the person types.
Classification grows from `xui-app/lazyweb/src/address.rs` (`normalize`), pure and host-tested:

1. It has a scheme (`https:`, `about:`, `data:`): navigate. `file:` still does not open (the `LoadHtml` route of
   webd-plan open question 5 is a separate item).
2. It looks like a host (`example.com`, `localhost`, `localhost:8080`, an IP literal, a single word followed
   by `/`): load `https://` first. Only a **connection** failure (refused, reset, timed out, before anything
   committed) offers `http://`, and never a certificate error: a network attacker can block port 443 to force
   the downgrade. The offer is a page of the browser's own ("This site did not answer over HTTPS. Load it
   without encryption?"), not a silent retry. A site the person said yes to is remembered for the session, and
   its padlock shows open. Typed text with no dot and no port (other than `localhost`) is a search.
3. Anything else, or text that starts with `?`: search. The default template is DuckDuckGo
   (`https://duckduckgo.com/?q=%s`), a setting in `confd` (`user/<uid>/lazyweb/search/template`). It is the
   user's, and the engine processes cannot reach it.

**Suggestions:** at most 8, computed in under 16 ms per keystroke on the window's thread. Nothing goes to the
network. A search engine's suggest API stays off unless the person turns it on, and that is not in v1.

| Source | Match | Rank |
|---|---|---|
| Open tabs | title, URL | a "Switch to tab" row, first |
| Bookmarks | title, URL, folder | high |
| History | title words, URL host prefix | visits x recency |
| Search | the typed text | always last |

The top match fills in the rest of the host inline (`git` becomes `github.com`), selected, so one more key
replaces it. `Shift+Delete` removes the highlighted history row.

**Display:** the host in bold and the rest in grey. To the left of the text, the padlock (`State.secure`) and a
site button (section 5.2). To the right, the bookmark star and the report button (B4).

## 4. B2: the profile store, history and bookmarks

### 4.1 Where and how

Everything lives in the browser's folder, `$HOME/.apps/os.lazy.lazyweb/`, which only the window writes.
The store is `xui-app/lazyweb/src/places/` (or a crate `xui-app/crates/places` if it grows), host-tested and
fuzzed. The draft asked for SQLite (`places.db` with FTS5). This plan recommends a **pure-Rust store** for v1:

* A record log per kind (`history.log`, `bookmarks.log`): length-prefixed, checksummed records, appended with
  `fsync` at commit points, compacted into a new file renamed into place when half of it is dead. The in-memory
  index (host prefixes, a word index over titles) is built at start.
* **Why not SQLite now.** LazyWeb builds today without zig. Mail's SQLite is why `tools/xui/build.py --mail`
  needs zig, and the same would hold here. WAL is unavailable on LazyOS (no shared writable file `mmap`, the
  reason WebKit's bundle uses `SQLITE_OMIT_WAL`). And 50,000 history rows in memory are a few MiB, well inside
  the 16 ms budget for a linear scan or a small index. If a later feature needs real queries, the log migrates
  into SQLite in one step. This is open question 1.

`history.tsv` is migrated once into `history.log` at the first start and then left in place (as the old
engine's files are, `lazyweb.md` "Upgrading"). History keeps the newest 90 days (a setting), not the newest
1000 pages.

### 4.2 Bookmarks

Schema: `bookmarks(id, parent_id, kind: folder | link | separator, title, url, position, created, modified)`
with three fixed roots: Bookmarks bar, Other bookmarks, Unsorted.

- [ ] The star in the omnibar: one click bookmarks into Unsorted. A second click opens a small editor (name,
  folder, remove). `Ctrl+D` does the same.
- [ ] A bookmarks bar under the toolbar, toggled with `Ctrl+Shift+B`. Folders open as menus.
- [ ] `about:bookmarks`: a tree, search, drag to reorder and refile, delete several with undo. It is a page
  of the browser's own (a `data:` page or `LoadHtml`, as `about:history` is), and its commands are
  `x-lazyweb:` links under today's rule: only from that page, only for a click.
- [ ] A Bookmarks menu in the menu bar, beside History.
- [ ] Import and export in the Netscape bookmark HTML format. Import parses untrusted HTML in the window: a
  small tolerant parser for the `<DL><DT><A HREF>` subset, fuzzed, at most 16 MiB and 100,000 entries, and
  `javascript:` and `data:` addresses dropped. The file comes through the Open dialog, and export goes through
  the Save dialog.

### 4.3 Favicons (a webd addition)

webd has no favicon today. `ViewConfig.icons: Bool = 18` (browser scope only) makes webd send
`Favicon(view, page_url, width, height, rgba: Bytes)` (event 126) once a page's icon has loaded and been
**decoded on the engine's side**: the web process already decodes images and is sandboxed, so untrusted icon
bytes are never parsed in the window, which holds the vault key (section 6). The picture is scaled to at most
32x32 (4 KiB of RGBA) and `page_url` is cut at 2 KiB, so the event stays well inside `EVENT_BYTES_MAX`
(`xui-app/crates/webd/src/limits.rs`). The window checks `rgba.len() == width * height * 4` and keeps the
pixels in `favicons.log`, one per host. Nothing is handed back to a page. Engine half: a **spike (S2)** to find which WebKit C API gives the icon's decoded image
in this port (the GTK and WPE ports use a favicon database that `OptionsLazyOS.cmake` may not enable).
The fallback is the first letter of the host on a coloured tile.

## 5. B2/B3: control over stored data

### 5.1 `about:data`

One page lists every kind of data the browser keeps, its size on disk, and a clear button per kind and per site.

| Data | Where | Kept by | Cleared by | Retention |
|---|---|---|---|---|
| History | `history.log` | window | range (hour, day, week, all), site | 90 days |
| Bookmarks | `bookmarks.log` | window | entry, folder | kept |
| Favicons | `favicons.log` | window | all; a range or site clear of history removes the hosts no history or bookmark still names | follows history |
| Session | `session.json` | window | all | last session |
| Passwords | `vault.db` (B3) | window | entry, site, all | kept |
| Notification choices | `notify.tsv` | window (told to webd) | site, all | kept |
| Downloads list | in memory today | window | entry, all (the files stay) | the session |
| Ad-block lists | `lists/` | window | all (downloaded again) | kept |
| Cookies, site storage | webd `browser` profile | webd | site, all | kept |
| HTTP cache | webd `browser` profile | webd | all | size cap 256 MiB |
| Compiled block rules | webd `browser` profile | webd | with the lists | kept |

The window's half runs on the window's own files. The engine's half needs new webd calls, all behind the browser
scope and only on the `browser` profile:

* `DataUsage(view, request: U64, offset: U32)` (method 41): WebKit fetches data records asynchronously, so the
  call returns at once and the answer is the event `DataUsageResult(view, request, sites: Array<SiteData>,
  total: U32, cache_bytes: I64)` (event 129), at most 256 rows per event; the window asks again with the next
  `offset`. `struct SiteData { site: String = 1 (at most 255 bytes), cookies: U32 = 2, storage_bytes: I64 = 3 }`.
  It is built from `WKWebsiteDataStore`'s fetch-records call.
* `ClearData(view, request: U64, kinds: U32, sites: Array<String>, since: I64)` (method 42): a bit mask of
  cookies, storage, cache and the compiled rules. Empty `sites` means all of them. `since` is a time or 0. The
  answer is the event `DataCleared(view, request, ok)` (event 128), because WebKit clears asynchronously, and
  the page shows progress until the network process says it is done.

One function in the window, `profile::clear(kind, scope)`, is the only caller of both halves. The menu,
`about:data`, the site panel and clear-on-exit all go through it, and its host tests are the tests of every
surface.

### 5.2 Controls

- [ ] **The site panel**, from the omnibar's site button: the padlock's meaning, cookies and storage for this
  site (`DataUsage` filtered), the notification permission, saved logins (B3), and "Clear data for this site".
- [ ] **Clear on exit**, per kind (history, cookies, cache), done at a clean quit and at the next start after an
  unclean one.
- [ ] **Third-party cookies blocked by default**, with per-site exceptions. A new call,
  `SetCookiePolicy(view, policy: U32, exceptions: Array<String>)` (method 43), sets
  `WKHTTPCookieStore`'s accept policy on the profile (Always, NoThirdParty, Never), kept per profile like
  `SetBlocking`. The window keeps the choice in `confd` and tells webd at each `CreateView`. WebKit's accept
  policy has no per-site exceptions, so the exceptions are a **spike (S4)** in the network process (WebKit's
  third-party cookie blocking has per-domain hooks, but the C API may not expose them). Without S4, B2 ships
  the policy with no exceptions. Blocking third-party cookies breaks some single sign-on flows, which is why
  the exceptions matter (section 10).
- [ ] **Export everything except passwords** as one archive (bookmarks HTML, history CSV, settings) through
  the Save dialog, with `lazyarc`'s zip writer (`xui-app/crates/archive`).

### 5.3 Private windows (B3)

`Ctrl+Shift+N` opens a window whose views are made with `ephemeral`: webd's `browser` profile in memory, the
mode File > Keep Page Data in Memory already uses (#940). The window writes no history, no session, no
favicon and no password, and its frame is drawn in a distinct colour.

* **An ephemeral view beside a persistent one already works in webd.** `fullconfig.rs` keys
  `Profile::Browser { ephemeral }` apart, the engine keeps the memory store and the disk store apart when it
  matches group-mates, and `webkit/port/webd/test/modes_store.cpp` tests it. B3's harness still checks it end
  to end: a cookie set in the private window is not seen in the normal one, and is gone when the private
  windows close. All of a client's private windows share **one** memory store for the session (as in other
  browsers); that is intended, and the store is dropped when the last private view ends.
* **Blocking in memory is the change.** `fullconfig.rs` refuses `blocking` for a profile that is not on disk,
  because the rule lists live in the profile's folder. The compiled store is only read after compile, so webd
  attaches the `browser` profile's compiled lists to the memory store too (`BlockingLoad` reads, never
  writes) and the check is relaxed for the browser scope. Notifications stay refused in a private window.

## 6. B3: passwords

### 6.1 The vault

The vault is an encrypted file whose key `keyd` holds. `os.lazy.keyd.appsecret.v1` allows 16 secrets of 1 KiB
per app and user, too few to keep one per site, and enough for two keys.

1. At the first save, the window makes a random 256-bit vault key (the OS's `getrandom`) and stores it with
   `appsecret.Put("vault-key")`. `keyd` seals it under the machine key and scopes it to the uid and
   `app:os.lazy.lazyweb`. A helper child (`app:os.lazy.lazyweb/listupdate`) is refused by keyd,
   and webd's labels are other apps.
2. `vault.db` in the browser's folder holds rows of `(origin, username, nonce, ciphertext, created,
   last_used)`. The password is sealed with XChaCha20-Poly1305 from RustCrypto's `chacha20poly1305` crate
   (random 192-bit nonces need no counter). `nettls-crypto` has the crate only as a private dependency behind
   TLS cipher suites, so the window adds it directly, and `licenses.py --lazyweb` checks it (MIT OR
   Apache-2.0). The associated data is the origin and the username, so a row cannot be moved to another site.
   The file is a record log like the history (section 4.1), with no SQLite.
3. **Optional primary password.** When one is set, the vault key is stored wrapped by an Argon2id key derived
   from it (`appsecret.Put("vault-key-wrapped")`, the bare key deleted), and the vault locks after 15 idle
   minutes. The Argon2id code is `libs/crypto/src/kdf.rs`, which the account database uses, with its
   parameters.

### 6.2 Capture and fill (webd additions)

The engine sees the form. The window holds the vault. webd carries what crosses between them, only for a view
made with `ViewConfig.credentials: Bool = 17`, which only the browser scope may set (`EACCES` otherwise,
like `downloads`).

- [ ] **Capture.** When a form with a password field is submitted in the main frame, webd sends
  `FormSubmitted(view, origin, page_url, username, password, new_password)` (event 127). **`origin` and
  `page_url` are webd's own**: the view's committed URL in the UI process, never a value the page process
  reports. Otherwise a compromised page process of `evil.example` could label a submission `bank.example`,
  and the person, trusting the bar, would save the attacker's password for the bank. A submission from a
  subframe is not offered in v1. The window then shows a bar naming that origin: Save, Never for this site,
  Not now. Nothing is written before the click. The event carries a secret, so webd sends it at once,
  outside the shared event queue (`QUEUE_KEPT`), wipes its copy after the send, never prints it, and the
  trace lint learns to refuse its fields. The draft's "plaintext never crosses to the network process" holds:
  the password goes page process, then webd's UI process, then the window, the path it already took to be
  typed.
- [ ] **Fill only on the exact origin**, scheme and port included: an `http://` page never gets an `https://`
  password. Fill only on the person's action (the key icon in the field or the omnibar), never at page load, so
  a hidden form cannot harvest it. The window calls `FillCredentials(view, origin, username, password)`
  (method 40). It is checked twice. webd compares the view's **committed** origin with `origin` and refuses
  with `EACCES` on any difference. Then the fill code in the page process (S3 below) receives `origin` too and
  compares it with the main frame document's own origin at the moment it fills, because a navigation can
  commit between webd's check and the fill. It refuses a field in a cross-origin subframe or outside the
  focused form, and fills that form's username and password fields and nothing else.
- [ ] The window decrypts into a buffer it zeroes after the call (`zeroize` on the plaintext and the key). The
  Messenger message is sent and dropped, and the window's request buffer is cleared.
- [ ] `about:passwords`: search, view (after the person's account password through
  `os.lazy.accounts.v1.Authenticate`, which is slowed per caller; `keyd.Verify` takes calls from `accountsd`
  only. With a primary password set, that password instead), edit, delete, export to CSV through the Save dialog behind a warning. A page of the
  browser's own whose rows are built from the vault in the window and given with `LoadHtml`. A password is
  never put in the page's HTML: View reveals one row through a window dialog.
- [ ] A password generator for new-account forms (20 characters, adjustable), filled by the same method.

Engine half, a **spike (S3)**: the form client and the fill both need code in the web process. WebKit2 gives
the UI process a submitted form's text fields only through an injected bundle (`WKBundlePageFormClient`), and
filling a field is either the bundle or a script in an isolated world. Does the port load an injected bundle?
If it does not, the spike adds a minimal one (form fields out, fill in, nothing else) to the `WebProcess`
build. Its sandbox does not change: it is code in the same process and label.

**Out of scope for v1:** passkeys and WebAuthn, sync, breach checks (they would send hashes off the box).

## 7. B4: report an issue through LazyAgent

LazyWeb collects a report and hands it to LazyAgent over Messenger. LazyAgent turns it into a draft on its
review screen (`ai-agent-plan.md` §8.3), and only the person's **File issue** click posts it to
`va1erian/lazyos`. This reuses what that plan specifies: the diagnosis model, the renderer that is the
exfiltration filter, the fingerprint search for duplicates, and the GitHub token that `keyd` keeps. The
browser gets no token, no path to GitHub and no new HIGH grant.

**A new interface, `os.lazy.agent.report.v1`** (`idl/agent_report.midl`, served by LazyAgent as
`app.os.lazy.agent.report`; the `SERVICE_NAMES` row or the generic rule for app-served interfaces,
webd-plan 3.1). It is **generic**: any app may report about itself, and the kernel-stamped label names the app.
The `app_id` field of the draft is dropped, because it would be a claim made over the wire.

* `Submit(report: Report) -> (draft: U64)`: opens LazyAgent on a new Diagnose conversation seeded with the
  report, or brings it to the front. Rated LOW: it only makes a draft that the person reviews.
* `Report { app_version, summary, steps, expected, actual, attachments: Array<Attachment> }`, at most 256 KiB
  in all, each text field cut and stripped of control characters. `Attachment { kind, name, text }`. LazyAgent
  sizes its receive buffer for the largest report.
* `SubmitWithPicture(report: Report, picture: Buffer)`: the same with a screenshot as a PNG. An object is a
  declared parameter that is always present (`messenger.md` §1: never in an `Option`), so a report without a
  picture uses `Submit`. A screenshot is page pixels that `libs/redact` cannot redact. The form says so in its
  preview, and the box is never ticked by default.
* LazyAgent accepts at most 3 reports a minute per app and one open draft per app at a time; more are
  `EBUSY`. Any app may call it, so this keeps a misbehaving app from flooding the person with drafts.
* Classified for developer tools in `tools/devtool/reviewed_interfaces.txt` (a script may file a draft), with an
  `explain` row and `midlc --rhai-api` regenerated.

**What the browser offers to attach.** Each is a checkbox, unticked by default except the first, with a
preview:

- [ ] Build id, LazyWeb version, webd's version, tab count, the window's memory and webd's.
- [ ] The current page's **host** only. The full address is opt-in, and its query string and fragment are
  stripped.
- [ ] The last 200 lines of the window's own `LazyWeb` trace. That needs B0's move to `xui_app::trace` and a
  small in-process ring of the channel. Page content never goes in it.
- [ ] A screenshot of the window, from the window's own picture (the mailbox), shown before sending.
- [ ] The engine events this run saw: `ViewLost` reasons, `Responsive(false)` times, `Recover` answers. webd's own
  `WEBD:NOTE` lines are not the browser's to read; LazyAgent's `diagd` tools fetch them if the agent wants
  more evidence.

**Flow.** The bug button in the omnibar (and Help > Report an issue) opens a small window form: what happened,
what you expected, the checkboxes with their previews. **Send to LazyAgent** calls `Submit`. LazyAgent is not a
resident app, so when its name does not resolve, the browser starts it with `init.Launch("os.lazy.agent")`
(a permission it lists in its manifest) and retries for 5 seconds. LazyAgent may then run its read-only tools,
redacts everything (`libs/redact`), and shows the exact issue text. The person edits it, files it or discards it
there.

**Fallback.** When LazyAgent is not installed, does not start, or has no GitHub token, the form offers **Copy
Markdown** and **Save .md** (through the Save dialog) instead of failing.

**Labels:** `bug`, `area:lazyweb`, and LazyAgent's `agent:diagnosis` when the agent added evidence.

## 8. webd interface additions, in one place

All are append-only in `idl/webd.midl` (methods after 39, events after 125, `ViewConfig` fields after 14; `process_key` 15 and `user_agent` 16 are taken). The
same fields are added to the `net` and `browser` scopes' `ViewConfig` copies. `python tools/webd/test_idl.py`
keeps the shared ids, and the `explain` rows and `midlc --rhai-api` output are regenerated.

| Addition | Id | Gate | Phase |
|---|---|---|---|
| `ViewConfig.process_key: String` | 15 | browser scope, `browser` profile on disk; light views have no field | B1 (built in webd) |
| `ViewConfig.user_agent: String` (a full agent, at most 256 bytes), `SetUserAgent(view, user_agent)` | field 16, method 46 | browser scope | B1 (built in webd) |
| `ViewConfig.credentials: Bool` | 17 | browser scope | B3 |
| `ViewConfig.icons: Bool` | 18 | browser scope | B2 |
| `FillCredentials(view, origin, username, password)` | method 40 | a `credentials` view | B3 |
| `DataUsage(view, request, offset)` | method 41 | browser scope, `browser` profile | B2 |
| `ClearData(view, request, kinds, sites, since)` | method 42 | browser scope, `browser` profile | B2 |
| `SetCookiePolicy(view, policy, exceptions)` | method 43 | browser scope | B2 |
| `Recover` with a group flag (a new method, `RecoverGroup(view)`, so method 17 keeps its shape) | method 44 | full view | B1 (webd service built; engine half unverified) |
| `DetachSlots(view) -> (seq)` (the reply names the last frame sent, so frames queued before it can be ignored) | method 45 | full view | B1 (built in webd) |
| `Favicon(view, page_url, width, height, rgba)` | event 126 | an `icons` view | B2 |
| `FormSubmitted(view, origin, page_url, username, password, new_password)` | event 127 | a `credentials` view | B3 |
| `DataCleared(view, request, ok)` | event 128 | browser scope | B2 |
| `DataUsageResult(view, request, sites, total, cache_bytes)` and `struct SiteData` | event 129 | browser scope | B2 |
| Limits: web processes per user 4 to 16 below the memory gate; views per client 8 to 64 with the browser scope | `limits.rs` | | B1 |
| Parked-view pool, `webd_engine_view_pid` | engine | | B1 |
| An ephemeral `browser` store beside the persistent one, blocking lists attached to it | engine | | B3 |
| A held `NewWindowRequested` answered with a new view, keeping `window.opener` | later | | after B4 |

webd's security table (webd-plan §5) gains three rows:

* **A fill on a page that navigated meanwhile.** webd checks the committed origin, and the page process
  checks its document's origin again at the moment it fills.
* **A compromised page process forges `FormSubmitted`.** The origin in the event is webd's committed URL, not
  the page's word, so a forged event can only offer to save a password for the site the process already
  serves. The window saves nothing without a click.
* **A client splits its group to exhaust memory.** At most 32 keys per client, the memory gate and the
  16-process ceiling bound it, and only its own user pays.

`os.lazy.webd.view.v1` is in `tools/devtool/reviewed_interfaces.txt` as script-callable. The new secret-carrying
calls are usable only on a `credentials` view, which needs the browser scope, and `os.lazy.webd.browser.v1` is in
`devtool::DENY` (`libs/devtool`), so a developer tool cannot make one. The classification stays, with that reason
written beside it.

## 9. Phasing

Each phase ships on its own and passes `tools/lazyweb/run.py`, `notify_run.py`, `hang_run.py` and
`tools/webkit/attack_run.py --browser`, with no new `LABEL:DENY`.

| Phase | Deliverable | Gate |
|---|---|---|
| **B0: groundwork** (days) | The four remaining `println!`s of `app/events.rs` through `marker()`, the host fallback in `lib.rs` kept out of the LazyOS build, `docs/lazyweb.md`'s "Evidence lines" limit corrected; spikes S1 (several xui windows in one process), S2 (decoded favicons), S3 (form bundle), S4 (per-site cookie exceptions) recorded in this file | `check_adhoc.py --ratchet` with `app/events.rs` at 0 and its baseline lowered; four spike answers |
| **B1: shell** | Tabs, the tab strip, keys, `target=_blank` as a tab, middle-click and link menu, the crashed-tab page, session restore (lazy), discard; in webd `process_key`, the memory-gated limits, the parked-view pool, group recovery, `view_pid`; windows if S1 allows | The tab boot of 2.6: 10 tabs over 3 sites, one site's process killed and the others painting; `target=_blank` opens a tab; a script's `window.open` refused; the restored session loads only the active tab; webd's view count back to its start after 200 tab open/close cycles; per-tab memory measured on real pages on both boxes and the discard numbers written here |
| **B2: profile store** | The places store with `history.tsv` migrated, the omnibar and its suggestions, bookmarks (star, bar, manager, HTML import/export), favicons, `about:data` with `DataUsage`/`ClearData`, the site panel, the third-party cookie policy, clear on exit, export | The omnibar under 16 ms per key with 50,000 history rows (a host bench like LazySky's `--test bench`); a cookie of a site cleared from the site panel is gone in the next boot's request log; the import fuzz target runs clean for its seeded cases; the attack harness shows that webd's network and page processes cannot write `$HOME/.apps/os.lazy.lazyweb` (a new row in `attackjudge.py`) |
| **B3: vault and private windows** | The `keyd` vault key, `vault.db`, the save bar, fill on action, `about:passwords`, the generator, the primary password; private windows with an ephemeral store and blocking | The attack harness: webd's page and network processes cannot read `vault.db` or the key (`keyd` refuses their labels), a `listupdate` child is refused by `keyd`; a cross-origin fill, an `http:` downgrade fill and a fill after a navigation are all refused (host tests and a harness page); a private window's cookie is not seen by the normal window and is gone after it closes; no password in the serial log, the session record or the volume outside `vault.db` |
| **B4: report an issue** | `idl/agent_report.midl`, the browser's form, the fallback; LazyAgent's handler when its P6 lands | Against a stub service: the report reaches it with only the ticked attachments, and the host is stripped; with LazyAgent at P6: a hostile summary (an image link, raw HTML, a foreign URL) comes out filtered in the posted body, and nothing reaches GitHub without the click |

B2 and B3 can overlap: the vault needs only the record log of 4.1. B4's browser half can be built against a
stub service as soon as the IDL exists.

## 10. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Real pages cost far more than the measured 28.7 MiB per view | 20 tabs do not fit the 8 GB box | Per-site processes, background tabs unpainted, discard by free memory, lazy restore; numbers measured in B1 before they are chosen |
| One site's process holds many tabs | a hung page takes its site's tabs down | group recovery reloads them; other sites are untouched |
| The parked-view pool is harder than it looks (WebKit keeps a proxy per ended page, patch `0021`) | webd grows as tabs come and go | B1's gate counts webd's views and memory after 200 cycles; the pool lands before tabs ship |
| No injected bundle in the port (S3) | B3's capture and fill need engine code | a minimal bundle in `WebProcess`; if that is refused, fill through a script in an isolated world and capture through a submit listener there |
| Losing `window.opener` | sign-in pop-ups that report back fail | documented; the held-`NewWindowRequested` item after B4 |
| The window is not sandboxed | vault plaintext lives in it briefly | zeroed buffers; the fill code small and fuzzed; the key never leaves the window; no untrusted image or HTML decoded in it except the fuzzed bookmark import |
| Third-party cookies blocked by default | some single sign-on flows break | per-site exceptions (spike S4); until then the setting can be turned off |
| A report leaks page data | private URLs end up in a public repository | host only by default, every attachment previewed, LazyAgent's redaction and renderer filter |
| A crashed webd blanks every tab | all windows lose their pages | the reconnect path exists (`lazyweb.md`); tabs keep their URLs and reload on reconnect |

## 11. Open questions

| # | Question | Answer taken (change it only if a measurement or the owner says otherwise) |
|---|---|---|
| 1 | SQLite (`places.db`, FTS5) or a pure-Rust store | a pure-Rust record log with an in-memory index for v1 (4.1): no zig, no WAL; SQLite when a feature needs queries |
| 2 | Is 16 app secrets enough with a primary password | yes: the vault key and the wrapped key are two (the owner's answer) |
| 3 | Which parts of `xui-app/web` move | none: LazyWeb went in webd P3 |
| 4 | The search template in `confd` or the profile | `confd`, `user/<uid>/lazyweb/search/template`, beside the blocker's keys: the user's, out of the engine's reach |
| 5 | Does LazyAgent accept reports from any app | yes, a generic `os.lazy.agent.report.v1`; the reporting app is the kernel-stamped label, never a field |
| 6 | The discard threshold on the 8 GB box | measured in B1 (2.5) |
| 7 | One web process per tab, or per site | per host for now, chosen by the client inside its own group (`process_key`, 2.3); per registrable domain once a public-suffix list ships |
| 8 | Several windows: one process or one each | one process (one session writer, one webd connection), if spike S1 shows xui can; otherwise one window with tabs first |

## 12. Conventions (AGENTS.md)

Every new interface or method in `.midl` (`idl/webd.midl`, `idl/agent_report.midl`), with `explain` rows, the
devtool classification and the Rhai docs regenerated. Evidence through `xui_app::trace` (`Channel::LazyWeb`) and
webd's channel, with no secret in any trace (the lint's channel list gains `LazyWeb` for the vault's lines).
Paths from `libs/fhs` (`fhs::app_data_dir`). Files under 500 lines (`app.rs` and `chrome.rs` will be split by the
tab strip). No new build switch: everything ships in `LAZYOS_LAZYWEB=1`, and the permissions it adds
(`sysmond`'s memory topic, `os.lazy.keyd.appsecret.v1` and `os.lazy.accounts.v1` for B3, `os.lazy.init.v1` and
`os.lazy.agent.report.v1` for B4) are derived from a `LAZYOS_LABEL_TRACE=1` run of the harness. The
GPLv2-compatible check on the window's crates (`python tools/nettls/licenses.py --lazyweb`) covers every new
dependency (`chacha20poly1305`, `zeroize`, the HTML import parser). webd changes keep their host tests against the recording engine and the in-memory fabric, and a seeded
soak for the group and pool state. The pre-PR review applies to each phase.
