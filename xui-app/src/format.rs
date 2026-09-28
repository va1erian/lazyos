//! Small number/text formatters shared by the viewers.

/// A byte count in binary units, one decimal (`1.5 MiB`).
pub fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", count, UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// PIT ticks (100 Hz) as `H:MM:SS` or `M:SS`.
pub fn uptime(ticks: u64) -> String {
    let seconds = ticks / 100;
    let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// An interface id as a stable `0x` hex token.
pub fn hex_id(id: u64) -> String {
    format!("0x{id:016x}")
}

/// The first `keep` characters of `text`, with a trailing `…` when truncated.
pub fn clip(text: &str, keep: usize) -> String {
    if text.chars().count() <= keep {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(keep.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}
