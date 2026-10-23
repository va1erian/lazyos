# LazyCal: a calendar for LazyOS

**Status:** draft, revision 2 (2026-10-19). Nothing is built. Revision 2
took in a pre-PR review: no loopback in the guest (sign-in pastes the
redirect in v1), how Mail resolves an app-served name (a pkgstore rule),
caller-label authorisation instead of an imagined gesture token, valid
MIDL, the `events.import` path, esMail's SMTP builder, DKIM before trusting
a REPLY, and smaller corrections. §12 lists what the survey established;
§13 lists the questions phase C0 must answer before code that depends on
them is written.

LazyCal (`os.lazy.lazycal`, binary `xui-lazycal.elf`, menu name
**Calendar**, trace channel `Lazycal`, markers `LAZYCAL:*` as
`tools/xui/new_app.py lazycal` writes them) is a desktop calendar. It keeps local
calendars, subscribes to iCalendar feeds, **syncs both ways with Google
Calendar**, and **answers meeting invitations that arrive in Mail
(esMail)**. Its window looks and behaves like Google Calendar (§7), so a
user of the web app finds their way around without reading anything.

Like LazySky and LazyAgent it is a **headless core plus a thin UI** (§2):
the core runs in the LazyOS window, in a host CLI and in host tests.

**Why Rust and not a LazyRAD form.** The "Rhai first" rule does not fit.
The window is a custom-painted time grid with drag, resize and overlap
layout; sync runs on threads of its own over HTTPS; iCalendar parsing and
recurrence expansion are heavy data work. Those are the rule's reasons for
Rust.

Read §3 (trust) and §11 (rules) before writing code.

---

## 1. Goals

1. **A calendar people can live in.** Day, week, month, year and schedule
   views; create, edit, move and delete events; recurring events with
   "this / this and following / all" edits; reminders as toasts; search.
2. **Google Calendar, both ways.** Sign in with a Google account, see every
   calendar in its list with its colours, edit offline, and have changes
   reach Google (and Google's changes reach LazyCal) within a minute of the
   network being up. RSVPs made in LazyCal are the organiser's RSVPs on
   Google.
3. **Invites from Mail.** A message carrying an iTIP request (`text/calendar;
   method=REQUEST`) shows an invite card in Mail's reading pane: when, where,
   who, what it collides with, and **Yes / Maybe / No**. Answering updates
   the calendar and tells the organiser, through Google when the event lives
   there, or by an iMIP reply that Mail sends otherwise. Cancellations and
   updates are handled the same way.
4. **Standard data.** iCalendar (RFC 5545) in and out: import a `.ics`,
   export a calendar, subscribe to `webcal:`/`https:` feeds. Files open it
   by double-click through `mimed`.
5. **Never blocks the UI.** LazySky's R1: the UI thread does no network, no
   parsing, no recurrence expansion over an unbounded range and no file I/O.
6. **No new ambient authority.** The app holds no capability. Mail reaches
   it through one interface in LazyCal's own namespace, granted by Mail's
   manifest and checked against Mail's kernel-stamped label (§6).

**Non-goals for v1:** CalDAV servers other than Google's (the provider trait
leaves room, §5.4), Exchange/EWS, tasks (VTODO), shared-calendar ACL
editing, booking pages, Meet link creation (an existing `hangoutLink` is
shown and opened), and appointment scheduling across other people's
free/busy beyond what Google's `freeBusy` returns.

## 2. Architecture

```
 xui window (lazycal-ui)                 lazycal-core (no xui type)
 ───────────────────────                 ───────────────────────────────────
 top bar / sidebar / time grid ───────▶ Session: calendars, edits, queries
 popovers, edit page            ◀────── Events (drained from a timer)
                                          │ store thread ─ files ─▶ $HOME/.apps/os.lazy.lazycal
                                          │ sync thread ── HTTPS ─▶ www.googleapis.com/calendar/v3
                                          │ feed thread ── HTTPS ─▶ subscribed .ics URLs
                                          └ alarm thread ─ clock ─▶ notify toasts (via Platform)
 invites service (app.os.lazy.lazycal.invites) ◀── Mail (xui-mail): Preview / Respond / Status (§6)
```

* **`xui-app/crates/lazycal-core`** (new): the event model, the iCalendar
  reader and writer, recurrence expansion, time zones, the store, the
  provider trait and its Google and ICS-feed implementations, the sync
  engine, iTIP processing (REQUEST/REPLY/CANCEL) and alarm scheduling. No
  xui type and no LazyOS crate; everything OS-specific is behind a
  `Platform` trait (as in lazysky-core): `Secrets`, `Clock`, `Notifier`,
  `Opener` (`mimed`), `app_data_dir`.
* **`xui-app/crates/lazycal-ui`** (new): the window (§7) on xui, with a
  `desktop` example for Windows/Linux/macOS like LazySky's.
* **`xui-app/src/bin/lazycal.rs`**: launches them, supplies the LazyOS
  `Platform`, serves the invites interface (§6) and prints the `LAZYCAL:*`
  evidence through `xui_app::trace`.
