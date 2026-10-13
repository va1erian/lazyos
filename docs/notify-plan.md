# Desktop notifications and toasts (`os.lazy.notify`)

Status: **implemented** (issue #747): the interface, LazyShell's presenter,
the motion helper, the app-stopped notice as a toast. No notification centre,
no sound, no keyboard reachability (see "Decisions").

Related: [`tray-plan.md`](tray-plan.md) (identity rules, `Attention`, the
tray the toast lands in), [`shell-plan.md`](shell-plan.md) (S5.3, the
shell-owned path), [`messenger.md`](messenger.md) section 11 (the original
sketch, illustrative only), [`hidpi-plan.md`](hidpi-plan.md) (design pixels),
[`rhai/msg.md`](rhai/msg.md) (`sys::notify::*`), the generated reference
[`idl/os.lazy.notify.v1.md`](idl/os.lazy.notify.v1.md).

## 1. What it is

A service or app tells the user that something happened. LazyShell shows it
as a small card (a *toast*) in a stack above the taskbar's right corner and
animates it. A sender says *what*; it never says *how*, so a hung app cannot
stall an animation, cover the screen or imitate the shell.

```
 app / script                LazyShell                         xuid
 ────────────                ─────────                         ────
 Notify(level,title,...) ──▶ identify (init.Services, kernel)
                             policy: rate limit, mute, DND
                             Center: place / wait / coalesce
 ◀── id                      measure text, lay the stack out
                             one panel per toast ────────────▶ CreateSurface(Panel)
                             tween the position each beat ───▶ PlaceSurface
 (Listen channel)  ◀──────── Activated / ActionInvoked / Closed
```

## 2. The interface (`idl/notify.midl`, `os.lazy.notify.v1`)

| Method | Meaning |
|---|---|
| `Notify(level, title, body, icon?, actions[], progress?, replaces_id?, timeout?) -> (id)` | Show a toast. `replaces_id` naming the sender's own live toast updates it in place and returns the same id. |
| `Close(id)` | Close the sender's toast (`Closed` reason `Sender`). Never an error. |
| `Listen(events: Channel)` | Ask for `Activated`, `ActionInvoked` and `Closed`. Optional: a sender that does not care never calls it. |

`level` is `Info`/`Success`/`Warn`/`Error`. Events come back on
`os.lazy.notify.events.v1`: `Activated(id)`, `ActionInvoked(id, action)`,
`Closed(id, reason)` with reason `User`/`Timeout`/`Replaced`/`Sender`/
`Restart`/`Muted`, and a `Ping` the shell uses for liveness.

Limits (validated; anything else is `EINVAL`): title 1-64 characters, body
256, three actions of at most 24 characters with unique non-zero ids,
`progress` 0-100, a Lucide name or an image of at most 64 x 64. A bad icon is
not an error: the toast shows its level's icon. Text is plain; control
characters become spaces; there is no markup.

Lifetime: the level's default is 5 s for Info and Success; `timeout` 0 means
until dismissed, otherwise 1-60 s. **Warn and Error never time out**, and
neither does a toast whose progress is below 100 (a finished one then times
out normally). Nothing is kept: a toast that is missed is gone, which is why
the serious ones wait for the user, and why an app with a tray item sets its
`Attention` state alongside (that is the durable signal).

## 3. Identity and policy

* Callers must be in the shell's login session and be uid 0 or the shell's
  uid (kernel-stamped), exactly as for the tray.
* The sender is the app `init` launched as the sending task. The header of a
  toast shows the registry name for that app (`init.ListApps`), never text
  from the call, so an app cannot post as another app or as the system.
* An unlabelled program in the login session that is no launched app (a
  script run from the Terminal) is shown as **Terminal**; the shell chooses
  that name. It is refused in session 0, where the services run, so a
  service cannot post as the Terminal.
* **Rate limit** per sender: a burst of 6, then one every half second
  (`EBUSY`, nothing is shown). At most 16 notifications of one sender are
  alive (`ENOSPC`). In-place updates are not rate limited.
* **Stack**: at most 4 toasts show, at most 3 from one sender, so a flood
  leaves room for others. The rest wait (at most 32; plain ones are evicted
  first). A burst of three or more plain notices (no actions, no progress,
  below Warn) from one sender waiting for a place becomes one summary,
  "N notifications", instead of stacking windows.
* **Do-not-disturb and mute** are the user's, honoured by the presenter: in
  `sys/ui/notify/dnd` only Error toasts show; `sys/ui/notify/mute` lists app
  ids (or `<terminal>`) that never show. A suppressed toast is accepted (the
  sender cannot tell) and closes `Muted`. The shell's own notice is never
  suppressed. Per-user shadows `user/<uid>/ui/notify/*` win over the machine
  keys.

## 4. The presenter

Shell-owned: LazyShell serves the interface and draws the toasts, like the
tray. A separate `notifyd` is not built; the interface is the same either
way, so it can move without touching senders. Services stay headless and
session-less: one that needs to notify goes through a session applet.

* The model is `lazyshell::notify` (`xui-app/crates/shell/src/notify/`): the
  validated note (`note.rs`), identity and the rate limit (`policy.rs`), the
  queue and its tweens (`center.rs`), a toast's face and hit test
  (`card.rs`), what is drawn at an instant and the icon flourishes
  (`frame.rs`), the settings (`settings.rs`). It has no clock, no screen and
  no service, and is tested on the host.
* The presenter is `xui-app/src/shell/notify/`: the service (`service.rs`),
  the apps' event channels (`listen.rs`), one chromeless panel per toast
  (`toast.rs`), painting (`paint.rs`), the settings feed (`feed.rs`).
* A toast is a `Panel` surface (never focused, no keyboard session) created
  at its place and moved with `PlaceSurface` while its tween runs. A surface
  is redrawn only when its quantised look changes (`frame::Look`); a toast
  at rest costs nothing. The shell's 30 ms heartbeat is the only clock: with
  no toast, `notify::pump` returns after reading the (empty) queue.
* **Panels stay on screen** (`clamp_panel` in `xuid`), so a toast cannot
  slide in from beyond the corner. It rises from the bar instead: it starts
  one bar height lower, over the taskbar, and springs up into its place.
  Leaving is the same motion reversed.

## 5. The cute part

All of it belongs to the presenter and is a pure function of milliseconds
since the toast appeared (`lazymotion`, `frame::flourish`):

* **Spring**: the slide in is a closed-form underdamped spring (about 7%
  overshoot, settled in ~375 ms); a reflow is an eased tween from wherever
  the toast is (no jump when moved again mid-slide); the sink out is eased.
* **Icon flourish** on arrival: a gentle wiggle (Info), a tick that draws
  itself (Success), a nudge up (Warn), a short shake (Error).
* **The tray item pulses once** when a toast of an app that has a tray item
  appears, so the toast visibly lands in its source.
* **Progress** fills smoothly between updates (eased, 250 ms).
* **Hover pauses the timeout** and shows a thin countdown bar along the
  bottom edge; moving off resumes it. A sideways drag of 48 design pixels
  (a flick) dismisses a toast.
* **`sys/ui/motion`**: `full` (default), `reduced` (nothing travels: the
  content fades in and out in place over 120 ms; no flourish) or `none`
  (instant). The layout is the same in all three.
* **Budget**: a toast moves for ~0.4 s (one `PlaceSurface` per 30 ms beat per
  moving toast) and redraws only while its flourish, fill or countdown
  changes; nothing runs when idle.

## 6. Motion helper

The issue asks for a shared easing/spring helper that the toast and later
window open/close can reuse. `xui-core` is a pinned external dependency, so
it lives beside the shell as `xui-app/crates/motion` (`lazymotion`): `Curve`
(`Linear`, `EaseOut`, `EaseInOut`, `Spring`), `Tween` (retargetable,
no clock), `Motion` (the setting). It can move upstream unchanged.

## 7. Consumers

* **App-stopped notice** (#549): `init`'s app-failure events become an Error
  toast from the shell's reserved system owner with *Restart* and *Close*,
  sticky until dismissed, retiring the bespoke window. This is the one
  notice with no tray item to fall back on, so it exercises the no-history
  rule. Markers are unchanged: `SHELL:NOTICE:OPEN|RESTART|CLOSE`.
* **Scripts**: `sys::notify::notify("Info", "Title", "Body", (), [], (), (), ())`
  from `rhai` or a LazyRAD form (Rhai first for small apps). `Listen` carries
  a channel and is not callable from a script.
* **LazyWeb+ web notifications** (issue #839, [`lazywebplus.md`](lazywebplus.md)): a page the person allowed
  (`Notification.requestPermission()`, a bar in the browser, kept per origin) shows a toast through
  `os.lazy.notify`. The sender is the browser: the title is the site's host (never the page's text), the page's
  `title` and `body` go in the body as two lines (a body may hold a line feed), the level is Info and the
  icon the app's own, so no page can look like a download or a system warning. Per-site rule: at most 3 live
  toasts and a burst of 3 plus one a second, 10 in all, so one site never uses the app's allowance of 16 live
  and a burst of 6. The click comes back as `Activated` and, from the same label and session, the shell raises the
  app's windows (`ActivateApp` of `idl/display.midl`); the browser's toast calls are made from its first thread
  (a thread's id is not the app's).
* **Next**: the removable-media applet of
  [`usb-hotplug-plan.md`](usb-hotplug-plan.md) ("stick mounted", "removed
  without ejecting").

## 8. Verification

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazymotion -p lazyshell
FUZZ_CASES=20000 cargo test --manifest-path xui-app/Cargo.toml -p lazyshell --release notify::fuzz
cargo test -p messenger-generated --test notify
python tools/midlc/midlc.py --check --out libs/generated/src/lib.rs --schema libs/rhai-lazy/src/msg/idl.rs --rhai-api libs/rhai-lazy/api --manifest idl/manifest.json --docs docs/idl idl/*.midl
# image: LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_UI_PROBE=1 LAZYOS_RESET_OS=1, with rhai
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/notify --script tools/screenshot/examples/notify.json
python tools/screenshot/qemu_session.py --image target/lazyos.img --screen 2560x1440 --out shots/notify_hidpi --script tools/screenshot/examples/notify.json   # image built with LAZYOS_DISPLAY_MODE=2560x1440
```

The session posts each level, an action toast (clicked by name: `{"click_at":
{"window": "toast", "widget": "retry"}}`, or `{"target": "toast:retry"}`; the
probe also names `dismiss` and `body`), a progress toast updated in place and
a burst, with frames captured while a toast is mid-slide. `LAZYOS_LABEL_TRACE=1` runs show no `LABEL:DENY` for the
shell (the interface is served by the shell, which is a built-in); an app
that posts needs `os.lazy.notify.v1` in its manifest permissions
(`interfaces = ["os.lazy.notify.v1", "os.lazy.notify.events.v1"]` when it
listens).

## 9. Decisions (v1) and what is not here

* **Shell-owned**, no `notifyd`. **No notification centre**, no history
  panel, no `Watch`. **No sound** (a presenter change later; senders do not
  change). **No focus**: a toast never takes keyboard focus or key events, so
  its actions and dismiss are mouse-driven. A rule for senders follows: **a
  toast action must never be the only way to do something**; a keyboard-only
  user always needs another path in the app or the tray menu.
* Not in v1: a notification centre and history, sound, keyboard reachability
  and a screen-reader label for toasts, a standalone `notifyd`, per-app
  settings beyond mute and do-not-disturb, a Settings page for the keys
  (they are plain `confd` keys until then), toasts sliding in from beyond the
  corner (needs off-screen panels in `xuid`).
