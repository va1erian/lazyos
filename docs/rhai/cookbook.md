# Rhai cookbook

Worked recipes for scripts that talk to LazyOS: read and watch settings, show
notifications, open files and apps, inspect the system and the network, listen
to topics, serve your own interface and write small command-line tools. Each
recipe says what it does, gives a script you can save and run, shows what to
expect, and says what it needs. Everything here uses the `rhai` command
([command](command.md)) and the `sys::*` modules ([API reference](api/index.md));
how values, errors and timeouts work is in [msg](msg.md), and the language is in
[language](language.md).

## Before you start

**Running a recipe.** Save it as `name.rhai` and run `rhai name.rhai`, or paste a
short one into `rhai -e '...'`. A script file does not print its final value;
use `print`. The scripts below assume the desktop Terminal.

**Permissions.** A script has the authority of the process that runs it and no
more (see [command](command.md), "Running scripts from the Terminal"):

| Where it runs | What it may call |
|---|---|
| the Terminal (label `tool:os.lazy.terminal`) | every service you could use yourself; the services still check your user id, so `sys/**` keys are read-only and a restart needs `elevd` |
| an installed LazyRAD app (label `app:<system_name>`) | only the interfaces and topics its manifest lists; they are found from the literal `sys::<alias>::...`, `msg::connect`, `msg::on`, `msg::subscribe` and `msg::publish` arguments in the scripts |

Each recipe has a **Needs** line saying which services it calls. In a
LazyRAD form there is no `msg::run()`: `on_*` handlers and `msg::on` run on the
form's window by themselves ([msg](msg.md), "In LazyRAD form scripts"), so a form
shows results in its controls rather than with `print`.

**Reading examples.** The "Example output" blocks show the shape of the result;
the numbers and names on your machine will differ. A line `Needs: ...` names the
interface.

**Two helpers** are used in several recipes; paste them at the top of a script
that needs them. LazyOS services return an address as four bytes and an error as
a string or a map:

```rhai
// "10.0.2.15" from a four-byte blob
fn ip(b) { `${b[0]}.${b[1]}.${b[2]}.${b[3]}` }

// the text of any caught error: a string from a service, a map from the language
fn describe(e) {
    if type_of(e) == "map" && "message" in e { e.message } else { e.to_string() }
}
```

## Settings (`confd`)

`confd` is the hierarchical settings store. `sys/**` keys are the machine's:
anyone reads, only an administrator through `elevd` writes. `user/<uid>/**` keys
are yours alone. A value is a map `#{ kind: N, ... }` where `kind` says which
field is set: `0` bool (`bool_value`), `1` i64 (`i64_value`), `2` u64
(`u64_value`), `3` string (`str_value`), `4` bytes (`bytes_value`). A path has
lowercase segments of `a-z 0-9 _ . -` separated by `/`, up to 256 bytes, and a
value is at most 4 KiB.

### 1. Read a setting

Print one key, or say it is not set.

```rhai
let v = sys::confd::get("sys/ui/theme");
if v == () {
    print("sys/ui/theme is not set");
} else {
    print(`kind ${v.kind}: ${v.str_value}`);
}
```

An absent key is `()`, not an error. Read the field that matches `kind`.

Needs: `os.lazy.confd.v1`. Any session may read `sys/**`.

### 2. Read a typed value with a default

Turn the value map into the plain value, and fall back to a default.

```rhai
fn conf_value(path, fallback) {
    let v = sys::confd::get(path);
    if v == () { return fallback; }
    switch v.kind {
        0 => v.bool_value,
        1 => v.i64_value,
        2 => v.u64_value,
        3 => v.str_value,
        _ => v.bytes_value
    }
}

print(conf_value("sys/ui/motion", "full"));
print(conf_value("sys/ui/notify/dnd", false));
```

Needs: `os.lazy.confd.v1`.

### 3. List the keys below a prefix

See what is stored under a part of the tree, with the values.

```rhai
let prefix = if os::args().len() > 0 { os::args()[0] } else { "sys/ui" };
for path in sys::confd::list(prefix) {
    let v = sys::confd::get(path);
    let shown = if v == () { "(gone)" } else {
        switch v.kind { 0 => v.bool_value, 1 => v.i64_value, 2 => v.u64_value,
                        3 => v.str_value, _ => "(bytes)" }
    };
    print(`${path} = ${shown}`);
}
```

`rhai list.rhai user/1000/term` lists a user's Terminal settings. `list` returns
only paths you may read, sorted.

Example output:

```text
sys/ui/motion = full
sys/ui/scale = auto
```

Needs: `os.lazy.confd.v1`.

### 4. Find your own user id

Per-user keys are named by user id. Ask the account service who you are.

```rhai
let name = os::env("USER");
let found = sys::accounts::lookup(name, ());
if !found.found {
    print(`no account called ${name}`);
    exit(1);
}
print(`${found.user.name} is uid ${found.user.uid}, home ${found.user.home}`);
```

`lookup(name, uid)` takes one of them and `()` for the other. The session sets
`USER` and `HOME` in the environment of its programs.

Example output: `user is uid 1000, home /home/user`

Needs: `os.lazy.accounts.v1`. A Terminal may call it; an installed app must list
it and may not be able to ask.

### 5. Change a Terminal setting live

Set the Terminal font size; open Terminal windows follow at once.

```rhai
let uid = sys::accounts::lookup(os::env("USER"), ()).user.uid;
let size = if os::args().len() > 0 { parse_int(os::args()[0]) } else { 16 };
sys::confd::set(`user/${uid}/term/font/size`, #{ kind: 1, i64_value: size });
print(`font size set to ${size}`);
```

Run `rhai fontsize.rhai 20`. Other Terminal keys (`font/family`, `cursor/shape`,
`colors/scheme`, ...) are in the [Terminal plan](../terminal-plan.md); the
type of each key matters (`font/size` is an i64, `font/family` a string, so use
`kind: 3, str_value: "..."` for it).

Needs: `os.lazy.confd.v1`, `os.lazy.accounts.v1`. You can write your own
`user/<uid>/**` keys without any prompt.

### 6. Toggle a boolean setting

Read, flip and write back a bool key.

```rhai
let uid = sys::accounts::lookup(os::env("USER"), ()).user.uid;
let path = `user/${uid}/term/cursor/blink`;
let v = sys::confd::get(path);
let now = if v == () { true } else { v.bool_value };
sys::confd::set(path, #{ kind: 0, bool_value: !now });
print(`${path}: ${now} -> ${!now}`);
```

Writes are last-writer-wins, so a script that must not lose a concurrent change
should keep the read and the write close together, as here.

Needs: `os.lazy.confd.v1`, `os.lazy.accounts.v1`.

### 7. Delete a setting

