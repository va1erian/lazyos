use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Write;

use crate::budget::{Budget, Charge};
use crate::line;
use crate::mask::{Mask, Problem};
use crate::tracer::{Sink, Tracer};
use crate::write::{self, LineBuf};
use crate::Channel;

fn text(build: impl FnOnce(&mut String) -> core::fmt::Result) -> String {
    let mut out = String::new();
    build(&mut out).unwrap();
    out
}

// ---- the catalogue -------------------------------------------------------

#[test]
fn the_catalogue_has_no_duplicate_prefix() {
    let mut seen: Vec<&str> = Vec::new();
    for channel in Channel::ALL {
        let prefix = channel.prefix();
        assert!(!seen.contains(&prefix), "{prefix} appears twice");
        seen.push(prefix);
    }
}

#[test]
fn every_prefix_is_a_marker_and_its_own_name() {
    for channel in Channel::ALL {
        let prefix = channel.prefix();
        assert!(line::is_marker(&format!("{prefix}:UP")), "{prefix}");
        assert!(!prefix.contains(':') && !prefix.is_empty());
        assert_eq!(Channel::from_name(prefix), Some(*channel));
        assert_eq!(
            Channel::from_name(&format!("{}", channel.name())),
            Some(*channel)
        );
    }
}

#[test]
fn discriminants_are_the_mask_bits() {
    for (index, channel) in Channel::ALL.iter().enumerate() {
        assert_eq!(channel.index(), index);
    }
    assert!(Channel::ALL.len() <= crate::channel::MASK_BITS);
}

// ---- line shapes ---------------------------------------------------------

#[test]
fn the_shapes_are_the_wire_formats_in_use() {
    assert_eq!(
        text(|o| write::pass(o, Channel::Writer, "SAVE")),
        "WRITER:SAVE:PASS"
    );
    assert_eq!(
        text(|o| write::pass_with(o, Channel::Pdf, "OPEN", format_args!("{}:{}", "/x.pdf", 3))),
        "PDF:OPEN:PASS:/x.pdf:3"
    );
    assert_eq!(
        text(|o| write::fail(o, Channel::Lrplay, "BIND", format_args!("code {}", 2))),
        "LRPLAY:BIND:FAIL:code 2"
    );
    assert_eq!(
        text(|o| write::warn(o, Channel::Lride, "HOME", format_args!("x"))),
        "LRIDE:HOME:WARN:x"
    );
    assert_eq!(
        text(|o| write::event(
            o,
            Channel::Ui,
            "RECT",
            &[
                ("x", &28),
                ("y", &500),
                ("w", &212),
                ("h", &24),
                ("name", &"start")
            ]
        )),
        "UI:RECT x=28 y=500 w=212 h=24 name=start"
    );
    assert_eq!(
        text(|o| write::line(
            o,
            Channel::Boot,
            format_args!("PHASE:{}:tsc={}", "mem_ready", 99)
        )),
        "BOOT:PHASE:mem_ready:tsc=99"
    );
    assert_eq!(
        text(|o| write::suppressed(o, Channel::Ui, 7)),
        "UI:SUPPRESSED n=7"
    );
}

#[test]
fn a_detail_is_always_one_line() {
    let line = text(|o| write::fail(o, Channel::Lrplay, "LOAD", format_args!("a\nb\r\nc\td")));
    assert_eq!(line, "LRPLAY:LOAD:FAIL:a b  c d");
    let line = text(|o| write::line(o, Channel::Perf, format_args!("x\n{}", "y\u{7}z")));
    assert!(!line.chars().any(char::is_control), "{line:?}");
}

#[test]
fn a_value_with_a_space_or_a_quote_round_trips() {
    let built = text(|o| {
        write::event(
            o,
            Channel::Ui,
            "WIDGET",
            &[
                ("name", &"menu:Text Editor"),
                ("window", &"say \"hi\""),
                ("empty", &""),
                ("n", &-3),
                ("eq", &"a=b"),
            ],
        )
    });
    let parsed = line::parse(&built);
    assert_eq!(parsed.tag.as_deref(), Some("UI:WIDGET"));
    assert_eq!(parsed.field("name"), Some("menu:Text Editor"));
    assert_eq!(parsed.field("window"), Some("say 'hi'"));
    assert_eq!(parsed.field("empty"), Some(""));
    assert_eq!(parsed.field("n"), Some("-3"));
    assert_eq!(parsed.field("eq"), Some("a=b"));
}

#[test]
fn a_built_line_parses_back_into_its_tag_and_fields() {
    for channel in Channel::ALL {
        let built = text(|o| write::event(o, *channel, "EV", &[("k", &"v w"), ("n", &7u8)]));
        let parsed = line::parse(&built);
        assert_eq!(parsed.tag, Some(format!("{}:EV", channel.prefix())));
        assert_eq!(parsed.field("k"), Some("v w"));
        assert_eq!(parsed.field("n"), Some("7"));
        let built = text(|o| write::pass(o, *channel, "UP"));
        assert_eq!(
            line::parse(&built).tag,
            Some(format!("{}:UP:PASS", channel.prefix()))
        );
    }
}

