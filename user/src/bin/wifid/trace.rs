//! `wifid`'s evidence lines, built with `libs/tracefmt` (one line by
//! construction, fields quoted the way `dbgd` parses them) on the `WIFID`
//! channel and written with one `write_str`, so another task's output never
//! lands inside a line. Read on the box with
//! `python tools/dbg/dbgctl.py --host <ip> log --source programs --follow`.

use core::fmt::{self, Display};

use tracefmt::write as line;
use tracefmt::{Channel, LineBuf, LINE_MAX};
use user::sys;

fn emit(build: impl FnOnce(&mut LineBuf<LINE_MAX>) -> fmt::Result) {
    let mut buf = LineBuf::<LINE_MAX>::new();
    let _ = build(&mut buf);
    // The builders write text only; `finish` adds the newline.
    if let Ok(text) = core::str::from_utf8(buf.finish()) {
        sys::write_str(text);
    }
}

/// `WIFID:<name> k=v ...`.
pub(super) fn event(name: &str, fields: &[(&str, &dyn Display)]) {
    emit(|buf| line::event(buf, Channel::Wifid, name, fields));
}

/// `WIFID:<text>`, for a line whose shape is its own (`REGS`).
pub(super) fn text(text: fmt::Arguments<'_>) {
    emit(|buf| line::line(buf, Channel::Wifid, text));
}

/// `WIFID:FAIL:<step> <detail>`: the step that stopped the bring-up.
pub(super) fn fail(step: &str, detail: fmt::Arguments<'_>) {
    emit(|buf| line::line(buf, Channel::Wifid, format_args!("FAIL:{step} {detail}")));
}

/// A register value as `0x` and eight hex digits.
pub(super) struct Hex32(pub u32);

impl Display for Hex32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#010x}", self.0)
    }
}

/// A station address as `aa:bb:cc:dd:ee:ff`.
pub(super) struct Mac(pub [u8; 6]);

impl Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}