Remove a key so the default applies again.

```rhai
let uid = sys::accounts::lookup(os::env("USER"), ()).user.uid;
sys::confd::delete(`user/${uid}/term/font/size`);
print("font size reset");
```

Deleting a key that is not there succeeds.

Needs: `os.lazy.confd.v1`.

### 8. Watch settings change

Print every change under a subtree as it happens.

```rhai
let uid = sys::accounts::lookup(os::env("USER"), ()).user.uid;
sys::confd::on_user_changed(uid, "term/#", |e| {
    let what = if e.payload.deleted { "deleted" } else { "changed" };
    print(`${what}: ${e.payload.path}`);
});
print("watching, Ctrl-C to stop");
msg::run();
```

A change event carries the path and whether it was a deletion, never the value;
read the key again to see the new value. `sys::confd::on_changed("sys/ui/#", ...)`
watches the machine keys the same way. `msg::run()` returns when `msg::stop()`
is called from a handler, or you can give it a time: `msg::run(60000)`.

Example output after changing the font in Preferences:

```text
changed: user/1000/term/font/size
```

Needs: `os.lazy.confd.v1`, and `subscribe` on `user/<your uid>/confd/changed/term/#`.

### 9. Back up your settings to a file

Dump a subtree to a JSON file, and put it back later.

```rhai
// backup.rhai: rhai backup.rhai user/1000/term /tmp/term.json
let prefix = os::args()[0];
let file = os::args()[1];
let saved = #{};
for path in sys::confd::list(prefix) {
    let v = sys::confd::get(path);
    if v != () && v.kind != 4 {
        saved[path] = v;
    }
}
write(file, saved.to_json());
print(`${saved.len()} keys written to ${file}`);
```

```rhai
// restore.rhai: rhai restore.rhai /tmp/term.json
let saved = parse_json(read(os::args()[0]));
for path in saved.keys() {
    let v = saved[path];
    sys::confd::set(path, #{
        kind: v.kind,
        bool_value: v.bool_value,
        i64_value: v.i64_value,
        u64_value: v.u64_value,
        str_value: v.str_value
    });
}
print(`${saved.len()} keys restored`);
```

Byte values (`kind` 4) are skipped here. Restoring refuses keys you may not
write (`sys/**`), which stops the script with an `EACCES` error.

Needs: `os.lazy.confd.v1`.

## Notifications

`sys::notify::notify(level, title, body, icon, actions, progress, replaces_id,
timeout)` shows a toast and returns its id. `level` is `"Info"`, `"Success"`,
`"Warn"` or `"Error"`. The title is 1 to 64 characters and the body at most 256.
Pass `()` for the options you do not use. A toast from a script is shown with
the name "Terminal", not as the system. Warn and Error toasts stay until
dismissed; the others close after five seconds.

### 10. Show a toast

Tell the user something from a script.

```rhai
let id = sys::notify::notify("Info", "Backup finished", "12 files copied", (), [], (), (), ());
print(`toast ${id}`);
```

Needs: `os.lazy.notify.v1`; the caller must be in the shell's login session.
Nothing is kept, so a toast that is missed is gone. Too many too fast fail with
`EBUSY`, and more than 16 alive at once with `ENOSPC`.

### 11. A toast tool for the shell

Make a command that other scripts and `sh` can call: `notify Level Title Body`.

```rhai
// notify.rhai
let a = os::args();
if a.len() < 2 {
    debug("usage: notify.rhai LEVEL TITLE [BODY]");
    exit(2);
}
let body = if a.len() > 2 { a[2] } else { "" };
try {
    sys::notify::notify(a[0], a[1], body, (), [], (), (), ());
} catch (e) {
    debug(`not shown: ${e}`);
    exit(1);
}
```

Make it a command by putting `#!/bin/rhai` on its first line (Rhai ignores it):

```bash
chmod +x notify.rhai
make && ./notify.rhai Success Build "All done" || ./notify.rhai Error Build "It failed"
```

A level that is not one of the four is an `EINVAL` error, which the script
reports on standard error and turns into exit status 1.

Needs: `os.lazy.notify.v1`.

### 12. A progress toast that updates in place

Show "Copying" climbing to 100 as one toast.

```rhai
let id = ();
for pct in [0, 25, 50, 75, 100] {
    id = sys::notify::notify(
        "Info", "Copying photos", `${pct}%`,
        (), [], pct, id, ()
    );
    sleep(500);
}
```

Passing the previous `id` as `replaces_id` updates that toast instead of making a
new one. A toast with `progress` below 100 does not time out; at 100 it closes
after the usual few seconds. Close one early with `sys::notify::close(id)`.

Needs: `os.lazy.notify.v1`.

### 13. A toast with buttons

Offer up to three actions, each with a unique non-zero id and a label of at most
24 characters.

```rhai
sys::notify::notify(
    "Error", "Backup failed", "The disk was removed before the copy finished",
    #{ lucide: "usb" },
    [#{ id: 1, label: "Retry" }, #{ id: 2, label: "Details" }],
    (), (), ()
);
```

The `icon` is a Lucide icon name (`#{ lucide: "usb" }`) or an image; an unknown
name falls back to the level's own icon. A script can post the buttons, but
cannot hear which one was pressed: the click reaches an app through a channel a
script cannot open. Treat actions as a convenience, never the only way to do
something (a toast never takes keyboard focus).

Needs: `os.lazy.notify.v1`.

### 14. Warn when a service is down

Combine the supervisor and the notifier: tell me if any service has failed or keeps restarting.

```rhai
let down = [];
for s in sys::init::services() {
    if s.state == "failed" || s.state == "restarting" {
        down.push(s.name);
    }
}
if down.len() > 0 {
    let names = "";
    for n in down { names += n + " "; }
    sys::notify::notify("Warn", "Services in trouble", names, (), [], (), (), ());
}
print(`${down.len()} service(s) in trouble`);
```

`state` is one of `pending`, `running`, `restarting`, `stopped` or `failed`;
`health` is the last health word for the row.

Needs: `os.lazy.init.v1`, `os.lazy.notify.v1`.

## Apps and files

### 15. Open a file with its default app

Do what a double-click in Files does.

```rhai
let path = os::args()[0];
let r = sys::mimed::open(path, "open");
print(`${path}: ${r.mime} -> ${r.app} (launched: ${r.launched})`);
```

`mimed` guesses the type from the name, picks the app registered for the verb
(`open`, `edit`, ...), asks `init` to launch it and tells it which file to load.
Example: `rhai open.rhai /home/user/Aurora.jpg` prints
`/home/user/Aurora.jpg: image/jpeg -> os.lazy.pictures (launched: true)` on an
image that has the Picture Viewer.