#[test]
fn a_full_buffer_cuts_the_line_and_keeps_the_newline() {
    let mut buf = LineBuf::<16>::new();
    write!(buf, "{}", "x".repeat(100)).unwrap();
    assert!(buf.truncated());
    let line = buf.finish();
    assert_eq!(line.len(), 16);
    assert_eq!(line.last(), Some(&b'\n'));
    // A cut never splits a character.
    let mut buf = LineBuf::<8>::new();
    write!(buf, "aaaaaaé").unwrap();
    assert!(core::str::from_utf8(buf.finish()).is_ok());
}

#[test]
fn a_cut_line_ends_in_dots_and_a_whole_one_does_not() {
    let mut buf = LineBuf::<16>::new();
    write!(buf, "{}", "x".repeat(100)).unwrap();
    assert_eq!(buf.finish(), b"xxxxxxxxxxxx...\n");
    // The dots land on a character boundary.
    let mut buf = LineBuf::<12>::new();
    write!(buf, "abcdefgéééé").unwrap();
    let line = buf.finish();
    assert!(core::str::from_utf8(line).is_ok());
    assert!(line.ends_with(b"...\n"));
    let mut buf = LineBuf::<16>::new();
    write!(buf, "short").unwrap();
    assert_eq!(buf.finish(), b"short\n");
}

// ---- the parser (moved from dbgwire::logline) -----------------------------

#[test]
fn log_lines_become_records() {
    let l = line::parse(
        "[12.345] USBD:DIAG port=1 timeout slot=Enabled/addr1 ep0dq=0x1f000 note=\"two words\" tail",
    );
    assert_eq!(l.stamp_ms, Some(12_345));
    assert_eq!(l.tag.as_deref(), Some("USBD:DIAG"));
    assert_eq!(l.field("port"), Some("1"));
    assert_eq!(l.field("slot"), Some("Enabled/addr1"));
    assert_eq!(l.field("ep0dq"), Some("0x1f000"));
    assert_eq!(l.field("note"), Some("two words"));
    assert_eq!(l.field("tail"), None);

    let plain = line::parse("kernel: mounted root");
    assert_eq!(plain.tag, None);
    assert!(plain.fields.is_empty());
    assert_eq!(plain.text, "kernel: mounted root");

    // `key=value` text that is not behind a marker is not split.
    assert!(line::parse("a=1 b=2").fields.is_empty());
    let hw = line::parse("HW:IRQCHIP:ioapic pins=24 dest=0");
    assert_eq!(hw.tag.as_deref(), Some("HW:IRQCHIP:ioapic"));
    assert_eq!(hw.field("pins"), Some("24"));
    // Unterminated quotes and empty keys cannot panic or loop.
    let odd = line::parse("X:Y =1 k=\"open");
    assert_eq!(odd.field("k"), Some("open"));
}

#[test]
fn followers_hold_back_partial_lines() {
    let (lines, rest) = line::split_complete("a\nb\nc");
    assert_eq!(lines, ["a", "b"]);
    assert_eq!(rest, "c");
    let (lines, rest) = line::split_complete("a\n");
    assert_eq!(lines, ["a"]);
    assert_eq!(rest, "");
    let (lines, rest) = line::split_complete("abc");
    assert!(lines.is_empty());
    assert_eq!(rest, "abc");
}

// ---- the mask ------------------------------------------------------------

fn mask_of(text: &str) -> (Mask, Vec<String>) {
    let mut problems = Vec::new();
    let mask = Mask::parse(text, |p| problems.push(format!("{p:?}")));
    (mask, problems)
}

#[test]
fn a_list_names_exactly_its_channels() {
    let (mask, problems) = mask_of("init,pkgd");
    assert!(problems.is_empty());
    assert!(mask.allows(Channel::Init) && mask.allows(Channel::Pkgd));
    assert!(!mask.allows(Channel::Shell) && !mask.allows(Channel::Ui));
}

#[test]
fn a_removal_list_starts_from_the_defaults() {
    let (mask, _) = mask_of("-shell");
    assert!(mask.allows(Channel::Init) && !mask.allows(Channel::Shell));
    assert!(!mask.allows(Channel::Label), "opt-in channels stay off");
    let (mask, _) = mask_of("all,-shell");
    assert!(mask.allows(Channel::Label) && !mask.allows(Channel::Shell));
    let (mask, _) = mask_of("default,label");
    assert!(mask.allows(Channel::Label) && mask.allows(Channel::Init));
}

#[test]
fn all_and_none() {
    let (all, _) = mask_of("all");
    let (none, _) = mask_of("none");
    for channel in Channel::ALL {
        assert!(all.allows(*channel));
        assert!(!none.allows(*channel));
    }
    assert_eq!(Mask::all(), all);
    assert_eq!(Mask::NONE, none);
}

