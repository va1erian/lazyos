extern crate std;

use std::string::String;
use std::vec;
use std::vec::Vec;

use super::*;

fn parse_all(chunks: &[&[u8]]) -> (Vec<Reply>, Result<(), ReplyError>) {
    let mut parser = ReplyParser::new();
    let mut out = Vec::new();
    let mut result = Ok(());
    for chunk in chunks {
        if let Err(e) = parser.feed(chunk) {
            result = Err(e);
        }
        while let Some(reply) = parser.next() {
            out.push(reply);
        }
    }
    (out, result)
}

fn one(text: &str) -> Reply {
    let (mut replies, result) = parse_all(&[text.as_bytes()]);
    assert_eq!(result, Ok(()), "{text:?}");
    assert_eq!(replies.len(), 1, "{text:?}");
    replies.remove(0)
}

#[test]
fn a_one_line_reply() {
    let r = one("220 Service ready\r\n");
    assert_eq!((r.code, r.text(), r.class()), (220, "Service ready", 2));
    assert_eq!(r.lines.len(), 1);
}

#[test]
fn a_multi_line_reply_is_returned_whole() {
    let r = one("211-Features:\r\n SIZE\r\n EPSV\r\n211 End\r\n");
    assert_eq!(r.code, 211);
    assert_eq!(r.lines, ["Features:", " SIZE", " EPSV", "End"]);
}

