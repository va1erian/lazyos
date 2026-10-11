//! The fuzz entry point shared by the in-tree seeded tests and (when one is
//! added) a cargo-fuzz target: arbitrary text must never panic the parser,
//! the mask or the builders, and whatever a builder writes is one line that
//! parses back into the channel it was built for.

use alloc::string::String;

use crate::line;
use crate::mask::Mask;
use crate::write::{self, LineBuf};
use crate::Channel;

/// Run every property on `data`.
pub fn run(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let _ = line::parse(&text);
    let _ = line::split_complete(&text);
    let _ = Mask::parse(&text, |_| {});

    let channel = Channel::ALL[data.first().copied().unwrap_or(0) as usize % Channel::ALL.len()];
    let mut buf = LineBuf::<128>::new();
    let _ = write::event(
        &mut buf,
        channel,
        "EV",
        &[("k", &text.as_ref()), ("n", &data.len())],
    );
    let cut = buf.truncated();
    let built = core::str::from_utf8(buf.finish()).expect("a built line is UTF-8");
    let body = built
        .strip_suffix('\n')
        .expect("a built line ends in a newline");
    assert!(!body.chars().any(char::is_control), "one line: {built:?}");
    if !cut {
        let parsed = line::parse(body);
        let expected = alloc::format!("{}:EV", channel.prefix());
        assert_eq!(parsed.tag.as_deref(), Some(expected.as_str()));
    }
}
