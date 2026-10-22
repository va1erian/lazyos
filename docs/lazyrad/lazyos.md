# LazyRAD on LazyOS: what only scripts here get

A LazyRAD form script on LazyOS runs in `lrplay`, the player, and gets three
things a desktop build of LazyRAD does not: **`msg` and `sys::*`**, to call
every system service over Messenger; **`modplay`**, to play ProTracker songs
through the system mixer; and a **file sandbox** rooted in the user's home.
This page covers those, the folders they use and the serial markers the player
prints. The portable functions are in [the standard library](stdlib.md); how
handlers and forms work is in [Scripting](scripting.md).

The modules exist only inside `lrplay` on LazyOS, and they are the only
extensions it adds to the standard library. A project played by a desktop build
of LazyRAD has no `msg`, `sys` or `modplay`: calling them fails at run time with
a "module not found" error, and the project check warns about the calls.

| You get | Where it is documented |
|---|---|
| `msg::*` (generic Messenger calls, topics, services) | [`msg`](../rhai/msg.md) |
| `sys::<service>::*` (one documented module per interface) | [Messenger API reference](../rhai/api/index.md) |
| `modplay::*`, `Song`, `Deck` | below |
| the sandbox and data folders | below and [the standard library](stdlib.md) |

## Messenger in a form: `msg` and `sys::*`

Every form script can call any Messenger service the app's permissions allow.
The generated `sys::*` modules are the readable way:

```rhai
fn form_load() {
    // Read a setting.
    let theme = sys::confd::get("sys/ui/theme");
    if theme == () {
        theme_label.text = "theme: (unset)";
    } else {
        theme_label.text = `theme: ${theme.str_value}`;
    }

    // React when any key under sys/ui changes.
    sys::confd::on_changed("sys/ui/#", Fn("theme_changed"));
}

fn theme_changed(event) {
    theme_label.text = `changed: ${event.payload.path}`;
}

fn open_button_click() {
    try {
        // Hand a file to whichever app is registered to open it.
        sys::mimed::open(path_edit.text, "open");
    } catch (error) {
        status_label.text = `${error}`;
    }
}
```

`msg::connect("os.lazy.confd.v1").get("sys/ui/theme")` is the same call
spelled with strings. The rules that are specific to forms, on top of
[`msg`](../rhai/msg.md) (calls, values, topics, services, errors):

* **Events need no loop.** A form never calls `msg::run()`; it is an error in a
  form, and a message says events arrive on their own. A handler passed to an
  `on_*` helper or `msg::on`, and a call to a service the form serves, run on
  the form's window. While a form has anything registered, its window polls
  Messenger every 50 ms **without blocking** and runs what is ready in the
  form's script, between window events. The window keeps painting.
* **Errors in a handler** are shown like an event handler's error: a `LazyRAD`
  message box with the located error, and the program keeps running. A handler
  that fails on every event is shown once; the repeats are counted on standard
  error.