Needs: `os.lazy.mimed.v1`. The app must be installed for that type.

### 16. Which app handles a type?

Look at the registry without launching anything.

```rhai
let path = os::args()[0];
let mime = sys::mimed::guess(path);
print(`type: ${mime}`);
for verb in sys::mimed::verbs(mime) {
    let app = sys::mimed::lookup(mime, verb);
    print(`  ${verb}: ${if app == () { "(none)" } else { app }}`);
}
```

An unknown extension reports `application/octet-stream`. Your own preference for
a type is a setting (`user/<uid>/mimed/default/<type>`), which `lookup` does not
show.

Needs: `os.lazy.mimed.v1`.

### 17. Open a web address

Hand a URL to the browser the same way.

```rhai
sys::mimed::open("http://example.com/", "open");
```

`mimed` maps a URL to the app registered for its scheme (`http`, `https`).
This works when a browser is installed (LazyWeb or LazyWeb+).

Needs: `os.lazy.mimed.v1`.

### 18. List the apps you can launch

Print the app registry: built-in apps first, then installed packages.

```rhai
for a in sys::init::list_apps() {
    if a.hidden { continue; }
    let where_ = if a.installed { "package" } else { "built in" };
    print(`${a.id}  ${a.name} (${where_}, ${a.category})`);
}
```

Example output:

```text
os.lazy.terminal  Terminal (package, system)
...
```

Needs: `os.lazy.init.v1`.

### 19. Launch an app

Start an app by its id, optionally with a file.

```rhai
let app = os::args()[0];
let file = if os::args().len() > 1 { os::args()[1] } else { "" };
let r = sys::init::launch(app, file, 0);
print(`${r.app} is pid ${r.pid}${if r.existing { " (already running)" } else { "" }}`);
```

`rhai launch.rhai os.lazy.files` opens Files, and `rhai launch.rhai os.lazy.pdf
/home/user/doc.pdf` opens a document in the PDF viewer. The second argument is
empty or one absolute path; the third is the session, and `0` means yours. A resident app that is already running is not started again: it is told to
reopen, and `existing` is true. `sys::shell::launch("os.lazy.files")` does the
same through the desktop shell, as the Start menu would.

Needs: `os.lazy.init.v1`.

### 20. Stop an app

End every instance of an app.

```rhai
let n = sys::init::stop(os::args()[0]);
print(`${n} instance(s) stopped`);
```

An app that watches its lifecycle is asked to quit and is killed if it still runs
3 seconds later; any other is killed at once. The supervisor does not restart it,
and the call returns once every instance has exited. Only you (the session's
owner) or an administrative service may stop your apps.

Needs: `os.lazy.init.v1`.

### 21. List the windows and raise one

Read the taskbar and bring a window to the front by its title.

```rhai
let st = sys::shell::status();
let want = os::args()[0];
for w in st.windows {
    print(`${w.surface}  ${w.title}${if w.minimized { " (minimized)" } else { "" }}`);
    if w.title.contains(want) {
        sys::shell::activate(w.surface);
        print("-> raised");
    }
}
```

`status()` also tells you the Start menu rows (`menu`), the desktop icons
(`desktop`) and the tray items (`tray`).

Needs: `os.lazy.shell.v1`.

### 22. See what is installed

List the packages, and look inside an `.lzp` before installing it.

```rhai
for app in sys::pkgd::list() {
    print(`${app.system_name}  ${app.version}  ${app.name}`);
}
```

```rhai
// inspect.rhai: rhai inspect.rhai /home/user/thing.lzp
let info = sys::pkgd::inspect(os::args()[0]);
print(`${info.name} ${info.version} by ${info.author} (${info.system_name})`);
for p in info.permissions {
    print(`  ${p.risk}  ${p.kind} ${p.value}  ${p.explanation}`);
}
for problem in info.problems {
    print(`  PROBLEM: ${problem}`);
}
```

`inspect` changes nothing. It takes a file under your home folder or `/transient`.
Installing is an administrator action ([recipe 38](cookbook.md)).

Needs: `os.lazy.pkgd.v1`.

### 23. React when a package is installed or removed

Follow the package events.

```rhai
sys::pkgd::on_pkg(|e| {
    let p = e.payload;
    print(`${p.op} ${p.system_name} ${p.version} ok=${p.ok}`);
});
msg::run();
```

The last segment of the topic (`system/events/pkg/<op>`) is the operation, so
`sys::pkgd::on_pkg(op, handler)` watches one kind of event; the payload's `op`
field says the same.

Needs: `os.lazy.pkgd.v1` topics `system/events/pkg/+`.

## Clipboard

### 24. Is something on the clipboard?

Ask what the session's newest clipboard offer holds, without reading it.

```rhai
let c = sys::clipboard::current();
if c == () {
    print("the clipboard is empty");
} else {
    let types = "";
    for m in c.mimes { types += m + " "; }
    print(`offered by ${c.owner}: ${types}${if c.lazy { "(lazy)" } else { "" }}`);
}
```

`current()` returns the metadata of the offer (owner, MIME types), never its
content. A script cannot copy or paste: `offer` and `request` are sent on separate
permission scopes (`os.lazy.clipboard.write.v1` and `.read.v1`), which the generic
`msg` call does not use, so `current()` and the offer-changed topic
(`sys::clipboard::on_session_changed(session, handler)`) are what a script can
reach.

Needs: `os.lazy.clipboard.v1`. The offer belongs to your login session.

## Logs

### 25. Show the latest log records

Read the in-memory event log, which the services write to.

