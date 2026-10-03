// A ProTracker MOD player written in LazyRAD (docs/lazyrad-modplay.md).
//
// The player hands this script the `modplay` module: it parses a song and
// plays it on a *deck* through the system mixer. Everything you see is this
// script: the playlist, the transport, the pattern view (the `cells` module
// turns cells into text), the channel meters and mutes, the mix controls.
//
// The deck calls back twice: `show_deck` while it plays (about twelve times
// a second, with the row you are hearing), and `deck_ended` when the song is
// over.

// Rows in the pattern view; the playing one is in the middle.
fn visible_rows() {
    9
}

fn form_load() {
    form.state.deck = ();
    form.state.song = ();
    form.state.current = -1;
    form.state.shown_row = -1;
    form.state.shown_order = -1;
    scan_songs();
    load_entry(0);
    status_label.text = if modplay::sound_available() {
        "Ready. Press Play."
    } else {
        "No sound card: songs play silently (start LazyOS with --sound)."
    };
}

// ---------------------------------------------------------------- songs ---

// The playlist: the built-in song, then every .mod in the app's `music`
// folder and in the project folder.
fn scan_songs() {
    let songs = [#{ name: demo_song::title() + " (built in)", path: "" }];
    for folder in ["music", app.path] {
        let names = [];
        try {
            names = dir_list(folder);
        } catch {
            // No such folder yet, or outside what this app may read.
        }
        for name in names {
            if name.to_lower().ends_with(".mod") {
                songs.push(#{ name: name, path: folder + "/" + name });
            }
        }
    }
    form.state.songs = songs;
    let rows = [];
    for song in songs {
        rows.push(song.name);
    }
    songs_list.items = rows;
}

// Load playlist entry `index` and show it (without playing).
fn load_entry(index) {
    let entry = form.state.songs[index];
    let song = if entry.path == "" {
        modplay::decode(demo_song::data())
    } else {
        modplay::load(entry.path)
    };
    form.state.song = song;
    form.state.current = index;
    songs_list.selected = index;
    title_label.text = "Song: " + if song.title == "" { entry.name } else { song.title };
    info_label.text = `${song.length} orders, ${song.patterns} patterns, ${count_samples(song)} instruments`;
    song_progress.max = song.length * song.rows;
    song_progress.value = 0;
    show_samples(song);
    show_pattern(song, 0, 0);
    position_label.text = `Order 1/${song.length}  Pattern ${song.pattern_at(0)}  Row 00`;
    time_label.text = "0:00";
}

fn count_samples(song) {
    let used = 0;
    for sample in song.samples {
        if sample.length > 0 {
            used += 1;
        }
    }
    used
}

// Instruments: every slot that has a sound or a name (musicians hide
// messages in sample names).
fn show_samples(song) {
    let rows = [];
    for sample in song.samples {
        if sample.length > 0 || sample.name != "" {
            let number = cells::hex(sample.number, 2);
            let about = if sample.length > 0 {
                `  (${sample.length} bytes, vol ${sample.volume}${if sample.looped { ", loop" } else { "" }})`
            } else {
                ""
            };
            rows.push(number + "  " + sample.name + about);
        }
    }
    samples_list.items = rows;
}

// ------------------------------------------------------------- playback ---

// Start the loaded song from the top on a new deck, with the mix settings
// the controls show.
fn start() {
    stop_deck();
    let deck = modplay::play(form.state.song, #{
        volume: volume_slider.value.to_int(),
        separation: separation_slider.value.to_int(),
        interpolate: interpolate_check.checked,
    });
    let channel_buttons = [ch1_button, ch2_button, ch3_button, ch4_button];
    for channel in 0..4 {
        deck.mute(channel, !channel_buttons[channel].checked);
    }
    deck.on_update(Fn("show_deck"));
    deck.on_end(Fn("deck_ended"));
    form.state.deck = deck;
    form.state.shown_row = -1;
    status_label.text = "Playing " + songs_list.items[form.state.current] + ".";
}

fn stop_deck() {
    if form.state.deck != () {
        form.state.deck.stop();
        form.state.deck = ();
    }
    clear_meters();
}

fn clear_meters() {
    for meter in [ch1_meter, ch2_meter, ch3_meter, ch4_meter] {
        meter.value = 0;
    }
}

// What the deck is playing now (the row you hear, not the row being mixed).
fn show_deck(deck) {
    let song = form.state.song;
    let levels = deck.levels;
    ch1_meter.value = levels[0];
    ch2_meter.value = levels[1];
    ch3_meter.value = levels[2];
    ch4_meter.value = levels[3];
    let seconds = deck.elapsed_ms / 1000;
    let padding = if seconds % 60 < 10 { "0" } else { "" };
    time_label.text = `${seconds / 60}:${padding}${seconds % 60}   speed ${deck.speed}   ${deck.tempo} BPM`;
    if deck.row == form.state.shown_row && deck.order == form.state.shown_order {
        return;
    }
    form.state.shown_row = deck.row;
    form.state.shown_order = deck.order;
    let row_text = if deck.row < 10 { "0" } else { "" };
    position_label.text = `Order ${deck.order + 1}/${song.length}  Pattern ${deck.pattern}  Row ${row_text}${deck.row}`;
    song_progress.value = deck.order * song.rows + deck.row;
    show_pattern(song, deck.order, deck.row);
    let notes = [ch1_note, ch2_note, ch3_note, ch4_note];
    let row = song.row(deck.order, deck.row);
    for channel in 0..4 {
        if row[channel].period != 0 {
            notes[channel].text = cells::note_name(row[channel].period) + " " + cells::hex(row[channel].sample, 2);
        }
    }
}

fn show_pattern(song, order, row) {
    pattern_caption.text = `Pattern ${song.pattern_at(order)}   (order ${order + 1} of ${song.length})`;
    pattern_list.items = cells::window(song, order, row, visible_rows());
    pattern_list.selected = visible_rows() / 2;
}

// The song played out: again, the next song, or done.
fn deck_ended(deck) {
    clear_meters();
    if repeat_check.checked {
        deck.seek(0);
        status_label.text = "Repeating.";
    } else if form.state.current + 1 < form.state.songs.len() {
        load_entry(form.state.current + 1);
        start();
    } else {
        status_label.text = `Finished (${deck.elapsed_ms / 1000} s).`;
    }
}

// ------------------------------------------------------------ transport ---

fn play_button_click() {
    let deck = form.state.deck;
    if deck != () && deck.status == "paused" {
        deck.resume();
        status_label.text = "Playing.";
    } else {
        start();
    }
}

fn pause_button_click() {
    let deck = form.state.deck;
    if deck == () {
        return;
    }
    if deck.status == "paused" {
        deck.resume();
        status_label.text = "Playing.";
    } else if deck.status == "playing" {
        deck.pause();
        clear_meters();
        status_label.text = "Paused.";
    }
}

fn stop_button_click() {
    stop_deck();
    status_label.text = "Stopped.";
}

fn prev_button_click() {
    let deck = form.state.deck;
    if deck != () && deck.order > 0 {
        deck.seek(deck.order - 1);
    }
}

fn next_button_click() {
    let deck = form.state.deck;
    if deck != () && deck.order + 1 < form.state.song.length {
        deck.seek(deck.order + 1);
    }
}

fn songs_list_activate(index) {
    if index < 0 || index >= form.state.songs.len() {
        return;
    }
    try {
        load_entry(index);
        start();
    } catch (error) {
        status_label.text = `Cannot play ${form.state.songs[index].name}: ${error}`;
    }
}

fn rescan_button_click() {
    scan_songs();
    songs_list.selected = form.state.current;
    status_label.text = `${form.state.songs.len()} song(s). Put .mod files in this app's music folder.`;
}

// ------------------------------------------------------------------ mix ---

fn volume_slider_change(value) {
    volume_value.text = `${value.to_int()}%`;
    if form.state.deck != () {
        form.state.deck.volume = value.to_int();
    }
}

fn separation_slider_change(value) {
    separation_value.text = `${value.to_int()}%`;
    if form.state.deck != () {
        form.state.deck.separation = value.to_int();
    }
}

fn interpolate_check_toggle(checked) {
    if form.state.deck != () {
        form.state.deck.interpolate = checked;
    }
}

// A latched channel button means "on"; not `mute`, which would shadow the
// deck's own `mute` method inside this function.
fn set_channel(channel, on) {
    if form.state.deck != () {
        form.state.deck.mute(channel, !on);
    }
}

fn ch1_button_toggle(checked) { set_channel(0, checked); }
fn ch2_button_toggle(checked) { set_channel(1, checked); }
fn ch3_button_toggle(checked) { set_channel(2, checked); }
fn ch4_button_toggle(checked) { set_channel(3, checked); }
