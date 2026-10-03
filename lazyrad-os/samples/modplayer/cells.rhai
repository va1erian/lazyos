// Pattern cells as a tracker shows them: "C-2 01 A0F".
//
// A cell is the map `song.cell(order, row, channel)` returns:
// #{ period, sample, effect, param }. Everything here is plain Rhai; the
// player only hands over the numbers.

// The ProTracker period of every note from C-1 to B-3 (finetune 0).
fn periods() {
    [856, 808, 762, 720, 678, 640, 604, 570, 538, 508, 480, 453,
     428, 404, 381, 360, 339, 320, 302, 285, 269, 254, 240, 226,
     214, 202, 190, 180, 170, 160, 151, 143, 135, 127, 120, 113]
}

// "C-2" for a period, the nearest note when the period is between two (a
// finetuned sample), "---" for no note.
fn note_name(period) {
    if period == 0 {
        return "---";
    }
    let names = ["C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-"];
    let table = periods();
    let best = 0;
    let best_distance = 100000;
    for i in 0..table.len() {
        let distance = (table[i] - period).abs();
        if distance < best_distance {
            best = i;
            best_distance = distance;
        }
    }
    names[best % 12] + `${best / 12 + 1}`
}

// `value` as `digits` upper-case hex digits.
fn hex(value, digits) {
    let text = "";
    for i in 0..digits {
        let shift = 4 * (digits - 1 - i);
        text += "0123456789ABCDEF"[(value >> shift) & 15];
    }
    text
}

// "C-2 01 A0F"; dots for the empty parts, like a tracker.
fn cell_text(cell) {
    let sample = if cell.sample == 0 { ".." } else { hex(cell.sample, 2) };
    let fx = if cell.effect == 0 && cell.param == 0 {
        "..."
    } else {
        hex(cell.effect, 1) + hex(cell.param, 2)
    };
    `${note_name(cell.period)} ${sample} ${fx}`
}

// "17 | C-2 01 A0F | --- .. ... | ..."; an empty string past the pattern.
// The bars keep the channels apart in a proportional font.
fn row_text(song, order, row) {
    if row < 0 || row >= song.rows {
        return "";
    }
    let text = if row < 10 { "0" } else { "" };
    text += `${row}`;
    for cell in song.row(order, row) {
        text += " | " + cell_text(cell);
    }
    text
}

// `count` rows centred on `row` (blank above the first and below the last),
// for a pattern view that keeps the playing row in the middle.
fn window(song, order, row, count) {
    let first = row - count / 2;
    let rows = [];
    for r in first..first + count {
        rows.push(row_text(song, order, r));
    }
    rows
}