```rhai
let n = if os::args().len() > 0 { parse_int(os::args()[0]) } else { 20 };
let total = sys::logd::count();
for r in sys::logd::tail(n) {
    print(`#${r.seq} t${r.tick} ${r.topic}: ${r.detail}`);
}
print(`(${total} records since boot)`);
```

Example output:

```text
#118 t4012 <topic>: <detail>
#119 t4020 <topic>: <detail>
(119 records since boot)
```

Needs: `os.lazy.logd.v1`. The per-source journals (`sys::logd::sources()` and
`tail_file`) are for uid 0 only and fail with `EACCES` for a session.

### 26. Follow the log

Print new records as they arrive.

```rhai
let seen = sys::logd::count();
print("following the log, Ctrl-C to stop");
loop {
    let now = sys::logd::count();
    if now > seen {
        for r in sys::logd::tail(now - seen) {
            print(`#${r.seq} ${r.topic}: ${r.detail}`);
        }
        seen = now;
    }
    sleep(500);
}
```

`logd` offers no push, so this polls twice a second. `tail(n)` returns the newest
`n` records oldest first.

Needs: `os.lazy.logd.v1`.

### 27. Check the log has not been tampered with

Every record is chained to the one before it.

```rhai
let v = sys::logd::verify();
if v.ok {
    print(`chain intact (${v.index} records)`);
} else {
    print(`chain broken at record ${v.index}`);
    exit(1);
}
```

Needs: `os.lazy.logd.v1`.

## System information

### 28. Memory use

Read the retained memory snapshot that `sysmond` publishes.

```rhai
let sub = sys::sysmond::subscribe_stats_memory();
let e = sub.next(3000);
sub.close();
if e == () {
    print("no snapshot yet");
    exit(1);
}
let m = e.payload;
print(`frames: ${m.frames_live} used, ${m.frames_free} free, ${m.frames_total} total`);
print(`kernel heap: ${m.heap_used} of ${m.heap_total}`);
```

The topic is retained, so a late subscriber gets the latest value straight away.
`sys::sysmond::snapshot()` returns the raw kernel block as a blob instead.

Needs: `os.lazy.sysmond.v1` topic `system/stats/memory`.

### 29. The busiest tasks

Sort the task table by CPU.

```rhai
let sub = sys::sysmond::subscribe_stats_tasks();
let e = sub.next(3000);
sub.close();
let tasks = e.payload.tasks;
tasks.sort(|a, b| if a.cpu > b.cpu { -1 } else if a.cpu < b.cpu { 1 } else { 0 });
let shown = 0;
for t in tasks {
    print(`${t.pid}  ${t.cpu}  ${t.state}  ${t.name}`);
    shown += 1;
    if shown == 10 { break; }
}
print(`${e.payload.live} live tasks`);
```

Needs: `os.lazy.sysmond.v1` topic `system/stats/tasks`.

### 30. Service health

Print the aggregate and every service's row.

```rhai
let h = sys::healthd::status();
print(`overall: ${h.summary.status} ${h.summary.detail}`);
for r in h.records {
    print(`  ${r.name}: ${r.status} ${r.detail}`);
}
```

Status is `ok`, `degraded` or `down`. To be told when it changes instead of
asking, subscribe to the retained summary:

```rhai
sys::healthd::on_summary(|e| print(`health is now ${e.payload.status}`));
msg::run();
```

Needs: `os.lazy.healthd.v1`.

### 31. The date and time

Show the wall clock in the machine's time zone, and the network time state.

```rhai
fn two(n) { if n < 10 { "0" + n } else { "" + n } }

// days since 1970-01-01 to [year, month, day]
fn civil(days) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400;
    [if m <= 2 { y + 1 } else { y }, m, d]
}

fn format_time(unix_secs) {
    let c = civil(unix_secs / 86400);
    let s = unix_secs % 86400;
    `${c[0]}-${two(c[1])}-${two(c[2])} ${two(s / 3600)}:${two(s % 3600 / 60)}:${two(s % 60)}`
}

let t = sys::timed::now();
let local = t.unix_ms / 1000 + t.tz_offset_s;
print(`${format_time(local)} ${t.tz_name}${if t.dst { " (summer time)" } else { "" }}`);

let n = sys::timed::ntp_status();
print(`network time: ${sys::timed::NTP_STATE[n.state]}, server ${n.server}, offset ${n.offset_ms} ms`);
```

Example output:

```text
2026-10-21 14:05:09 Europe/Paris (summer time)
network time: Synchronized, server 10.0.2.2, offset 3 ms
```

`format_time(1700000000)` gives `2023-11-14 22:13:20`. For an immediate network
sync use `sys::timed::sync_now()` (refused with `EAGAIN` if one just ran).
Setting the clock or the zone is a machine change ([recipe 40](cookbook.md)).

Needs: `os.lazy.timed.v1`.

### 32. Devices and their drivers

List the hardware the kernel found and who drives it.

```rhai
for d in sys::devd::devices() {
    let ids = d.vendor.to_hex() + ":" + d.device.to_hex();
    print(`${d.id}  ${ids}  ${d.class}  ${d.state}  ${d.driver}  ${d.model}`);
}
```

Filter by class to answer "is there a sound card?":

```rhai
let cards = sys::devd::devices().filter(|d| d.class == "audio");
print(if cards.len() > 0 { "sound card found" } else { "no sound card" });
```

Needs: `os.lazy.devd.v1`.

### 33. Who is logged in

List the graphical sessions and the accounts.

```rhai
let s = sys::logind::sessions();
print(`active session: ${s.active}`);
for x in s.sessions {
    print(`  ${x.id}: ${x.user} (uid ${x.uid}) ${x.state}`);
}
let u = sys::accounts::list_users();
for a in u.users {
    print(`  account ${a.name} uid ${a.uid}${if a.admin { " admin" } else { "" }}`);
}
```

Needs: `os.lazy.logind.v1`, `os.lazy.accounts.v1`.

## Network

### 34. Addresses, routes and counters

What the network stack is doing.

```rhai
fn ip(b) { `${b[0]}.${b[1]}.${b[2]}.${b[3]}` }

for i in sys::net_stack::interfaces() {
    let kind = if i.primary { " (default route)" } else { "" };
    print(`${i.name}: link ${if i.link { "up" } else { "down" }}, mtu ${i.mtu}, metric ${i.metric}${kind}`);
}
for a in sys::net_stack::addresses() {
    print(`  ${a.interface}: ${ip(a.addr)}/${a.prefix_len} (${sys::net_stack::ADDR_SOURCE[a.source]})`);
}
for r in sys::net_stack::routes() {
    print(`  route ${ip(r.dest)}/${r.prefix_len} via ${ip(r.gateway)} on ${r.interface}`);
}
let st = sys::net_stack::stats();
print(`  rx ${st.rx_frames} frames, tx ${st.tx_frames} frames, ${st.leases} lease(s)`);
```

Example output:

```text
eth0: link up, mtu 1500, metric 100 (default route)
  eth0: 10.0.2.15/24 (Dhcp)
  route 10.0.2.0/24 via 0.0.0.0 on eth0
  route 0.0.0.0/0 via 10.0.2.2 on eth0
  rx 412 frames, tx 307 frames, 1 lease(s)
```

Needs: `os.lazy.net.stack.v1`. A tool needs the network declared; the Terminal has
it. On an image without networking there is no such service and `connect` fails.

### 35. Ping and look up a name

An ICMP echo and a DNS lookup from a script.

```rhai
fn ip(b) { `${b[0]}.${b[1]}.${b[2]}.${b[3]}` }

let host = os::args()[0];
let addrs = sys::net_stack::resolve(host, 3000);
if addrs.len() == 0 {
    print(`${host}: no address`);
    exit(1);
}
let target = addrs[0];
print(`${host} is ${ip(target)}`);

