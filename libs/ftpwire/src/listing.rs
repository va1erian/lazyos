//! Directory listings: the `MLSD` machine format (RFC 3659) and the Unix
//! `ls -l` lines most servers send for `LIST`. Both are server text: a line
//! that does not parse is skipped, never guessed at, and a name that could
//! not be a single path component (empty, `.`, `..`, a `/`, a control
//! character) is dropped, so a hostile server cannot make a client walk out
//! of the directory it listed.

use alloc::string::String;
use alloc::vec::Vec;

/// The most entries one listing yields.
pub const MAX_ENTRIES: usize = 1 << 16;
/// The longest entry name kept, bytes.
pub const MAX_NAME: usize = 255;

/// One entry of a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Modification time, Unix seconds (UTC), when the server gave one this
    /// side could read (`MLSD`'s `modify` fact).
    pub mtime: Option<i64>,
}

/// Whether `name` can be one path component.
pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name != "."
        && name != ".."
        && !name.chars().any(|c| c == '/' || c.is_control())
}

/// Every entry of a listing body (`MLSD` facts when `mlsd`, else `LIST`
/// lines), in the server's order, at most [`MAX_ENTRIES`].
pub fn parse_listing(body: &[u8], mlsd: bool) -> Vec<ListEntry> {
    let text = String::from_utf8_lossy(body);
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter_map(|line| {
            if mlsd {
                parse_mlsd_line(line)
            } else {
                parse_list_line(line)
            }
        })
        .take(MAX_ENTRIES)
        .collect()
}

/// One `MLSD` line: `fact=value;fact=value; name`. Only `type=file` and
/// `type=dir` entries are kept (`cdir`, `pdir`, links and devices are not).
pub fn parse_mlsd_line(line: &str) -> Option<ListEntry> {
    let (facts, name) = line.split_once(' ')?;
    let mut kind = None;
    let mut size = 0;
    let mut mtime = None;
    for fact in facts.split(';').filter(|f| !f.is_empty()) {
        let (key, value) = fact.split_once('=')?;
        match key.to_ascii_lowercase().as_str() {
            "type" => kind = Some(value.to_ascii_lowercase()),
            "size" => size = value.parse().ok()?,
            "modify" => mtime = parse_modify(value),
            _ => {}
        }
    }
    let dir = match kind.as_deref()? {
        "file" => false,
        "dir" => true,
        _ => return None,
    };
    valid_name(name).then(|| ListEntry {
        name: String::from(name),
        dir,
        size,
        mtime,
    })
}

/// One Unix `ls -l` line: `perms links owner group size month day time-or-year
/// name`. Regular files (`-`) and directories (`d`) are kept; a name keeps its
/// inner spaces.
pub fn parse_list_line(line: &str) -> Option<ListEntry> {
    let dir = match line.as_bytes().first()? {
        b'-' => false,
        b'd' => true,
        _ => return None,
    };
    // The name starts after the eighth whitespace-separated field.
    let mut rest = line;
    let mut fields = Vec::with_capacity(8);
    for _ in 0..8 {
        rest = rest.trim_start_matches(' ');
        let end = rest.find(' ')?;
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    let name = rest.strip_prefix(' ')?;
    let size = fields[4].parse().ok()?;
    valid_name(name).then(|| ListEntry {
        name: String::from(name),
        dir,
        size,
        mtime: None,
    })
}

/// `YYYYMMDDHHMMSS[.sss]` (UTC) as Unix seconds.
pub fn parse_modify(value: &str) -> Option<i64> {
    let digits = value.split('.').next()?;
    if digits.len() != 14 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |range: core::ops::Range<usize>| digits[range].parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(4..6)?, field(6..8)?);
    let (hour, minute, second) = (field(8..10)?, field(10..12)?, field(12..14)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