* **`xui-app/crates/https`** (`lazyhttps`, exists): TLS, roots, resolver
  and the capped blocking request path. LazyCal is its third user; it is
  not copied. Its public API is `get`, `get_following`, `post` and
  `post_stream` today (`send` is private, `src/http.rs`); C4 adds `patch`,
  `put` and `delete` with request headers (`If-Match`). Google requests use
  `Policy::https_hosts` (`www.googleapis.com`, `oauth2.googleapis.com`);
  feeds use the any-host policy but refuse private, loopback and gateway
  addresses.
* **`xui-app/crates/lazycal-core/examples/cal_cli.rs`**: a host CLI
  (`import`, `expand`, `export`, `google login|sync|list`) used by the P0
  spikes, the tests and anyone debugging sync without QEMU.

### 2.1 Threads

| Thread | Owns | Talks to |
|---|---|---|
| UI | the window, a read-only snapshot of the visible range | core through a command queue and an event queue |
| store | the files on disk, the in-memory index | everyone else by messages |
| sync | one Google account's sync state, the outgoing queue | `lazyhttps`, the store |
| feed | subscribed `.ics` URLs | `lazyhttps`, the store |
| alarm | the next N alarm instants | `Clock`, `Notifier` |
| invites | the Messenger server loop for §6 | the store, the sync thread |

The UI asks for "instances in [start, end) for these calendars" and gets a
`Vec<Instance>` back on the event queue; it never expands an RRULE itself.

## 3. Trust model

* **Everything that arrives is untrusted**: `.ics` files, feeds, invite
  attachments in mail, and Google's JSON. The parser has caps per component
  (256 properties, 64 KiB per unfolded line, 1 MiB per component) and per
  file (equal to the `max_body` LazyCal sets on `lazyhttps`, 32 MiB, above
  the crate's 8 MiB default so a large Google feed fits), never recurses on input, and is fuzzed (§10.1). 75
  octets is the folding target when *writing*; real Outlook and Google
  files exceed it and are read anyway.
* **Recurrence is bounded.** A rule with no `COUNT` or `UNTIL` is infinite;
  expansion is always for a window and stops at 5 000 instances per series per
  query, with `EXDATE`/`RDATE`/overrides applied inside the window. A rule
  whose step can never land (`BYMONTHDAY=31;BYMONTH=2`) ends after a bounded
  number of empty periods, never loops.
* **Nothing is added behind the user's back.** An invite from Mail is shown,
  never written, until the user answers. (Google adds invitations sent to a
  Gmail address to the calendar by itself; LazyCal shows those as
  "Awaiting reply", as Google does.)
* **Spoofed organisers.** `From` is trivially forged and esMail checks no
  signatures, so LazyCal trusts a sender only with the receiving server's
  `Authentication-Results: dkim=pass` for the sender's domain (Mail passes
  the header; C0 checks Gmail and common providers add it). A REQUEST whose
  `ORGANIZER` differs from `From` (allowing a matching `SENT-BY` delegate),
  or whose sender has no DKIM pass, gets a warning line on the card. A
  "Google calendar notification" is recognised by a DKIM pass for
  `google.com` and the `calendar-notification@google.com` sender, never by
  the From domain alone. A REPLY or CANCEL is applied only on a click, and
  the card warns when its sender is not the attendee or organiser it claims
  to be.
* **Descriptions are text.** Google descriptions may hold HTML; v1 shows them
  as text with links detected. A link opens only on a click, through `mimed`.
  No remote content is ever fetched for an event.