let r = sys::net_stack::ping(target, 32, 2000);
print(`reply from ${ip(r.source)}: ${r.bytes} bytes, ${r.rtt_ms} ms`);
```

`resolve` takes a name and a timeout in milliseconds (10 to 60000), and returns
an array of four-byte blobs; a dotted address is answered without a query.
`ping(dst, payload_len, timeout_ms)` sends one echo and fails with `ETIMEDOUT`
when nothing comes back, or `ENETUNREACH` when the machine has no address yet.
Both are limited to a few in flight per caller (`EAGAIN`).

Needs: `os.lazy.net.stack.v1` (needs the network declaration for a tool).

### 36. Wi-Fi status and networks

Show the radio's state, scan, and list what was heard.

```rhai
let names = sys::net_wifi::interfaces();
if names.len() == 0 {
    print("no wireless interface");
    exit(1);
}
let ifname = names[0];

let st = sys::net_wifi::status(ifname);
print(`${ifname}: ${sys::net_wifi::CONN_STATE[st.state]} ${st.ssid}`);

let done = sys::net_wifi::subscribe_scan(ifname);
sys::net_wifi::scan(ifname, []);
if done.next(10000) == () {
    print("the scan did not finish");
}
done.close();

for n in sys::net_wifi::networks(ifname) {
    let sec = sys::net_wifi::SECURITY[n.security];
    print(`${if n.connected { "*" } else { " " }} ${n.ssid}  ${n.rssi_dbm} dBm  ${sec}`);
}
```

Example output:

```text
wlan0: Connected HomeNet
* HomeNet  -48 dBm  Wpa2Psk
  Cafe  -71 dBm  Open
```

`scan` returns at once and the results arrive on a topic, so the script waits for
the scan event first. A scan that is already running fails with `EBUSY`.

Needs: `os.lazy.net.wifi.v1`, topic `system/net/<ifname>/wifi/scan`; only on a
machine with a supported Wi-Fi radio.

### 37. Wait for the network

Block until the stack has an address, then carry on.

```rhai
fn ip(b) { `${b[0]}.${b[1]}.${b[2]}.${b[3]}` }