#[test]
fn bad_tokens_are_reported_and_skipped() {
    let (mask, problems) = mask_of("init, nosuch ,,PKGD # comment");
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(mask.allows(Channel::Init) && mask.allows(Channel::Pkgd));
    assert_eq!(mask_of("zz").1, [format!("{:?}", Problem::Unknown("zz"))]);
    // Hostile text never panics.
    for hostile in ["", ",", "-", "--init", "\u{0}", &"a,".repeat(500)] {
        let _ = mask_of(hostile);
    }
}

// ---- the budget ----------------------------------------------------------

#[test]
fn a_budget_allows_its_lines_then_one_notice_per_window() {
    let budget = Budget::new(3, 100);
    assert_eq!(budget.charge(0), Charge::Allow);
    assert_eq!(budget.charge(1), Charge::Allow);
    assert_eq!(budget.charge(2), Charge::Allow);
    for _ in 0..10 {
        assert_eq!(budget.charge(50), Charge::Deny);
    }
    // The window rolls: one notice with the count, then lines again.
    assert_eq!(budget.charge(100), Charge::AllowAfter(10));
    assert_eq!(budget.charge(101), Charge::Allow);
    assert_eq!(budget.charge(102), Charge::Allow);
    assert_eq!(budget.charge(103), Charge::Deny);
    // The denied line was counted: the next window opens with a notice.
    assert_eq!(budget.charge(205), Charge::AllowAfter(1));
    // A quiet window prints no notice.
    assert_eq!(budget.charge(400), Charge::Allow);
}

// ---- the tracer ----------------------------------------------------------

#[derive(Default)]
struct Capture(RefCell<Vec<String>>);

impl Sink for Capture {
    fn write_line(&self, line: &[u8]) {
        self.0
            .borrow_mut()
            .push(String::from_utf8(line.to_vec()).unwrap());
    }
}

#[test]
fn a_masked_channel_writes_nothing() {
    let tracer = Tracer::new();
    let sink = Capture::default();
    tracer.set_mask(Mask::parse("init", |_| {}));
    assert!(tracer.emit(&sink, 0, Channel::Init, |b| {
        let _ = write::pass(b, Channel::Init, "UP");
    }));
    let wrote = tracer.emit(&sink, 0, Channel::Shell, |_| panic!("built a masked line"));
    assert!(!wrote);
    assert_eq!(*sink.0.borrow(), ["INIT:UP:PASS\n"]);
}

#[test]
fn a_new_tracer_has_the_default_mask() {
    let tracer = Tracer::new();
    assert_eq!(tracer.mask(), Mask::defaults());
    assert!(tracer.enabled(Channel::Init));
    assert!(!tracer.enabled(Channel::Label));
}

#[test]
fn a_channel_over_budget_prints_one_suppressed_line_per_window() {
    let tracer = Tracer::new();
    let sink = Capture::default();
    tracer.set_mask(Mask::all());
    let rate = Channel::Label.rate().unwrap();
    let emit = |now: u64| {
        tracer.emit(&sink, now, Channel::Label, |b| {
            let _ = write::line(b, Channel::Label, format_args!("DENY"));
        })
    };
    for _ in 0..rate.lines + 25 {
        emit(1);
    }
    assert_eq!(sink.0.borrow().len(), rate.lines as usize);
    emit(1 + rate.window);
    let lines = sink.0.borrow();
    assert_eq!(lines.len(), rate.lines as usize + 2);
    assert_eq!(lines[rate.lines as usize], "LABEL:SUPPRESSED n=25\n");
    assert_eq!(lines[rate.lines as usize + 1], "LABEL:DENY\n");
}

#[test]
fn the_ui_channel_is_not_rate_limited_for_its_probe_evidence() {
    // `UI:RECT` lines (a menu opens fifty rows at once; a session clicks them
    // by name) share the channel with `UI:STALL`, which has a budget of its
    // own: a channel-wide limit would drop probe lines and break a session.
    assert_eq!(Channel::Ui.rate(), None);
    let tracer = Tracer::new();
    let sink = Capture::default();
    for _ in 0..500 {
        tracer.emit(&sink, 1, Channel::Ui, |b| {
            let _ = write::line(b, Channel::Ui, format_args!("RECT x=0 y=0 w=1 h=1 name=a"));
        });
    }
    assert_eq!(sink.0.borrow().len(), 500);
}

#[test]
fn emit_always_ignores_the_mask() {
    let tracer = Tracer::new();
    let sink = Capture::default();
    tracer.set_mask(Mask::NONE);
    tracer.emit_always(&sink, |b| {
        let _ = write!(b, "TEST:x:PASS");
    });
    assert_eq!(*sink.0.borrow(), ["TEST:x:PASS\n"]);
}

#[test]
fn random_text_never_panics_and_builds_one_line() {
    fuzzkit::for_seeds("tracefmt_random", |_, rng| {
        let len = rng.below(300) as usize;
        let bytes = rng.bytes(len);
        crate::fuzz::run(&bytes);
    });
}