* **Secrets.** The Google refresh token is the only secret. It never reaches
  argv, the environment, a file or a trace (the `LAZYCAL` channel joins the
  trace lint's secret list). Where it is kept is §5.2.
* **The invites interface** (§6) can read the user's calendar (overlaps)
  and change it. Listing it in a manifest is not enough: LazyCal serves it
  only to callers whose kernel-stamped label is `app:os.lazy.mail` (§6.3),
  so another app cannot probe the calendar with made-up `.ics` windows.

## 4. The data model and files

* **Calendar**: id, provider (`local`, `feed`, `google:<account>`), name,
  colour, time zone, access role (owner/writer/reader/freeBusy), visible,
  default reminders.
* **Event**: provider id + iCalendar `UID`, `RECURRENCE-ID` for overrides,
  summary, description, location, start/end (date for all-day, else instant
  + `TZID`), `RRULE`/`RDATE`/`EXDATE`, transparency, visibility, status,
  organiser, attendees with `PARTSTAT`, reminders, colour id, conference
  link, `SEQUENCE`, etag, `LAST-MODIFIED`, a local `dirty` flag.
* **Instance**: (event, occurrence start) after expansion. Only instances in
  a requested window exist.

Storage (`fhs::app_data_dir(home, "os.lazy.lazycal")`): one directory per
calendar holding `calendar.toml`, `events.ics` (the calendar as a
standard iCalendar file, rewritten atomically with rename) and, for synced
calendars, `sync.toml` (sync token, last full sync) and `outbox.jsonl`
(pending writes, appended before the UI shows the change as saved).
Calendars are small (thousands of events), so the store loads every
calendar into memory at start and keeps an interval index; no SQLite, so
no zig and no WAL question (see docs/mail.md). The `.ics` files are
also the export format and are readable with any calendar tool.

### 4.1 Time zones

`libs/timed` resolves ten built-in zones (`zones.rs`), and `timed.SetZone`
refuses any other name, so the system zone is one of those ten until
`timed` moves to the full database. That is enough for the clock and far
too few for a calendar: Google and Outlook invites name any IANA zone (Outlook
uses Windows names, mapped through CLDR's `windowsZones` table). LazyCal
needs the full tz database:

* **C0 decides** between `chrono-tz` (compiled-in tables, MIT/Apache) and
  TZif files in the image read by a small reader (`/system/share/zoneinfo`,
  a new `fhs` constant, written by the image build from a pinned tzdata
  release). The second lets `timed` and Settings use the same data later and
  updates tzdata without relinking apps; the first is simpler. Measure the
  size both ways.
* `VTIMEZONE` blocks in incoming `.ics` are parsed and used only when the
  `TZID` is unknown (custom zones from old Outlooks).
* The view's zone is the system zone (`sys/time/zone` via `timed`), with a
  per-user override and an optional secondary zone column as in Google.

## 5. Providers and sync

```rust
trait Provider {
    fn list_calendars(&mut self) -> Result<Vec<RemoteCalendar>>;
    fn changes(&mut self, cal: &CalId, token: Option<&SyncToken>) -> Result<Changes>; // full when token is None or stale
    fn put(&mut self, cal: &CalId, ev: &Event, if_match: Option<&Etag>) -> Result<Etag>;
    fn delete(&mut self, cal: &CalId, id: &EventId, if_match: Option<&Etag>) -> Result<()>;
    fn respond(&mut self, cal: &CalId, id: &EventId, partstat: PartStat, note: Option<&str>) -> Result<Etag>;
}
```

### 5.1 Google Calendar API v3

REST + JSON over `lazyhttps` (blocking, on the sync thread).

* **Read:** `calendarList.list` (names, colours, access role, default
  reminders), `colors.get`, `events.list` with `singleEvents=false` (series
  are expanded locally) and `showDeleted=true`, paged through
  `nextPageToken` (the `nextSyncToken` comes only on the last page), then
  **incremental sync with `syncToken`**; a `410 Gone` drops the token and
  does a full sync.
* **Write:** `events.insert`, `events.patch` with `If-Match: <etag>`,
  `events.delete`; `sendUpdates=all` when the user chose to notify guests
  (Google's own dialog, §7.4). RSVP is a patch of the user's own attendee
  `responseStatus`; a patch replaces the whole `attendees` array, so it
  sends the full list with only the `self: true` entry changed.
* **Conflicts:** a `412 Precondition Failed` fetches the server copy; if the
  fields the user changed were not changed remotely, the edit is reapplied
  on top; otherwise the server wins and the user's version is shown in a
  "Your changes could not be saved" bar with *Keep mine* / *Keep theirs*.
* **Schedule:** sync at start, every 60 s while the window is open, every
  5 min when resident in the tray, at once after a local edit, and on
  `netd`'s "default route up". Push channels (`events.watch`) need a public
  HTTPS endpoint and are not used.
* **Backoff:** `429`, `403 rateLimitExceeded|usageLimitsExceeded` and
  `5xx` back off exponentially with jitter up to 15 min;
  `401` refreshes the access token once, then asks the user to sign in again.
* **Offline:** edits go to `outbox.jsonl` first and are replayed in order;
  the sidebar shows "Offline: 3 changes waiting".
* **Recurring edits:** "this event" is an override instance; "this and
  following" splits the series (old `RRULE` gets an `UNTIL`, a new series
  starts); "all events" patches the master.

### 5.2 Signing in

esMail implements Google OAuth for installed apps (RFC 8252, loopback
redirect on `127.0.0.1:<port>` with PKCE, `crates/esmail/src/oauth.rs` at the
pinned revision, on tokio). **The guest has no loopback interface** (`netd`
has none; docs/networking-plan.md lists it as not built), so a redirect to
`127.0.0.1` cannot reach a listener in LazyCal. v1 therefore uses the same
installed-app flow with the redirect **pasted**:

1. The sync thread builds the consent URL (PKCE, random `state`, scope
   `https://www.googleapis.com/auth/calendar`, redirect
   `http://127.0.0.1:<random port>`) and asks `mimed` to open it in the
   user's browser (LazyWeb; Google's sign-in needs full JavaScript).
2. After consent the browser fails to load the redirect, but its address bar
   holds `?code=…&state=…`. LazyCal's sign-in dialog asks the user to paste
   that address; it checks `state` and exchanges the code (with the PKCE
   verifier) on the sync thread.
3. The account name is the primary calendar's id (the account's address),
   so no `openid email` scope is asked for. The refresh token is kept as in
   §3; access tokens live in memory only.

Two later improvements remove the paste, each its own change: a netstack
loopback interface (then the listener works as in esMail), or a LazyWeb
hand-off that sends a navigation to `http://127.0.0.1:<port>/?code=` for a
registered port to LazyCal. The OAuth device flow is not an option: Google
does not allow the `calendar` scope for "TV and limited input" clients.

**The client id is the user's.** Google issues tokens only to registered
clients, so like esMail LazyCal ships none: LazyCal's own settings page
takes a "Desktop app" client id and secret (kept in `confd` at
`user/<uid>/lazycal/google/client`; the desktop secret is not confidential).
A build may preset one with `LAZYOS_GOOGLE_CLIENT_ID` for a developer's own
image; CI never sets it. A user-created client left in Google's "Testing"
publishing status gets refresh tokens that **expire after 7 days** for a
sensitive scope like `calendar`; the settings page says so, and a
`invalid_grant` on refresh asks for a new sign-in.