* **Closing a form** unsubscribes its topics and withdraws the names it served
  (hot reload does the same before the rebuilt form's `form_load`).
* **Blocking calls block the window.** A call waits up to `msg::timeout()`
  (default 5 seconds) for a stuck service; keep slow calls out of click
  handlers, or lower the timeout with `msg::set_timeout(ms)`.
* **All forms share one connection** to the fabric, and a service is resolved
  once per process. The generated modules are compiled once per process.
* **Scripts act with the player's own credentials.** There is no extra
  authority: a call the user's session or the app's permissions would refuse
  fails with an error that ends in `permission denied (EACCES)`.

### Permissions of an installed app

An installed app runs under kernel rules compiled from its manifest, so the
package has to declare the services its scripts use. File -> Make LazyOS App
(the IDE) and `tools/lazyrad/package.py` **derive** those from the scripts by
reading their source:

| What the scripts contain | What is declared |
|---|---|
| `sys::<alias>::function(...)` | that module's interface (`sys::confd::get` gives `os.lazy.confd.v1`) |
| a topic helper (`on_changed`, `subscribe_...`, `publish_...`) | `subscribe:` or `publish:` for the topic's filter; literal leading arguments narrow it, so `on_changed("sys/ui/#", ...)` declares `subscribe:system/confd/changed/sys/ui/#` |
| a string literal passed to `msg::connect`, `msg::on`, `msg::subscribe`, `msg::publish` | that interface or topic |
| the text `modplay::play` | `os.lazy.audio.v1` |
| always, for the player itself | `os.lazy.input.v1` (keyboard) and `os.lazy.display.cursor.v1` (pointer shape); the packager adds the display |

Limits worth knowing:

* A name built **at run time** (`msg::connect(name_edit.text)`, a topic made by
  string concatenation) cannot be seen; the consent screen lists what was
  found. A topic helper called with a computed argument declares the bare
  pattern, which the kernel's per-segment check refuses past its wildcard.
* **Serving** a name (`msg::serve`) is a development feature: an installed
  app's manifest cannot grant it, so wrap it in `try`/`catch`. An error that
  escapes `form_load` ends the app (`init` restarts it).
* Methods whose request carries a channel, buffer or ring are not generated for
  scripts at all.
* **Play** runs the project under the label the installed app would get (when
  the IDE is the package, after a consent prompt), so a refusal in Play is a
  refusal the installed app would show. Otherwise Play inherits the IDE's
  permissions and the output pane adds a note when a call is refused.
  [Packaging](packaging.md) has the details.
* In the IDE process itself the `msg`, `sys` and `modplay` modules are not
  registered (only the player does that), so the project check may list calls
  into them under *names a module the project and its extensions do not
  define*. It is a warning; it does not stop Play.

## `modplay`: ProTracker songs

`modplay` loads a four-channel ProTracker module (`.mod`, signature `M.K.` or
`4CHN`) and plays it through the system mixer. The song is parsed and mixed in
native code; the script only decides what to play and shows what is heard
(position, channel levels, the end). The LazyOS MOD Player sample is written
this way; see [the MOD player](../lazyrad-modplay.md).

```rhai
let deck = ();

fn form_load() {
    // A song the app may read, or base64 text a script module carries.
    let song = modplay::load(app.path + "/music/tune.mod");
    title_label.text = song.title;
    deck = modplay::play(song, #{ volume: 80, separation: 50 });
    deck.on_update(Fn("show_deck"));
    deck.on_end(Fn("deck_ended"));
}

fn show_deck(d) {
    row_label.text = `${d.order}:${d.row}`;
}

fn deck_ended(d) {
    status_label.text = "done";
}

fn pause_button_click() {
    if deck.playing { deck.pause(); } else { deck.resume(); }
}
```

### Functions

| Function | Returns | Fails when | Example |
|---|---|---|---|
| `modplay::load(path: string)` | `Song` | the file sandbox refuses the path (``access denied for "...": ...``); the file cannot be read (`` `path`: <reason> ``); it is not a module (`` `path`: not a ProTracker module: <reason> ``) | `let s = modplay::load("music/tune.mod");` |
| `modplay::decode(text: string)` | `Song` from standard base64 (`A-Z a-z 0-9 + /`, `=` padding; whitespace is ignored, so a long string may span lines) | `base64: the length is not a multiple of 4`, ``base64: unexpected `c` ``, or the data is not a module | `let s = modplay::decode(demo_song::data());` |
| `modplay::play(song)` | `Deck`, already playing, with the defaults below | no audio stream can be opened (the mixer's error text) | `let d = modplay::play(s);` |
| `modplay::play(song, options: map)` | `Deck` | as above, or an option is wrong (below) | `let d = modplay::play(s, #{ loops: 0 });` |
| `modplay::sound_available()` | bool: whether a mixer is running | never | `if !modplay::sound_available() { hint_label.text = "no sound card"; }` |

* `load` reads the real file system through the **same sandbox** as
  `file_read_bytes` (so the app's data folder, its own project folder and the
  documents it was started to open work), but not through the project's
  in-memory file view, and it builds its own copy of the policy: a file picked
  in `open_file_dialog` is not granted to it. For a song shipped in the project,
  build the path from `app.path`.
* The module's reasons are: `the file is too short`, `no M.K./4CHN signature`,
  `only four-channel modules are supported`, `the song length is invalid`,
  `the order list names an impossible pattern`, `the pattern data is truncated`.

**Options** (all optional; an unknown key is an error naming the valid ones;
a value of the wrong type or range is ``modplay::play: `key` must be an integer 0..=100``):

| Key | Type and range | Default | Meaning |
|---|---|---|---|
| `loops` | int 0 to 1000 | 1 | full plays of the song before it ends; `0` plays forever |
| `separation` | int 0 to 100 | 50 | stereo separation: 0 is mono, 100 hard Amiga panning |
| `volume` | int 0 to 100 | 100 | the deck's volume, percent |
| `interpolate` | bool | `true` | linear interpolation between sample bytes |
| `rate` | int 8000 to 48000 | 22050 | mixing rate, Hz (the mixer may round it) |

### `Song`

A parsed module, shared by every deck that plays it. Names from the file are
shown with non-ASCII bytes as `?`.

| Member | Type | Meaning |
|---|---|---|
| `song.title` | string | the module's title |
| `song.length` | int | number of orders (positions in the play list) |
| `song.patterns` | int | number of distinct patterns |
| `song.restart` | int | the restart order |
| `song.channels`, `song.rows` | int | always 4 and 64 |
| `song.orders` | array of int | the pattern played at each order |
| `song.samples` | array of maps | one per sample slot: `#{ number, name, length, volume, finetune, looped }` (`number` starts at 1) |
| `song.pattern_at(order)` | int | the pattern at `order`, or `-1` past the end |
| `song.row(order, row)` | array of 4 maps | the four cells of a row (see `cell`); empty cells outside the song |
| `song.cell(order, row, channel)` | map | `#{ period, sample, effect, param }`, all ints; all zero outside the song |

### `Deck`

A deck is a song playing. Its properties report what is **heard**, not what has
been mixed (the deck renders up to about 0.74 s ahead).

| Member | Type | Meaning |
|---|---|---|
| `deck.status` | string | `"playing"`, `"paused"`, `"ended"` or `"stopped"` |
| `deck.playing` | bool | `status == "playing"` |
| `deck.order`, `deck.row`, `deck.pattern` | int | where the song is |
| `deck.speed`, `deck.tempo` | int | the song's current speed and tempo |
| `deck.levels` | array of 4 ints | each channel's level, 0 to 64 |
| `deck.elapsed_ms` | int | milliseconds heard so far |
| `deck.sound` | bool | whether a sound card is playing it; `false` means a silent run at real-time speed |
| `deck.song` | `Song` | the song |
| `deck.volume` | int, read/write | 0 to 100 (values are clamped); setting can fail if the audio stream does |
| `deck.separation` | int, read/write | 0 to 100 (clamped) |
| `deck.interpolate` | bool, read/write | interpolation on or off |
| `deck.pause()`, `deck.resume()` | `()` | pause without losing the place; either fails if the audio stream does |
| `deck.stop()` | `()` | stop for good (a stopped deck is released) |
| `deck.seek(order)` | `()` | jump to an order (a negative one means 0); it also plays an ended deck again |
| `deck.mute(channel, muted)` | `()` | mute or unmute channel 0 to 3 (an invalid channel does nothing) |
| `deck.muted(channel)` | bool | whether the channel is muted (`false` for an invalid one) |
| `deck.on_update(f)` | `()` | register `f(deck)` (below) |
| `deck.on_end(f)` | `()` | register `f(deck)` (below) |

`f` is a closure or `Fn("name")`, and takes exactly one parameter, the deck.

* `on_update` runs on the window thread between events: once at the start, then
  whenever the status or the heard position changed, **at most every 80 ms**,
  and once more when the state changes (pause, end).
* `on_end` runs **once** when the song has been heard to its last frame
  (`seek` starts a new play, which can end again).
* A handler that throws is reported once and **dropped**, so a broken meter
  cannot flood the screen with message boxes.
* The window polls every 40 ms while a deck is playing; a paused deck costs
  nothing. A deck the script no longer holds keeps playing until it ends or the
  window closes; **closing the window stops its decks**.

Without a sound card (`modplay::sound_available()` is `false`) a deck still
plays: positions, levels, events and the end arrive at the song's real speed,
just silently. In a `LAZYOS_SOUND` image the mixer is `audiod`; sound is checked
by recording it, see [the MOD player](../lazyrad-modplay.md).

A function you define must not share a name and parameter count with a deck or
song method you call inside it (`fn mute(channel, on) { deck.mute(channel, on); }`
recurses until `Stack overflow`); name it `set_channel`. See the lint warning in
[Scripting](scripting.md).

## Where things live

`<home>` is `$HOME` (`/home/<name>`). If it is unset or unusable the home is
the RAM disk `/transient/lazyrad` and nothing is kept after a reboot.

| Path | What |
|---|---|
| `<home>/.apps/<system_name>/` | an installed app's private folder: the only place it can write, and where relative file paths point |
| `<home>/.apps/os.lazy.lazyrad/data/` | the private folder of a project run by Play, from a terminal or by `lrplay <project>` |
| `<home>/.apps/os.lazy.lazyrad/config/` | the IDE's settings |
| `<home>/projects/` | where the IDE keeps and starts projects; the designer preview's sandbox |
| `/apps/<system_name>/<version>-<hash>/` | an installed package: `bin/` has the player, `resources/project/` the project (read only) |
| `/system/share/lazyrad/<sample>` | the samples every LazyRAD image embeds: `hello`, `calculator`, `todo`, `brickbreaker`, `messenger`, `modplayer` |

Run a project by hand from the Terminal (the player of the LazyRAD package):

```bash
$(echo /apps/os.lazy.lazyrad/*/bin/lrplay.elf) --client /system/share/lazyrad/hello &
```

`lrplay` takes `--client` (inside a desktop session), `--debug` (under the IDE's
debugger), `--project <path>` (a path relative to its install directory) or a
positional project (a directory or an `.lrp`). Any other positional path is a
**document**: it arrives in `app.documents`, readable together with its folder.
At most 64 documents are accepted. `mimed` does this when a file is
double-clicked in Files and the app registered for its type.

## Serial markers

The player writes one line per event to the serial console through the trace
API, in the form `LRPLAY:<STAGE>:<PASS|FAIL|WARN>[:<detail>]`. Session scripts
and the harnesses wait for them; you can read them with the debugging tools.

| Marker | When |
|---|---|
| `LRPLAY:PROJECT:PASS:<project> exe=<program>` | the project and the player's own path were resolved |
| `LRPLAY:HOME:WARN:<text>` | `$HOME` is unset; files go to `/transient/lazyrad` |
| `LRPLAY:DOCUMENT:PASS:<path>` | one line per document the program was started to open |
| `LRPLAY:DATA:PASS:<folder>` | the private read/write folder |
| `LRPLAY:MSG:PASS` | `msg` and `sys::*` are installed in the form engines |
| `LRPLAY:MSGEVENT:PASS` | the first Messenger handler ran without error |
| `LRPLAY:DEBUG:PASS` | started under the IDE's debugger |
| `LRPLAY:MODPLAY:PASS:title="<t>" audio=<0 or 1> rate=<Hz>` | a script started a song; `LRPLAY:MODPLAY:FAIL:<why>` when its sound stops with an error |
| `LRPLAY:MODEND:PASS:elapsed_ms=<n>` | a song played out |
| `LRPLAY:UP:PASS` | the first frame reached the compositor |
| `LRPLAY:EVENT:PASS` | the first script event handler ran without error |
| `LRPLAY:EXIT:PASS` | the loop ended cleanly |
| `LRPLAY:ARGS:FAIL:<why>`, `LRPLAY:BIND:FAIL:<why>`, `LRPLAY:RUN:FAIL:<why>` | bad command line; no display; the program stopped with an error |
| `LRPLAY:REASON:PASS:<text>` | the reason a failed run gave `init`, shown in the "stopped unexpectedly" notice |

An image built with `LAZYOS_UI_PROBE=1` also prints one `UI:RECT`/`UI:WIDGET`
line per named control of the startup form, so a session script can click a
control by name.

## See also

* [Scripting](scripting.md): handlers, lifecycle, errors, lint warnings
* [The standard library](stdlib.md): files, the sandbox, dialogs, timers
* [`msg`: Messenger from Rhai](../rhai/msg.md)
* [Messenger API reference](../rhai/api/index.md): every `sys::*` module
* [Packaging apps](packaging.md): Make LazyOS App, permissions, consent
* [The IDE](ide.md): Play, the debugger, the error list
* [The MOD player and `modplay`](../lazyrad-modplay.md)
* [The Picture Viewer](../lazyrad-pictures.md): a document-opening LazyRAD app
* [Messenger APIs in LazyRAD (plan)](../lazyrad-messenger-plan.md)
