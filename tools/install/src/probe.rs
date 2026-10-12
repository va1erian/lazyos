//! The parsing half of the Linux probe (`linux.rs`): device numbers and
//! `/proc/self/mountinfo`, portable so they are tested on any host.

/// A block device number.
pub type DevNum = (u32, u32);

/// The number in `dev_t` form as Linux's glibc encodes it.
pub fn split_dev(dev: u64) -> DevNum {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major as u32, minor as u32)
}

/// One key for "the same partition": major and minor packed.
pub fn key(dev: DevNum) -> u64 {
    u64::from(dev.0) << 32 | u64::from(dev.1)
}

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub dev: DevNum,
    pub point: String,
    pub fstype: String,
    pub source: String,
}

/// Parse `/proc/self/mountinfo` (fields: id, parent, `major:minor`, root,
/// mount point, options, optional fields, `-`, type, source, options).
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            let (major, minor) = fields.get(2)?.split_once(':')?;
            let dash = fields.iter().position(|f| *f == "-")?;
            Some(Mount {
                dev: (major.parse().ok()?, minor.parse().ok()?),
                point: unescape(fields.get(4)?),
                fstype: fields.get(dash + 1)?.to_string(),
                source: unescape(fields.get(dash + 2)?),
            })
        })
        .collect()
}

/// Undo mountinfo's octal escapes (`\040` is a space).
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|b| (b'0'..=b'7').contains(b)));
        match (bytes[i], octal) {
            (b'\\', Some(d)) => {
                out.push((d[0] - b'0') << 6 | (d[1] - b'0') << 3 | (d[2] - b'0'));
                i += 4;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
