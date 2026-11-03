//! What the host is told when it starts: the boot arguments `init` passes
//! `btd` (docs/bluetooth-plan.md B3). Pairing is a harness-only argument in
//! this phase; B5 brings the real interface.

use alloc::vec::Vec;

use bthci::BdAddr;

/// Targets one `pair=` list may hold.
pub const MAX_TARGETS: usize = 4;
/// How long the pairing window stays open after the host is armed.
pub const PAIR_WINDOW_MS: u64 = 60_000;

/// Who may pair while the window is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairPolicy {
    /// Nobody: the default of every build that is not a harness build.
    Off,
    /// The host pages these addresses and pairs with them; nothing else is
    /// admitted unbonded.
    Targets(Vec<BdAddr>),
    /// Any device that pages the host in the window is admitted to pair.
    Any,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Harness builds only (`trace=1`): echo report contents, key edges and
    /// the passkey. Never on by default: the trace would carry typed
    /// passwords.
    pub trace_reports: bool,
    pub pair: PairPolicy,
    /// LE devices that may pair while the window is open (`pair=le:AA:..`);
    /// `pair=any` admits any LE HID advertiser too.
    pub le_targets: Vec<BdAddr>,
    pub pair_window_ms: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            trace_reports: false,
            pair: PairPolicy::Off,
            le_targets: Vec::new(),
            pair_window_ms: PAIR_WINDOW_MS,
        }
    }
}

impl Config {
    /// Read the service arguments: `trace=1`, `pair=any`, `pair=none`, or
    /// `pair=AA:BB:CC:DD:EE:FF[,...]`. Anything else is ignored (`init` also
    /// appends `attempt=<n>`).
    pub fn from_args(text: &str) -> Config {
        let mut config = Config::default();
        for arg in text.split_whitespace() {
            match arg.split_once('=') {
                Some(("trace", "1")) => config.trace_reports = true,
                Some(("pair", value)) => {
                    config.pair = parse_pair(value);
                    config.le_targets = parse_le_targets(value);
                }
                _ => {}
            }
        }
        config
    }
}

fn parse_pair(value: &str) -> PairPolicy {
    match value {
        "any" => PairPolicy::Any,
        "" | "none" => PairPolicy::Off,
        list => {
            let targets: Vec<BdAddr> = list
                .split(',')
                .filter(|item| !item.starts_with("le:"))
                .filter_map(parse_addr)
                .take(MAX_TARGETS)
                .collect();
            if targets.is_empty() {
                PairPolicy::Off
            } else {
                PairPolicy::Targets(targets)
            }
        }
    }
}

/// The `le:AA:BB:CC:DD:EE:FF` entries of a `pair=` list: LE pairing targets.
fn parse_le_targets(value: &str) -> Vec<BdAddr> {
    value
        .split(',')
        .filter_map(|item| item.strip_prefix("le:"))
        .filter_map(parse_addr)
        .take(MAX_TARGETS)
        .collect()
}

/// `AA:BB:CC:DD:EE:FF` (the printed order).
pub fn parse_addr(text: &str) -> Option<BdAddr> {
    let mut octets = [0u8; 6];
    let mut parts = text.split(':');
    for slot in &mut octets {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then(|| BdAddr::from_msb(octets))
}