#[test]
fn a_continuation_line_may_start_with_digits_of_its_own() {
    // Only the reply's own code followed by a space ends it; shorter digit
    // runs and indented numbers are text.
    let r = one("214-Help
99 not a code
 214 indented
214 done
");
    assert_eq!(r.code, 214);
    assert_eq!(r.lines.len(), 4);
}

#[test]
fn a_different_code_inside_a_multi_line_reply_is_an_error() {
    let (_, result) = parse_all(&[b"214-Help
 999 x
214 y
"]);
    assert_eq!(result, Ok(()), "text with a leading space is text");
    let (_, result) = parse_all(&[b"214-Help
999 x
214 y
"]);
    assert_eq!(
        result,
        Err(ReplyError::CodeChanged),
        "another code, even mid-reply"
    );
    let (_, result) = parse_all(&[b"214-Help
250 x
"]);
    assert_eq!(result, Err(ReplyError::CodeChanged));
}

#[test]
fn bare_lf_and_crlf_both_end_a_line() {
    assert_eq!(one("200 ok\n").code, 200);
    assert_eq!(one("200 ok\r\n").code, 200);
}

#[test]
fn a_bare_code_is_a_reply_without_text() {
    let r = one("250\r\n");
    assert_eq!((r.code, r.text()), (250, ""));
}

#[test]
fn replies_split_at_every_byte_parse_the_same() {
    let text = b"220 hi\r\n211-a\r\n b\r\n211 c\r\n150 go\r\n";
    let whole = parse_all(&[text]).0;
    let bytes: Vec<&[u8]> = text.chunks(1).collect();
    assert_eq!(parse_all(&bytes).0, whole);
    assert_eq!(whole.len(), 3);
}

#[test]
fn several_replies_in_one_chunk() {
    let (replies, result) = parse_all(&[b"150 a\r\n226 b\r\n"]);
    assert_eq!(result, Ok(()));
    assert_eq!(
        replies.iter().map(|r| r.code).collect::<Vec<_>>(),
        [150, 226]
    );
}

#[test]
fn malformed_first_lines_are_refused() {
    for bad in [
        &b"hello\r\n"[..],
        b"22 x\r\n",
        b"2200 x\r\n",
        b"220x\r\n",
        b" 220 x\r\n",
        b"\r\n",
        b"-220 x\r\n",
    ] {
        let (replies, result) = parse_all(&[bad]);
        assert!(replies.is_empty(), "{bad:?}");
        assert_eq!(result, Err(ReplyError::BadLine), "{bad:?}");
    }
    for out_of_range in [&b"099 x\r\n"[..], b"600 x\r\n", b"000 x\r\n", b"999 x\r\n"] {
        assert_eq!(parse_all(&[out_of_range]).1, Err(ReplyError::BadCode));
    }
}

#[test]
fn a_bare_cr_inside_a_line_is_refused() {
    assert_eq!(parse_all(&[b"220 a\rb\r\n"]).1, Err(ReplyError::BadLine));
}

#[test]
fn a_line_over_the_limit_is_refused() {
    let mut line = b"220 ".to_vec();
    line.extend(core::iter::repeat_n(b'a', MAX_LINE));
    line.extend_from_slice(b"\r\n");
    assert_eq!(parse_all(&[&line]).1, Err(ReplyError::LineTooLong));
    // Exactly at the limit is fine.
    let mut ok = b"220 ".to_vec();
    ok.extend(core::iter::repeat_n(b'a', MAX_LINE - 4));
    ok.extend_from_slice(b"\r\n");
    assert_eq!(parse_all(&[&ok]).1, Ok(()));
}

#[test]
fn a_reply_with_too_many_lines_is_refused() {
    let mut text = String::from("214-start\r\n");
    for _ in 0..MAX_LINES + 1 {
        text.push_str(" more\r\n");
    }
    assert_eq!(
        parse_all(&[text.as_bytes()]).1,
        Err(ReplyError::TooManyLines)
    );
}

#[test]
fn the_parser_buffers_a_bounded_amount_whatever_arrives() {
    let mut parser = ReplyParser::new();
    let junk = vec![b'x'; 100_000];
    let _ = parser.feed(&junk);
    assert!(parser.pending() <= MAX_LINE);
    // And it stays failed.
    assert!(parser.feed(b"220 ok\r\n").is_err());
    assert!(parser.next().is_none());
}

#[test]
fn control_characters_in_text_are_neutralised() {
    let r = one("200 a\x1b[31mb\x07\r\n");
    assert!(
        r.text().bytes().all(|b| (0x20..0x7F).contains(&b)),
        "{:?}",
        r.text()
    );
}

#[test]
fn pasv_parses_the_address_and_port() {
    let text = "Entering Passive Mode (10,0,2,2,186,95).";
    assert_eq!(parse_pasv(text), Some(([10, 0, 2, 2], 186 * 256 + 95)));
    assert_eq!(parse_pasv("(1,2,3,4,0,21)"), Some(([1, 2, 3, 4], 21)));
}

#[test]
fn pasv_refuses_anything_else() {
    for bad in [
        "",
        "no parens",
        "(1,2,3,4,5)",
        "(1,2,3,4,5,6,7)",
        "(1,2,3,4,5,256)",
        "(1,2,3,4,5,-1)",
        "(1,2,3,4,5,+6)",
        "(1, 2,3,4,5,6)",
        "(1,2,3,4,,6)",
        "(a,b,c,d,e,f)",
        "(1,2,3,4,0,0)",
        "(1,2,3,4,5,0006)",
        "(1,2,3,4,5,6",
        ")1,2,3,4,5,6(",
    ] {
        assert_eq!(parse_pasv(bad), None, "{bad:?}");
    }
}

#[test]
fn epsv_parses_the_port() {
    assert_eq!(
        parse_epsv("Entering Extended Passive Mode (|||6446|)"),
        Some(6446)
    );
    assert_eq!(parse_epsv("(!!!4000!)"), Some(4000));
}

#[test]
fn epsv_refuses_anything_else() {
    for bad in [
        "",
        "(|||0|)",
        "(|||65536|)",
        "(|||-1|)",
        "(|||12a|)",
        "(||1|5|)",
        "(|1.2.3.4|5|)",
        "(|||5)",
        "(a||a5a)",
        "(||||5|)",
        "(|||123456|)",
    ] {
        assert_eq!(parse_epsv(bad), None, "{bad:?}");
    }
}

#[test]
fn commands_are_built_with_crlf() {
    assert_eq!(command("USER", Some("lazy")).unwrap(), b"USER lazy\r\n");
    assert_eq!(command("PWD", None).unwrap(), b"PWD\r\n");
    assert_eq!(
        command("RETR", Some("a b.txt")).unwrap(),
        b"RETR a b.txt\r\n"
    );
}

#[test]
fn an_argument_cannot_carry_a_second_command() {
    for bad in ["x\r\nDELE y", "x\nQUIT", "x\rQUIT", "x\0y"] {
        assert_eq!(
            command("RETR", Some(bad)),
            Err(CommandError::BadCharacter),
            "{bad:?}"
        );
    }
    assert_eq!(
        command("RETR", Some(&"a".repeat(MAX_ARG + 1))),
        Err(CommandError::TooLong)
    );
    assert!(command("RETR", Some(&"a".repeat(MAX_ARG))).is_ok());
    for verb in ["", "AB", "ABCDE", "user", "US ER", "US\r"] {
        assert_eq!(command(verb, None), Err(CommandError::BadVerb), "{verb:?}");
    }
}

#[test]
fn crc32_matches_the_standard_check_values() {
    assert_eq!(crc32(b""), 0);
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(
        crc32(b"The quick brown fox jumps over the lazy dog"),
        0x414F_A339
    );
}

#[test]
fn crc32_is_incremental() {
    let data: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
    let whole = crc32(&data);
    for cut in [0, 1, 17, 2500, 4999, 5000] {
        let (a, b) = data.split_at(cut);
        assert_eq!(crc32_update(crc32(a), b), whole, "cut {cut}");
    }
}

#[test]
fn the_pattern_matches_the_harnesss_python_generator() {
    // tools/net/hostpeers.py `xorshift_pattern(16)`: the host regenerates the
    // stream `put -g` sends, so the two must agree byte for byte.
    let mut first = [0u8; 16];
    Pattern::new().fill(&mut first);
    assert_eq!(
        first,
        [11, 2, 229, 54, 161, 78, 214, 26, 176, 73, 184, 86, 173, 214, 63, 252]
    );
}

#[test]
fn the_pattern_is_deterministic_and_resumable() {
    let mut a = vec![0u8; 3000];
    Pattern::new().fill(&mut a);
    let mut p = Pattern::new();
    let (mut x, mut y) = (vec![0u8; 1234], vec![0u8; 1766]);
    p.fill(&mut x);
    p.fill(&mut y);
    x.extend(y);
    assert_eq!(a, x);
    assert!(a.iter().collect::<std::collections::BTreeSet<_>>().len() > 200);
}

/// Hostile and random input never panics, never buffers without bound, and
/// parses identically however it is chunked.
#[test]
fn random_streams_parse_the_same_in_any_chunking() {
    use fuzzkit::for_seeds;
    const TOKENS: &[&[u8]] = &[
        b"220 ",
        b"211-",
        b"211 ",
        b"150",
        b"-",
        b" ",
        b"\r\n",
        b"\n",
        b"\r",
        b"abc",
        b"12",
        b"999 ",
        b"(1,2,3,4,5,6)",
        b"\x00",
        b"\xff",
    ];
    for_seeds("ftpwire::random_streams", |_, rng| {
        let mut stream = Vec::new();
        for _ in 0..rng.range(1, 60) {
            stream.extend_from_slice(rng.pick(TOKENS));
        }
        let whole = parse_all(&[&stream]);
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut at = 0;
        while at < stream.len() {
            let n = rng.range(1, 9) as usize;
            let end = (at + n).min(stream.len());
            chunks.push(&stream[at..end]);
            at = end;
        }
        assert_eq!(parse_all(&chunks), whole, "{stream:?}");
        let mut parser = ReplyParser::new();
        let _ = parser.feed(&stream);
        assert!(parser.pending() <= MAX_LINE);
    });
}

#[test]
fn random_text_never_panics_the_address_parsers() {
    use fuzzkit::for_seeds;
    for_seeds("ftpwire::random_addresses", |_, rng| {
        let len = rng.range(0, 40) as usize;
        let alphabet = b"0123456789,()|! -+a\r\n";
        let text: String = (0..len).map(|_| char::from(*rng.pick(alphabet))).collect();
        let _ = parse_pasv(&text);
        let _ = parse_epsv(&text);
    });
}

mod listing {
    use super::super::listing::*;

    #[test]
    fn mlsd_lines() {
        let e = parse_mlsd_line("type=file;size=1234;modify=20260101120000; notes v2.txt").unwrap();
        assert_eq!(
            (e.name.as_str(), e.dir, e.size),
            ("notes v2.txt", false, 1234)
        );
        assert_eq!(e.mtime, Some(1_767_268_800));
        let d = parse_mlsd_line("Type=dir;Modify=19700101000000.123; sub").unwrap();
        assert!(d.dir && d.mtime == Some(0));
        for skipped in [
            "type=cdir; .",
            "type=pdir; ..",
            "type=OS.unix=symlink; l",
            "size=1; nokind",
            "type=file;size=x; bad",
            "type=file; a/b",
            "type=file; ",
            "type=file;size=1;nospace",
        ] {
            assert_eq!(parse_mlsd_line(skipped), None, "{skipped:?}");
        }
    }

    #[test]
    fn list_lines() {
        let e = parse_list_line("-rw-r--r--    1 lazy lazy     4096 Jan  1 00:00 a file").unwrap();
        assert_eq!((e.name.as_str(), e.dir, e.size), ("a file", false, 4096));
        let d = parse_list_line("drwxr-xr-x 2 0 0 0 Oct 04  2026 pub").unwrap();
        assert!(d.dir && d.name == "pub");
        for skipped in [
            "lrwxrwxrwx 1 a a 3 Jan 1 00:00 l -> x",
            "total 12",
            "-rw-r--r-- 1 a a big Jan 1 00:00 x",
            "-rw-r--r-- 1 a a 3 Jan 1 00:00",
            "drwxr-xr-x 2 a a 0 Jan 1 00:00 ..",
            "",
        ] {
            assert_eq!(parse_list_line(skipped), None, "{skipped:?}");
        }
    }

    #[test]
    fn whole_listings() {
        let body = b"type=dir; d\r\ntype=file;size=3; f\r\ngarbage\r\ntype=file; bad\x01name\r\n";
        let entries = parse_listing(body, true);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].size, 3);
        let list = b"-rw-r--r-- 1 a a 5 Jan 1 00:00 x\n\xff\xfe\n";
        assert_eq!(parse_listing(list, false).len(), 1);
        assert_eq!(parse_modify("20261332000000"), None);
        assert_eq!(parse_modify("2026"), None);
    }
}