let sub = sys::net_stack::subscribe_network_up();
let have = false;
for a in sys::net_stack::addresses() {
    if a.addr[0] != 0 { have = true; }
}
if !have {
    print("waiting for the network...");
    let e = sub.next(30000);
    if e == () {
        print("no network after 30 s");
        exit(1);
    }
    print(`up: ${e.payload.interface} ${ip(e.payload.addr)}`);
}
sub.close();
print("online");
```

Subscribing before checking the addresses avoids missing the event that arrives
in between.

Needs: `os.lazy.net.stack.v1`, topic `system/events/network/up`.

## Accounts and elevated actions

A change that needs an administrator is requested from `elevd` with a fixed
operation name and string arguments. `xuid` shows a trusted prompt and an
administrator types their name and password; the script waits for the answer.
Set `msg::set_timeout(0)` first, because a person takes longer than the default
5 seconds ([command](command.md), "Elevated actions").

### 38. Install or remove a package

Ask for an administrator's approval to install an `.lzp`.

```rhai
msg::set_timeout(0);
let path = os::args()[0];
try {
    let r = sys::elevd::request("pkg.install", [path]);
    print(`installed: ${r.detail}`);
} catch (e) {
    print(`not installed: ${e}`);
    exit(1);
}
```

Put the package under `/transient` or your home first. `pkg.remove` takes the
app's system name instead. The prompt names the package, its version and what it
asks for.

Needs: `os.lazy.elevd.v1`. Do not expect it to work from a session with no
administrator account or from a console image (`ENODEV`: no prompt can be shown).

### 39. Restart a service

Restart one of the services a session may ask to restart.

```rhai
msg::set_timeout(0);
let name = os::args()[0];
try {
    sys::elevd::request("service.restart", [name]);
    print(`${name} restarted`);
} catch (e) {
    let text = e.to_string();
    if text.contains("ECANCELED") {
        print("cancelled at the prompt");
    } else if text.contains("EACCES") {
        print("no administrator approved it");
    } else {
        print(`failed: ${text}`);
    }
    exit(1);
}
```

The allowed names are `inputd`, `audiod`, `sndd`, `netd`, `netdrv`, `usbd`,
`devd`, `mountd`, `printd`, `clipboardd`, `mimed`, `healthd` and `sysmond`; any
other fails before a prompt appears.

Needs: `os.lazy.elevd.v1`.

### 40. Change a machine setting

Write a `sys/**` key, which only an administrator may.

```rhai
msg::set_timeout(0);
let r = sys::elevd::request("conf.set", ["sys/ui/motion", "str", "reduced"]);
print(r.detail);
```

The arguments are the path, the kind (`bool`, `i64`, `u64`, `str` or `bytes` in
hex) and the value as text. `conf.delete` takes a path. `time.set` takes UTC
seconds and `power.policy` a key and a value. Every change asks again, and a
prompt you cancel or leave alone slows later requests down for a while (`EAGAIN`).
The full operation table is in [command](command.md), "Elevated actions".

Needs: `os.lazy.elevd.v1`.

### 41. See who has been asking for elevation

Follow the audit stream of every request.

```rhai
sys::elevd::on_request(|e| {
    let r = e.payload;
    print(`${r.operation}: ${r.outcome} for ${r.user} (approved by ${r.admin})`);
});
msg::run();
```

Needs: `os.lazy.elevd.v1` topic `system/events/elevd/request`. If the
subscription is refused (`EACCES`), your session may not read the audit stream.

## Topics

A topic is a named stream of events. A name is `/`-separated; a filter may use
`+` for one segment and a trailing `#` for the rest. The system's topics have
declared payload types, so `event.payload` is a map. Your own topics carry bytes:
publish a string or a blob and read `payload` as a blob (`payload.as_string()`).

Every event is a map with `topic`, `publisher`, `sequence`, `retained`, `payload`
and `bytes`.

### 42. Publish and receive

One script both sides, to see the pieces.

```rhai
let sub = msg::subscribe("demo/rhai/#");
let reached = msg::publish("demo/rhai/hello", "hi there");
print(`reached ${reached} subscriber(s)`);

let e = sub.next(2000);
if e == () {
    print("nothing arrived");
} else {
    print(`${e.topic}: ${e.payload.as_string()}`);
}
sub.close();
```

Example output:

```text
reached 1 subscriber(s)
demo/rhai/hello: hi there
```

`sub.next(ms)` waits up to `ms` and returns `()` on a timeout; `next(0)` waits
for ever. Use names under your own prefix (`demo/...` here): the system's
namespaces are reserved.

Needs: topics `demo/rhai/#`; any non-reserved name is allowed to a Terminal.

### 43. A publisher and a subscriber in two scripts

Run a listener in the background and send it messages. The listener:

```rhai
// listener.rhai
msg::on("demo/chat/+", |e| {
    print(`[${e.topic}] ${e.payload.as_string()}`);
    if e.payload.as_string() == "bye" { msg::stop(); }
});
print("listening");
msg::run();
print("done");
```

The sender:

```rhai
// say.rhai: rhai say.rhai alice "hello" ; rhai say.rhai alice bye
msg::publish("demo/chat/" + os::args()[0], os::args()[1]);
```

```bash
rhai listener.rhai &
rhai say.rhai alice hello
rhai say.rhai bob bye
```

`msg::on(filter, handler)` subscribes and registers the handler; `msg::run()`
then waits and runs handlers until `msg::stop()`. `publish` returns how many
subscriptions received the event, so a sender can tell when nobody is listening.

Needs: topics `demo/chat/+`.

### 44. Structured messages

Send maps as JSON text and read them back.

```rhai
// publisher
let reading = #{ sensor: "kitchen", celsius: 21.5, ok: true };
msg::publish("demo/sensors/kitchen", reading.to_json());
```

```rhai
// subscriber
msg::on("demo/sensors/+", |e| {
    let r = parse_json(e.payload.as_string());
    print(`${r.sensor}: ${r.celsius} C`);
    msg::stop();
});
msg::run(30000);
```

An undeclared topic carries bytes, so JSON in a string is a simple convention both
sides understand. For a typed topic, declare it in a `.midl` file instead
([MIDL](../midl.md)).

Needs: topics `demo/sensors/+`.

### 45. Retained values: state for latecomers

Publish the latest state so a subscriber that starts later still sees it.

```rhai
msg::publish("demo/state/lamp", "on", true);        // retained
```

```rhai
let sub = msg::subscribe("demo/state/lamp");
let e = sub.next(2000);
if e != () {
    print(`${e.payload.as_string()} (retained: ${e.retained})`);
}
```

The broker keeps the last retained payload of each topic and hands it to every new
subscription. The system uses this for status: `system/health/summary` and
`system/stats/memory` are retained, which is why recipes 28 and 30 get a value
immediately.

Needs: topics `demo/state/lamp`.

### 46. Delivery guarantees (QoS)

Choose what happens when a subscriber is slower than the publisher.

```rhai
// keep the newest event only
let a = msg::subscribe("demo/q/#", #{ qos: "latest" });
// queue up to 32 events, drop the oldest when full
let b = msg::subscribe("demo/q/#", #{ qos: "buffered", depth: 32 });
// one event per publisher, the newest, until you read it
let c = msg::subscribe("demo/q/#", #{ qos: "conflate" });
// never lose an event: it is redelivered until acknowledged
let d = msg::subscribe("demo/q/#", #{ qos: "reliable" });
print([a.filter, b.id, c.id, d.id]);
```

`depth` (1 to 64) only matters for `buffered`. Without options a subscription
takes the declared topic's QoS, or `buffered` for a topic of your own.

Needs: topics `demo/q/#`.

### 47. Reliable work queue

Take jobs one at a time and acknowledge each only when it is done.

```rhai
// worker.rhai
let jobs = msg::subscribe("demo/jobs/#", #{ qos: "reliable" });
print("waiting for jobs");
loop {
    let e = jobs.next(60000);
    if e == () { break; }
    let name = e.payload.as_string();
    print(`working on ${name}`);
    sleep(200);
    jobs.ack(e.sequence);
    print(`finished ${name}`);
}
```

```bash
rhai worker.rhai &
rhai -e 'msg::publish("demo/jobs/1", "resize"); msg::publish("demo/jobs/2", "upload")'
```

A reliable subscription keeps an event until you `ack` its `sequence` (which
retires everything up to it): call `next()` again without acking and the same
event comes back. With `msg::on(filter, #{ qos: "reliable" }, handler)` the
acknowledgement is automatic when the handler returns.

Needs: topics `demo/jobs/#`.

### 48. Run handlers for a while

Collect events for ten seconds, then report.

```rhai
let seen = [];
msg::on("demo/#", |e| seen.push(e.topic));
let handled = msg::run(10000);
print(`${handled} event(s)`);
for t in seen { print(`  ${t}`); }
```

`msg::run(ms)` returns how many events and service calls it handled. Without a
time it runs until a handler calls `msg::stop()`. It fails with "nothing to wait
for" if no `msg::on` or `msg::serve` was registered first. An error thrown by a
handler ends the loop with that error.

Needs: topics `demo/#`.

### 49. Watch services start and stop

React to the supervisor's service events.

```rhai
sys::init::on_service(|e| {
    let s = e.payload;
    print(`${e.topic}: ${s.state} pid ${s.pid} restarts ${s.restarts} ${s.detail}`);
});
sys::init::on_app(|e| {
    print(`app trouble: ${e.payload.name}: ${e.payload.summary} ${e.payload.reason}`);
});
msg::run();
```

The first handler sees every `system/events/service/<name>` event and the second
the "stopped unexpectedly" notices for apps (`system/events/app/<id>`).
Narrow either to one name: `sys::init::on_service("netd", handler)`.

Needs: `os.lazy.init.v1` topics `system/events/service/+` and
`system/events/app/+`.

## Writing a service in Rhai

`msg::serve(name, interface, handlers)` registers `name` and answers calls to any
interface in `msg::interfaces()`. Handlers are keyed by the IDL method name, get
the decoded arguments in order, and return the reply.

### 50. An echo service

Serve `os.lazy.echo.v1` under your own name.

```rhai
// serve.rhai
let served = 0;
msg::serve("demo.rhai.echo", "os.lazy.echo.v1", #{
    Echo: |text, count| {
        served += 1;
        let out = "";
        for i in 0..count { out += text; }
        out
    },
    Ping: || true,
});
print("serving demo.rhai.echo");
msg::run(120000);
print(`served ${served} call(s)`);
```

Run it in the background and call it from a second script:

```bash
rhai serve.rhai &
rhai call.rhai
```

```rhai
// call.rhai
let echo = msg::connect("os.lazy.echo.v1", "demo.rhai.echo");
print(echo.echo("ab", 3));       // ababab
print(echo.ping());              // true
```

The handler of a method with one reply value returns that value; one with
several returns a map. A method without a handler answers `ENOSYS`. Names that
start with `os.lazy.` are reserved for the system.

Needs: registering a name that is not reserved. An installed LazyRAD app cannot be
granted `msg::serve`; wrap it in `try`/`catch` in app code.

### 51. A service that refuses

Send errors back the way a real service does.

```rhai
msg::serve("demo.rhai.strict", "os.lazy.echo.v1", #{
    Echo: |text, count| {
        if count > 10 { throw #{ code: 22, message: "count is at most 10" }; }
        if text == "" { throw "empty text"; }
        text
    },
});
msg::run(60000);
```

A caller sees an error whose text ends with the errno name:

```rhai
let svc = msg::connect("os.lazy.echo.v1", "demo.rhai.strict");
try {
    svc.echo("x", 99);
} catch (e) {
    print(e);   // os.lazy.echo.v1.Echo: count is at most 10 (EINVAL, code 22)
}
```

`throw "text"` answers with `EIO`; `throw #{ code: N, message: "..." }` picks the
errno (13 `EACCES`, 22 `EINVAL`, 2 `ENOENT`, ...).

Needs: as recipe 50.

### 52. A one-way event service

Handle fire-and-forget calls: methods marked `oneway` in the interface run their
handler and send no reply.

```rhai
let log = [];
msg::serve("demo.rhai.sink", "os.lazy.echo.v1", #{
    Notify: |event| {
        log.push(`${event.topic} at ${event.at}`);
        print(log[log.len() - 1]);
    },
});
msg::run(60000);
```

```rhai
msg::connect("os.lazy.echo.v1", "demo.rhai.sink").notify(#{ topic: "ping", at: 7 });
```

`Notify` takes an `Event` struct, which a script writes as a map; the caller gets
`()` once the message is queued.

Needs: as recipe 50.

## Discovery

### 53. What can I call?

List what is installed and what each interface offers.

```rhai
print(`${msg::interfaces().len()} interfaces are known`);
print("running services:");
for s in msg::services() { print(`  ${s}`); }
print(msg::describe("os.lazy.echo.v1"));
```

`msg::interfaces()` is every interface compiled from `idl/`; `msg::services()` is
every name registered right now. `msg::describe(name)` prints the interface's
documentation, its methods with signatures and its topics, and names the
`sys::<alias>` module that wraps it.

Needs: nothing special; `services()` reads the name table.

### 54. Is a service available?

Test before you rely on it.

```rhai
fn have(interface) {
    try {
        msg::connect(interface);
        return true;
    } catch (e) {
        return false;
    }
}
print(`confd: ${have("os.lazy.confd.v1")}`);
print(`netd: ${have("os.lazy.net.stack.v1")}`);
print(`wifi: ${have("os.lazy.net.wifi.v1")}`);
```

`connect` resolves the service and fails with "no service for ..." when no one
serves it, which is the normal case for optional parts of an image.

Needs: nothing.

### 55. Call a method by its IDL name

Use `invoke` when the sugar does not fit, or a method is named like a built-in.

```rhai
let confd = msg::connect("os.lazy.confd.v1");
print(confd.methods);                           // ["Get", "Set", "Delete", "List", "Info"]
print(confd.invoke("Info"));                    // no arguments
print(confd.invoke("Get", ["sys/ui/theme"]));   // positional arguments in an array
print(confd.get(#{ path: "sys/ui/scale" }));    // one map: named arguments
```

Method names are the IDL name in `snake_case` (`list_topics`); the generic form
takes the IDL spelling (`"ListTopics"`). A method named `invoke`, `call`,
`interface`, `service`, `methods`, `type_of` or `invoke_oneway` is only reachable
through `invoke`.

Needs: `os.lazy.confd.v1`.

### 56. Which topics are in use?

The topics the broker has seen, with how many subscriptions each has.

```rhai
for t in sys::messenger_topics::list_topics() {
    print(`${t.topic}  ${t.subscribers} subscriber(s)${if t.retained { " retained" } else { "" }}`);
}
```

`list_topics()` is the quickest way to see which retained status topics exist
(`system/health/summary`, `system/stats/memory`, ...) and whether anyone is
listening to yours. The broker may refuse it to a restricted label; wrap the call
in `try` as in recipe 57.

Needs: `os.lazy.messenger.topics.v1`.

## Errors and timeouts

### 57. Handle a refused call

A service says no; keep going.

```rhai
fn describe(e) {
    if type_of(e) == "map" && "message" in e { e.message } else { e.to_string() }
}

try {
    sys::confd::set("sys/ui/theme", #{ kind: 3, str_value: "dark" });
    print("changed");
} catch (e) {
    let text = describe(e);
    if text.contains("code 1004") {
        print("not allowed: only an administrator changes sys/** keys");
    } else {
        print(`failed: ${text}`);
    }
}
```

The message starts with the interface and method, then the service's own text,
then the errno name and number: `... (EACCES, code 13)`. `confd` has codes of its
own, shown as `error`: 1001 no value, 1002 bad path, 1003 too large, 1004 not
allowed, 1005 store error, 1006 bad value, so a refused write reads
`os.lazy.confd.v1.Set: the caller may not access that path (error, code 1004)`.
Test for the name or code with `contains`, not for the exact text.

Needs: `os.lazy.confd.v1`.

### 58. A timeout and a retry

Do not hang on a stuck service; try a few times.

```rhai
msg::set_timeout(1500);
let attempt = 0;
let ok = false;
while !ok && attempt < 3 {
    attempt += 1;
    try {
        print(sys::timed::now().tz_name);
        ok = true;
    } catch (e) {
        if e.to_string().contains("ETIMEDOUT") {
            print(`attempt ${attempt}: timed out`);
            sleep(500);
        } else {
            throw e;
        }
    }
}
if !ok { exit(1); }
```

`msg::set_timeout(ms)` applies to every later call in the script (`0` waits for
ever, the default is 5000). A call that times out raises
`... timed out (ETIMEDOUT)`. A service that restarts is found again on the next
call. `throw e;` passes any other error on unchanged.

Needs: `os.lazy.timed.v1`.

### 59. A missing module and a typo

Typos are caught before anything is sent.

```rhai
try {
    sys::confd::set("sys/test/x", #{ kind: 3, str_val: "oops" });
} catch (e) {
    print(e);
}
try {
    msg::connect("os.lazy.nope.v1");
} catch (e) {
    print(e);
}
```

An unknown struct key is reported ("unknown field `str_val`") and so is an
unknown interface ("unknown interface `os.lazy.nope.v1` (see msg::interfaces())").
Wrong argument counts and out-of-range numbers are reported the same way, before
the request leaves.

Needs: nothing.

## Scripts as tools

### 60. Arguments and options

Parse `--flag`, `--name=value` and plain arguments.

```rhai
// args.rhai: rhai args.rhai --verbose --name=lazy file1 file2
let opts = #{};
let rest = [];
for a in os::args() {
    if a.starts_with("--") {
        let kv = a.sub_string(2).split("=", 2);
        opts[kv[0]] = if kv.len() > 1 { kv[1] } else { true };
    } else {
        rest.push(a);
    }
}
print(opts);
print(rest);
```

Example output:

```text
#{"name": "lazy", "verbose": true}
["file1", "file2"]
```

The arguments are everything after the script name, so `rhai` itself never
interprets them (`rhai args.rhai --max-ops 5` gives the script `--max-ops`).

Needs: nothing.

### 61. Exit codes

Tell the shell whether it worked.

```rhai
// check.rhai: succeeds only when the network is up
let have = false;
try {
    for a in sys::net_stack::addresses() {
        if a.addr[0] != 0 { have = true; }
    }
} catch (e) {
    debug(`cannot ask: ${e}`);
    exit(2);
}
exit(if have { 0 } else { 1 });
```

```bash
if rhai check.rhai; then echo online; else echo offline; fi
```

`exit(n)` ends the script with status `n` (modulo 256) and cannot be caught.
An uncaught error is status 1. Use 2 for "could not check", as most tools do.

Needs: `os.lazy.net.stack.v1`.

### 62. A filter in a pipeline

Read standard input, write standard output.

```rhai
// topn.rhai: the longest lines of the input
let n = if os::args().len() > 0 { parse_int(os::args()[0]) } else { 3 };
let lines = [];
for line in stdin_text().split("\n") {
    if line != "" { lines.push(line); }
}
lines.sort(|a, b| if a.len() > b.len() { -1 } else if a.len() < b.len() { 1 } else { 0 });
let shown = 0;
for l in lines {
    print(l);
    shown += 1;
    if shown == n { break; }
}
```

```bash
ls /system/bin | rhai topn.rhai 5
```

`stdin_text()` reads everything once. Anything you `print` goes to standard output;
send diagnostics to `debug` (standard error) so they do not pollute the next stage.

Needs: nothing.

### 63. List a folder, biggest first

Use the `os` functions for files.

```rhai
// big.rhai: rhai big.rhai /home/user
let dir = if os::args().len() > 0 { os::args()[0] } else { "." };
let files = [];
for e in ls(dir) {
    if e.kind == "file" { files.push(e); }
}
files.sort(|a, b| if a.size > b.size { -1 } else if a.size < b.size { 1 } else { 0 });
for f in files {
    print(`${f.size}  ${f.name}`);
}
```

`ls` returns maps with `name`, `size` and `kind` (`file`, `dir`, `symlink`,
`other`), sorted by name; reading a folder with more than `--max-array` entries
is an error.

Needs: read access to the folder.

### 64. Keep a small state file

Store a counter in JSON between runs.

```rhai
let file = os::env("HOME") + "/.counter.json";
let state = #{ runs: 0 };
try {
    state = parse_json(read(file));
} catch (e) {
    // first run: no file yet
}
state.runs += 1;
write(file, state.to_json());
print(`this script has run ${state.runs} time(s)`);
```

`write` replaces the file; there is no append, so keep the whole state in one
value. For a setting other programs should see, use `confd` instead (recipe 5).

Needs: write access to your home.

### 65. A script you can run by name

Give a script a shebang line and make it executable.

```rhai
// whoami.rhai
let u = sys::accounts::lookup(os::env("USER"), ());
print(`${u.user.name} (uid ${u.user.uid}) in ${u.user.home}`);
if u.user.admin { print("administrator"); }
```

Start the file with the line `#!/bin/rhai`, then:

```bash
chmod +x whoami.rhai
./whoami.rhai
```

The `#!` line is ignored by Rhai and read by the kernel. Put such scripts in a
folder on your `PATH` (or call them with a path). `#!/usr/bin/env rhai` and
options after the interpreter (`#!/bin/rhai --sandbox`) work too.

Needs: `os.lazy.accounts.v1`.

## Long-running scripts

### 66. A background job with a clean stop

A loop that does periodic work and reacts to a "stop" message.

```rhai
// daemon.rhai: rhai daemon.rhai &   ...   rhai -e 'msg::publish("demo/daemon/stop", "x")'
let running = true;
let ticks = 0;
msg::on("demo/daemon/stop", |e| {
    running = false;
    msg::stop();
});
msg::on("demo/daemon/ping", |e| print("pong"));

let next_work = clock();
while running {
    msg::run(1000);                 // wait up to a second for messages
    if running && clock() >= next_work {
        ticks += 1;
        print(`work ${ticks}`);
        next_work = clock() + 5.0;  // every five seconds
    }
}
print("stopped");
```

`msg::run(ms)` returns after `ms` milliseconds even if nothing arrived, which
lets the loop do its own timed work between messages; `msg::stop()` makes the
current `run` return immediately. `clock()` is a float in seconds.

Needs: topics `demo/daemon/#`.

### 67. Run a task every minute, with a cap

Poll something on a schedule and give up after a while.

```rhai
let deadline = clock() + 600.0;     // ten minutes
let last = ();
while clock() < deadline {
    let h = sys::healthd::status();
    if last != h.summary.status {
        print(`health: ${h.summary.status}`);
        last = h.summary.status;
    }
    sleep(60000);
}
```

A single `sleep` may be at most an hour, and the whole script is bound by
`--max-ops` (10 million operations); sleeping is not counted as operations, so a
polling loop stays well inside it. For a loop that does real work all day,
start it with `rhai --max-ops 0 daemon.rhai`.

Needs: `os.lazy.healthd.v1`.

### 68. Run something once the desktop is ready

Wait for the shell, then open a document.

```rhai
// start.rhai
let tries = 0;
while tries < 30 {
    try {
        let st = sys::shell::status();
        if st.menu.len() > 0 { break; }
    } catch (e) {
        // the shell is not serving yet
    }
    tries += 1;
    sleep(1000);
}
sys::mimed::open("/home/user/notes.md", "open");
```

`connect` retries the lookup on every call, so a script can wait for a service
that has not started yet by trying until it answers.

Needs: `os.lazy.shell.v1`, `os.lazy.mimed.v1`.

## See also

- [Scripting on LazyOS](index.md): what Rhai is used for here
- [The `rhai` command](command.md): options, the `os` module, elevated actions
- [The Rhai language](language.md): the language tour and its surprises
- [`msg`](msg.md): calling, topics, serving, errors
- [API reference](api/index.md): every `sys::` module, for example [confd](api/confd.md),
  [notify](api/notify.md), [mimed](api/mimed.md), [init](api/init.md), [pkgd](api/pkgd.md),
  [elevd](api/elevd.md), [sysmond](api/sysmond.md), [net_stack](api/net_stack.md)
- [LazyRAD scripting](../lazyrad/scripting.md) and [LazyRAD on LazyOS](../lazyrad/lazyos.md):
  the same calls inside forms and installed apps
- [Messenger](../messenger.md) and the [topics catalogue](../topics-catalog.md):
  the system behind the calls