**Where the refresh token lives.** keyd app secrets
(`os.lazy.keyd.appsecret.v1`, LazyAgent plan §5.3, P2b) are exactly this.
Until they land, the token is in memory and the user signs in once per
session (Mail's current model). C4 does not wait for P2b; "Stay signed in"
appears when it lands (subject to the 7-day limit above).

**If Google refuses LazyWeb** as an embedded browser (C0), the user signs
in on another machine and pastes the redirected address the same way.

### 5.3 Feeds (read-only)

`webcal:`, `http:` and `https:` iCalendar URLs, refreshed every 30 min with
`If-None-Match`/`If-Modified-Since`. This is also a sign-in-free way to see a
Google calendar: Google's "secret address in iCal format". A feed URL may
embed a secret, so it is treated like a password in traces. A `webcal:`
link opened through `mimed` shows a "Subscribe to <host>?" dialog first;
nothing is subscribed without that click.

### 5.4 Later providers

CalDAV (Google, Fastmail, Nextcloud, iCloud) fits the same trait with
`sync-collection` (RFC 6578) and ETags. Not in v1; the trait and the
store's etag/sync-token columns are shaped for it.

## 6. Mail integration

### 6.1 What the user sees

When the reading pane opens a message with a `text/calendar` part (inline
or attached, `method=REQUEST|CANCEL|REPLY|PUBLISH`), Mail draws an invite
card above the body, like Gmail's:

```
┌──────────────────────────────────────────────────────────────┐
│ ▣ Design review                                              │
│   Thu 23 Oct · 14:00 – 15:00 (Europe/Paris)  · Room 2 / Meet │
│   Organiser: alice@example.com · 4 guests                    │
│   ⚠ Overlaps "1:1 with Bob" (14:30)                          │
│   Going?   [ Yes ]  [ Maybe ]  [ No ]   More options ▾       │
│   Add to: [ Work (Google) ▾ ]                    Open in Calendar │
└──────────────────────────────────────────────────────────────┘
```

* **REQUEST**: the card above. After an answer it reads "You replied Yes",
  with a link to change it. An update (higher `SEQUENCE`, or the same
  `SEQUENCE` and a later `DTSTAMP`, RFC 5546 §2.1.5) says what changed
  (time, place).
* **CANCEL**: "This event has been cancelled" and *Remove from calendar*.
* **REPLY** (to the user as organiser): "Bob accepted" and *Update guest
  status*; applied on that click only, only for events the user organises,
  with the DKIM warning of §3 when the sender is unproven.
* **PUBLISH**: *Add to calendar*.
* **COUNTER, REFRESH, ADD, DECLINECOUNTER**: "This calendar message is not
  supported" and *Open in Calendar* (import as a file).

Google sends both an inline `text/calendar` part and an `invite.ics`
(`application/ics`) attachment; Mail dedupes by `UID` and prefers the part
that carries a `METHOD`. "More options" carries *Reply with note* and
*Don't send a reply*.

### 6.2 The interface (`idl/lazycal.midl`)

LazyCal registers `app.os.lazy.lazycal.invites` and advertises
`os.lazy.lazycal.invites.v1` (its own domain, docs/security-model.md
"Namespaces"); the bin serves it on a thread while the app runs.

**Mail must be able to resolve that name, and today it cannot.** A
manifest `interfaces` entry grants resolving the interface name, that name
without `.vN`, and the extra names in pkgstore's `SERVICE_NAMES`
(`libs/pkgstore/src/rules.rs`); listing `os.lazy.lazycal.invites.v1`
grants `os.lazy.lazycal.invites`, which LazyCal may not register. No app
serves another app yet, so this is a first. Phase C6a adds a pkgstore
rule: an interface `<id>.<name>.vN` of an app's domain is also served as
`app.<id>.<name>`, plus a `pkgstore::explain` entry rating
`os.lazy.lazycal.invites.v1` MEDIUM ("Read and answer calendar
invitations") — an interface missing from that table is rated **high**
(docs/packages.md), and the test that fails when `idl/` declares an
unknown interface must keep passing.

**Running when Mail needs it.** LazyCal is a **resident app** with
`autostart = true` (`[entry]`): it starts at login, has a tray icon and
lives without a window (it needs that for reminders too, §8). The user can
still Quit it from the tray; then the name does not resolve and Mail shows
only the *Open in Calendar* fallback (§6.3). Mail holds no `init.Launch`
permission and does not start it.

```
interface os.lazy.lazycal.invites.v1 {
    enum Method { Request, Cancel, Reply, Publish, Unsupported }
    enum Answer { Accept, Tentative, Decline, Apply, NoReply }
    enum Outcome { Handled, SendReply, Refused }

    struct Overlap { summary: String = 1, start_ms: I64 = 2, end_ms: I64 = 3 }
    struct Target { calendar_id: String = 1, name: String = 2, suggested: Bool = 3 }

    // Parse one iCalendar object from a message; nothing is written.
    method Preview(ics: Bytes = 1, from: String = 2, to: Array<String> = 3, dkim_pass: Bool = 4)
        -> (method: Method = 1, summary: String = 2, start_ms: I64 = 3, end_ms: I64 = 4,
            all_day: Bool = 5, zone: String = 6, location: String = 7, organizer: String = 8,
            guests: U32 = 9, newer_than_stored: Bool = 10, overlaps: Array<Overlap> = 11,
            targets: Array<Target> = 12, warnings: Array<String> = 13) = 1;
    // Answer a REQUEST, apply a CANCEL/REPLY/PUBLISH. Says what Mail must send, if anything.
    method Respond(ics: Bytes = 1, from: String = 2, account: String = 3, answer: Answer = 4,
                   calendar_id: Option<String> = 5, note: Option<String> = 6,
                   dkim_pass: Bool = 7)
        -> (outcome: Outcome = 1, reply_to: String = 2, reply_subject: String = 3,
            reply_ics: Bytes = 4) = 2;
    // The user's current answer for a UID, for a card drawn again later.
    method Status(uid: String = 1, recurrence_id: Option<String> = 2)
        -> (known: Bool = 1, answer: Answer = 2) = 3;
}
```

MIDL has no sum types, so `Respond`'s reply is a flat tuple with an
`Outcome` (the `reply_*` fields are empty unless it is `SendReply`). `ics`
fields obey the messenger rule "never a `Bytes` field over 64 KiB"
(docs/messenger.md): an invite larger than 64 KiB (rare: a huge
description or an embedded attachment) is refused with a "Too large, open
it in Calendar" card. The sketch follows docs/midl.md's grammar; `midlc`
compiles the real file in C6a. `dkim_pass` is in `Respond` too, so the
sender check of §3 is made again when a REPLY or CANCEL is applied.

`SendReply` is an iMIP reply (RFC 6047) that Mail sends through its own
SMTP account, `multipart/alternative` with a text part and
`text/calendar; method=REPLY`.

Which path a response takes:

| The event | Answer goes |
|---|---|
| is already on a Google calendar of the receiving account (looked up with `events.list?iCalUID=`; Google usually adds invites from Google organisers by itself) | `events.patch` of the attendee's own `responseStatus` (§5.1). For a Google organiser, Google tells them and Mail sends nothing (`Handled`). For an external organiser, whether Google emails the reply is checked in C0; until then LazyCal also returns `SendReply` |
| is not on Google yet, user picks a Google calendar | `events.import` of the invite (a private copy: Google will **not** tell a non-Google organiser), with the answer set, then `SendReply` |
| goes to a local calendar | stored locally, then `SendReply` |
| `Don't send a reply` | stored, `Handled` |

### 6.3 Who may call it

`Respond` changes the user's calendar and speaks for them, and `Preview`
returns overlapping events, so the interface is served to **one caller**:
LazyCal reads the kernel-stamped label of each request and refuses any
label but `app:os.lazy.mail` (`Refused`, and a `LAZYCAL:INVITE:DENY`
trace with a budget). Mail calls `Respond` only from the card's click
handler. A cross-process proof of a user gesture would be stronger, but no
such token exists (nothing in `idl/` for display or input carries one);
it would be an `xuid` interface of its own (minted on input delivery,
checked against the caller's identity, age and single use) and is out of
scope here.

An image without LazyCal never resolves the name, and Mail falls back to an
*Open in Calendar* button that hands the part to `mimed` (`text/calendar`,
verb `open`), or to nothing when no app handles that type.

### 6.4 Mail-side work (`xui-app/mail/src/app/`)

* `invite.rs`: find the `text/calendar` part in the raw message (esMail's
  `mailparse` walk in `render.rs` already visits every part), read
  `Authentication-Results` (only the topmost header whose authserv-id is
  the account's own IMAP provider; a sender can put a forged pass line
  lower in the message), call `Preview` on a worker thread, draw the
  card, call `Respond` on a click, and send a `SendReply`.
* **esMail cannot send an iMIP reply today**: `smtp::build_message` builds
  only `multipart/mixed` with a plain body and attachments
  (`crates/esmail/src/smtp.rs` at the pinned revision). C6 needs an upstream
  esMail change (an alternative `text/calendar; method=REPLY` part) and a
  re-pin of `xui-app/mail/Cargo.toml`, which also keeps the Windows client
  able to answer invites later.
* Status cache per message so the card does not flicker on revisit.
* Markers `MAIL:INVITE:<method>:<uid-hash>` and `MAIL:INVITE:REPLIED:<answer>`
  through `xui_app::trace` (Mail already depends on `xui-app`); Mail's
  existing markers are still `println!` (baseline entries in
  `tools/trace/adhoc_baseline.json`), and the ratchet must not grow.
* Mail's manifest gains `os.lazy.lazycal.invites.v1`. It resolves only
  once C6a's pkgstore rule and `explain` entry are in; without them the
  entry would be rated high and the name would not resolve.
* esMail upstream: the card could later move into esMail's view model so the
  Windows client gets it too; v1 keeps it in the LazyOS window.

### 6.5 Other directions

* **Email guests** in an event's popover opens `mailto:` with the attendees
  (Mail already handles it).
* **Sending invitations from a local calendar** (iMIP REQUEST/CANCEL) needs
  Mail to send on LazyCal's behalf: a second, Mail-owned interface
  (`os.lazy.mail.send.v1`, `Send(to, subject, text, ics)`, served only to `app:os.lazy.lazycal` and confirmed in a Mail dialog).
  Phase C7; Google calendars do not need it.

## 7. The window (Google Calendar's layout)

### 7.1 Layout

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ ☰  ▣ Calendar   [Today]  ‹ ›   October 2026          🔍  ⚙   [ Week ▾ ]  ⓖ │
├──────────────┬──────────────────────────────────────────────────────────────┤
│ [ + Create ] │        SUN 19   MON 20   TUE 21   WED 22   THU 23  FRI  SAT  │
│              │ GMT+2  ─────── all-day strip ──────────────────────────────  │
│  October 2026│  9 AM ┌──────┐                                              │
│  S M T W T F S│ 10 AM │Stand-│          ┌────────┐                         │
│  …mini month…│ 11 AM └──────┘          │Design  │                          │
│              │ ━━━━━━━━━━━━━●━━━━━━━━━━━ now line ━━━━━━━━━━━━━━━━━━━━━━━━  │
│ ▾ My calendars│ 12 PM                   └────────┘                         │
│  ☑ ■ Personal│  …                                                          │
│  ☑ ■ Work    │                                                              │
│ ▾ Other      │                                                              │
│  ☑ ■ Holidays│                                                              │
└──────────────┴──────────────────────────────────────────────────────────────┘
```

* **Top bar:** sidebar toggle, app name, *Today*, previous/next, the period
  title, search, settings, the view switcher (Day, Week, Month, Year,
  Schedule, 4 days), the account avatar with sync status.
* **Sidebar:** *Create*, the mini month (click a day to go there; the shown
  range shaded; Google's "Search for people" is left out), *My calendars* and *Other
  calendars* with colour checkboxes, a hover menu per calendar (*Display this
  only*, colour, settings, export), *+* to add a calendar or subscribe.
* **Day / week / 4 days:** an hour grid with a sticky header and an all-day
  strip, the current-time line on today, events as rounded colour blocks laid
  out in columns when they overlap (Google's packing: an event widens into
  free columns to its right), declined events struck through, tentative and
  unanswered ones outlined.
* **Month:** six weeks, up to N chips per day then "+3 more" opening a day
  popover; multi-day events span cells.
* **Year:** twelve mini months, days with events dotted.
* **Schedule:** an agenda list grouped by day, virtualized like LazySky's
  feed.

### 7.2 Interaction

* Click an empty slot → **quick-create popover** (title, time, calendar,
  *More options*); drag across slots to pick a range first.
* Click an event → **detail popover**: title, time and recurrence, location
  (a link opens maps through `mimed`), conference link, guests with their
  RSVP icons, the user's own *Going? Yes / No / Maybe*, description,
  calendar, and *Edit*, *Delete*, *Email guests*, *Duplicate*.
* **Edit page:** the full form (Google's "Event details" tab): all-day,
  time zone, repeat (presets and a *Custom* dialog building an RRULE),
  location, notifications, colour, busy/free, visibility, guests with
  permissions, description.
* **Drag** moves an event, dragging its bottom edge resizes it, in 15-minute
  steps; a recurring event asks "This event / This and following / All
  events"; a Google event with guests asks "Send update emails?".
* **Keyboard:** Google's shortcuts: `t` today, `j`/`k` or `n`/`p` next and
  previous, `d` `w` `m` `y` `a` `x` views, `c` create, `e` edit, `Del`
  delete, `/` search, `g` go to date, `?` shortcut list, `Esc` closes.
* **Undo:** a snackbar ("Event deleted · Undo") for 8 s after a delete or a
  move, as Google does.

### 7.3 Look

Google's material spacing and colour palette (the eleven event colours and
the 24 calendar colours of `colors.get`), adapted to xui's theme: the light
theme is close to Google's, the dark theme follows `uitheme`. Pixel
constants go through `xui_app::hidpi` so the 2x desktop is sharp. The grid
is one custom-painted view; per LazySky's rule, **no clip is pushed on the
normal paint path**; event text is shaped on a layout thread and cached
per (event, width).

### 7.4 Dialogs Google has and LazyCal copies

"Send update emails to guests?" (*Send* / *Don't send*), "Delete recurring
event" (three choices), "Discard unsaved changes?", and the conflict bar of
§5.1.

## 8. Reminders and the tray

The alarm thread keeps the next 64 alarm instants (popup reminders of every
visible calendar, Google's default reminders included) and wakes on the
earliest; the minute topic `time/tick` and a clock step from `timed`
rebuild it. A reminder is an `os.lazy.notify.v1` toast at Info: title,
time, location, with *Open* and *Snooze 5 min*; it is never the only way to
reach the event (the notify plan's rule). The tray icon shows today's date;
its menu lists the next three events, *Open Calendar*, *New event*, *Quit*.

## 9. Packaging and wiring

* Package `xui-app/packages/lazycal/` (listed in `tools/xui/core_packages.py`),
  `system_name = "os.lazy.lazycal"`, `category = "office"`,
  `resident = true`, `autostart = true`, `network = ["outbound"]`,
  `[[mime]]` for `text/calendar` (`open`, `import`) and
  `x-scheme-handler/webcal` (`open`). Interfaces: display, cursor, input,
  `mimed`, clipboard, notify, `timed`, `confd` for its own keys, and (once
  it exists) keyd app secrets. Derived from a `LAZYOS_LABEL_TRACE=1` run,
  not by hand.
* Build switch `LAZYOS_LAZYCAL=1`, which needs `LAZYOS_DESKTOP=1` and
  `LAZYOS_NETD=1`, exactly like LazySky (`build_support/lazysky_embed.rs`):
  the CA bundle is in every image and `lazyhttps` is linked in, so
  `LAZYOS_TLS=1` (the `curl`/`wget` programs) is not needed.
  `run_demo.py --lazycal` implies them; the GUI launcher gets *Calendar* on
  the Simple tab and the raw switch on the Advanced tab, wired in
  `tools/lazygui/catalog.py` with tests.
* Start from `python tools/xui/new_app.py lazycal --name "Calendar" ...`,
  which writes the `Lazycal` channel row (`LAZYCAL:*` markers), icons and
  registrations.
* Licence: GPL-3.0-or-later like `lazyhttps`; every dependency is checked
  with `tools/nettls/licenses.py` (a `--lazycal` set), no ring, aws-lc or
  tokio (so esMail's OAuth code is a model, not a dependency).

## 10. Phases

| Phase | Builds | Done when |
|---|---|---|
| **C0** spikes | §13 answered in this document: Google sign-in in LazyWeb with the pasted redirect; whether Google emails an API-made RSVP to an external organiser; whether `Authentication-Results` reaches Mail; tz data choice and size; iCalendar parser choice (in-house vs `icalendar`/`calcard` crates) by fuzz robustness; RRULE expansion against python-dateutil | each question has an answer and a measured number |
| **C1** core | model, iCalendar read/write, RRULE/EXDATE/overrides, time zones, store with atomic files, local calendars, `cal_cli` | host tests, differential RRULE test, seeded fuzz of the parser, round-trip of a corpus of real `.ics` files (Google, Outlook, Apple, Thunderbird exports) |
| **C2** window | §7.1–7.3 for local calendars: views, popovers, edit page, drag/resize, keyboard; package, switch, `run_demo.py --lazycal`, GUI; `.ics` import via Files | offscreen snapshots of every view in both themes and at 2x; session `lazycal.json` creates, moves and deletes events by widget name; `LAZYCAL:UP:PASS` |
| **C3** feeds | §5.3 subscriptions, `webcal:` handler with its confirmation | harness serves a changing feed under the test CA; the session sees the change after refresh |
| **C4** Google | §5.1–5.2: `lazyhttps` `patch`/`put`/`delete`, pasted-redirect sign-in, calendar list, paged and incremental sync, writes, conflicts, outbox, backoff | `tools/lazycal/run.py` with a **mock Google Calendar server** on the host (`tools/lazycal/gcal_mock.py`: OAuth token endpoint, calendarList, paged events with sync tokens, etags, 410/412/429/403-rate faults) behind the TLS harness; `--live` against a real test account by hand |
| **C5** reminders | §8: resident app, autostart, tray, toasts, snooze | session advances the guest clock with `timectl` and judges `SHELL:NOTIFY:SHOW` |
| **C6a** app-served names | pkgstore rule (`<id>.<name>.vN` served as `app.<id>.<name>`), the `explain` entry and `idl/lazycal.midl` through `midlc`, in one change (the unknown-interface test needs the entry when the IDL lands) | `cargo test -p pkgstore -p lazypkg`; `midlc` generates the client and server |
| **C6b** esMail iMIP | upstream esMail: `text/calendar` alternative part in `smtp::build_message`; re-pin | esMail's own tests; the mock SMTP receives a well-formed REPLY |
| **C6** Mail invites | §6.1, §6.4: the service, Mail's card, DKIM handling, Google and iMIP answers, CANCEL/REPLY/updates, dedupe | `tools/mail/run.py --invites`: the mock IMAP server holds Google-, Outlook- and Thunderbird-style invites (with and without DKIM results); the judge checks the card, the calendar, the iMIP reply the mock SMTP received, and the mock Google server's lookup and patch |
| **C7** polish | search, year view, secondary zone, *Find a time* via `freeBusy`, `os.lazy.mail.send.v1` and invitations from local calendars, "Stay signed in" once keyd app secrets land, loopback or LazyWeb hand-off instead of the paste | per-feature sessions |

C1, C6a and C6b do not depend on each other and can run in parallel once
C0 has picked the parser. C6 needs C2 (a LazyCal binary to serve the
interface), C4 (Google answers), C6a and C6b; the label check and its
attack row (another app listing the interface is refused, `Refused` and
`LAZYCAL:INVITE:DENY`) are part of C6.

### 10.1 Verification

* Host: `cargo test --manifest-path xui-app/Cargo.toml -p lazycal-core -p lazycal-ui`;
  `FUZZ_CASES=20000 ... --release seeded` for the iCalendar parser, the
  RRULE expander and the Google JSON decoder; offscreen snapshots in
  `xui-app/target/snapshots/lazycal-*.png` (Read them).
* RRULE oracle: python-dateutil does not count a `DTSTART` that the rule
  itself would not produce, while RFC 5545 practice and Google do; the
  differential test adds `DTSTART` to dateutil's set, and its generated
  rules include DST gaps and overlaps in several zones.
* QEMU: the sessions above, judged by markers **and** screenshots (the grid
  is visual; AGENTS.md's rule applies).
* Never a real Google account in CI; the mock server is the CI peer.

### 10.2 Markers (channel `Lazycal`)

`LAZYCAL:UP:PASS`, `LAZYCAL:QUIT:PASS`,
`LAZYCAL:STORE:LOADED:<calendars>:<events>`, `LAZYCAL:VIEW:<kind>:<start>`,
`LAZYCAL:EVENT:SAVED|DELETED|MOVED:<hash>`,
`LAZYCAL:SYNC:<account-hash>:<calendar-hash>:full|delta:<in>:<out>`,
`LAZYCAL:SYNC:FAIL:<status>`, `LAZYCAL:AUTH:PASS|FAIL`,
`LAZYCAL:FEED:<n>:<events>`, `LAZYCAL:ALARM:<hash>`,
`LAZYCAL:INVITE:PREVIEW|RESPOND:<method>:<answer>`, `LAZYCAL:INVITE:DENY`.
Hashes, not titles or addresses: event content is the user's.

## 11. Rules for the implementing agent

1. The UI thread never does network, parsing, expansion or file I/O.
2. Every Messenger interface is in `idl/` and generated by `midlc`.
3. Paths come from `libs/fhs`; markers go through `trace`; files stay under
   500 lines (split the views, the editor and the sync engine early).
4. Expansion is always windowed and capped (§3); a test proves an infinite
   or impossible rule terminates.
5. Nothing in the store is lost to a crash: write the outbox before
   answering, files by write-and-rename; a test kills the store mid-write.
6. No secret, title, address or feed URL in a trace or an error toast.
7. A change that touches Google's wire format is checked against the mock
   **and** recorded responses from the live API, never one alone.
8. The invites service checks the caller's label on every request.
9. Pre-PR review per AGENTS.md for every phase.

## 12. Known facts from the survey (2026-10-19)

* esMail (pinned `a065da5`) has Google OAuth (loopback + PKCE, user-supplied
  client id, refresh-token rotation, on tokio) in `crates/esmail/src/oauth.rs`;
  on LazyOS, Mail says "OAuth sign-in needs a browser and is not offered"
  (docs/mail.md). It parses messages with `mailparse` and already walks every
  MIME part (`render.rs`, `extract_attachments`), so finding a
  `text/calendar` part is a small change. Its SMTP builder makes only
  `multipart/mixed` (`smtp.rs`), so it cannot send an iMIP reply yet.
* **The guest has no loopback interface** (docs/networking-plan.md; LazyAgent
  plan §12), so an OAuth loopback redirect cannot reach an app.
* Mail keeps passwords in memory only (`xui-app/mail/src/secrets.rs`); keyd
  has no per-app secret yet (LazyAgent plan §5.3, P2b proposes one; a
  session implementing it was started 2026-10-19).
* `lazyhttps` (`xui-app/crates/https`) is the shared blocking HTTPS client
  for GPL-3.0-or-later apps; it exposes GET and POST only.
* `timed` serves the system zone from `sys/time/zone`, publishes
  `time/tick` each minute; `libs/timed` knows ten zones and `SetZone`
  refuses others.
* An `app:<id>` label may register `app.<id>.<name>` and advertise
  `<id>.<name>.vN` interfaces (docs/security-model.md "Namespaces"), but a
  manifest entry does not let another app resolve `app.<id>.<name>`
  (`libs/pkgstore/src/rules.rs`): no app serves another app today.
* No cross-process user-gesture token exists; LazyWeb's "only a user
  gesture" rule is checked inside its own process.
* Resident apps (`[entry] resident = true`) run once per session with a tray
  item and `init`'s Reopen/Quit lifecycle; they start at login only with
  `autostart` (docs/tray-plan.md).
* The CA bundle is in every image (`build.rs`); LazySky's switch needs only
  the desktop and the network stack.
* Toasts (`os.lazy.notify.v1`) are available to apps; an action must never
  be the only way to do something.

## 13. Open questions (C0)

1. **Google sign-in in LazyWeb.** Does `accounts.google.com` accept
   WebKit-on-LazyOS's user agent, and does the failed redirect leave the
   `code` readable in the address bar for the paste? Is copying it and
   pasting into LazyCal's dialog bearable (clipboard across the two apps,
   the address bar's error page), or does C4 need the LazyWeb hand-off?
2. **External organisers.** When an attendee's RSVP is patched through the
   API on an event whose organiser is not on Google, does Google email the
   organiser? Until this is known, LazyCal sends an iMIP reply too.
3. **`Authentication-Results`**: do Gmail, Outlook.com and Fastmail add it
   on IMAP-fetched messages, and does esMail's fetch keep it?
4. **tz data**: `chrono-tz` linked in vs TZif in the image (size, update
   path, whether `timed` should move to it in the same change).
5. **iCalendar parser**: in-house (small, fuzzed, exactly our caps) vs a
   crate; the deciding test is the fuzz run and the real-file corpus.
6. **RRULE**: in-house vs the `rrule` crate (which pulls `chrono-tz`).
7. **One Google sign-in for Mail and Calendar**: worth a combined consent
   (mail + calendar scopes) once Mail offers OAuth on LazyOS, or keep them
   separate? v1 keeps them separate.
