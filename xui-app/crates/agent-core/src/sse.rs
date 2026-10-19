//! A byte-level Server-Sent Events parser (docs/ai-agent-plan.md §5.2).
//!
//! Bytes arrive in reads of any size; an event may be split anywhere, even
//! inside a UTF-8 sequence or between the `\r` and `\n` of a line end. The
//! parser is pure: it holds the unfinished line and the unfinished event and
//! hands each complete event to a callback.
//!
//! Following the WHATWG rules: lines end with `\n`, `\r\n` or a bare `\r`;
//! a line starting with `:` is a comment; `field: value` drops one space
//! after the colon; several `data` lines join with `\n`; a blank line
//! dispatches the event. One deviation, on purpose: at end of input an event
//! that has `data` but no closing blank line is dispatched (the stream ended
//! without its final new line) instead of dropped.

use std::fmt;

/// An event, or a single message of the stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

/// The stream broke a limit; nothing after it can be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SseError {
    /// One event grew past [`MAX_EVENT`] bytes.
    EventTooLarge,
}

impl fmt::Display for SseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SseError::EventTooLarge => f.write_str("an event of the stream is over 1 MiB"),
        }
    }
}

/// The most bytes one event (all its lines) may hold.
pub const MAX_EVENT: usize = 1024 * 1024;

#[derive(Default)]
pub struct SseParser {
    /// The unfinished line.
    line: Vec<u8>,
    /// A `\r` ended the last line; a `\n` right after it belongs to it.
    after_cr: bool,
    /// The event being built.
    data: Vec<u8>,
    has_data: bool,
    event: Option<String>,
    id: Option<String>,
    /// Bytes counted against [`MAX_EVENT`] for the event being built.
    size: usize,
    /// The first line has ended (a byte order mark is only dropped there).
    past_first_line: bool,
    failed: bool,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser::default()
    }

    /// Feeds `bytes`; calls `emit` once per completed event. After an error
    /// the parser ignores all further input.
    pub fn feed(&mut self, bytes: &[u8], emit: &mut dyn FnMut(SseEvent)) -> Result<(), SseError> {
        if self.failed {
            return Err(SseError::EventTooLarge);
        }
        for &byte in bytes {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\n' => self.end_line(emit)?,
                b'\r' => {
                    self.after_cr = true;
                    self.end_line(emit)?;
                }
                other => {
                    self.line.push(other);
                    self.size += 1;
                    self.check_size()?;
                }
            }
        }
        Ok(())
    }

    /// The input ended: finishes the last line and event (see the module
    /// note on the unterminated one).
    pub fn finish(&mut self, emit: &mut dyn FnMut(SseEvent)) -> Result<(), SseError> {
        if self.failed {
            return Err(SseError::EventTooLarge);
        }
        if !self.line.is_empty() {
            self.end_line(emit)?;
        }
        self.dispatch(emit);
        Ok(())
    }

    fn check_size(&mut self) -> Result<(), SseError> {
        if self.size > MAX_EVENT {
            self.failed = true;
            self.line = Vec::new();
            self.data = Vec::new();
            return Err(SseError::EventTooLarge);
        }
        Ok(())
    }

    fn end_line(&mut self, emit: &mut dyn FnMut(SseEvent)) -> Result<(), SseError> {
        if self.line.is_empty() {
            self.past_first_line = true;
            self.dispatch(emit);
            return Ok(());
        }
        let mut line = std::mem::take(&mut self.line);
        if !std::mem::replace(&mut self.past_first_line, true) && line.starts_with(b"\xEF\xBB\xBF")
        {
            // A byte order mark at the very start of a stream is not data.
            line.drain(..3);
        }
        if line.first().is_none_or(|&b| b == b':') {
            return Ok(());
        }
        let (name, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => {
                let value = &line[i + 1..];
                (&line[..i], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (&line[..], &line[line.len()..]),
        };
        match name {
            b"data" => {
                if self.has_data {
                    self.data.push(b'\n');
                    self.size += 1;
                }
                self.data.extend_from_slice(value);
                self.has_data = true;
                self.check_size()?;
            }
            b"event" => self.event = Some(String::from_utf8_lossy(value).into_owned()),
            // An id holding NUL is ignored.
            b"id" if !value.contains(&0) => {
                self.id = Some(String::from_utf8_lossy(value).into_owned());
            }
            // `retry` and unknown fields are of no use here.
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, emit: &mut dyn FnMut(SseEvent)) {
        let event = self.event.take();
        let id = self.id.take();
        let had_data = std::mem::take(&mut self.has_data);
        let data = std::mem::take(&mut self.data);
        self.size = 0;
        if had_data {
            emit(SseEvent {
                event,
                id,
                data: String::from_utf8_lossy(&data).into_owned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut parser = SseParser::new();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.feed(chunk, &mut |e| out.push(e)).unwrap();
        }
        parser.finish(&mut |e| out.push(e)).unwrap();
        out
    }

    fn data(events: &[SseEvent]) -> Vec<&str> {
        events.iter().map(|e| e.data.as_str()).collect()
    }

    #[test]
    fn a_data_only_stream_with_done() {
        let events = parse(&[b"data: {\"a\":1}\n\ndata: [DONE]\n\n"]);
        assert_eq!(data(&events), ["{\"a\":1}", "[DONE]"]);
    }

    #[test]
    fn events_split_at_every_byte_are_the_same() {
        let whole: &[u8] = b"data: one\r\n\r\n: comment\r\ndata: two\ndata: three\n\ndata:four\r\r";
        let expect = parse(&[whole]);
        assert_eq!(data(&expect), ["one", "two\nthree", "four"]);
        for cut in 0..=whole.len() {
            assert_eq!(
                parse(&[&whole[..cut], &whole[cut..]]),
                expect,
                "cut at {cut}"
            );
        }
        let bytes: Vec<&[u8]> = whole.chunks(1).collect();
        assert_eq!(parse(&bytes), expect);
    }

    #[test]
    fn a_crlf_split_between_reads_is_one_line_end() {
        let events = parse(&[b"data: a\r", b"\n\r", b"\ndata: b\n\n"]);
        assert_eq!(data(&events), ["a", "b"]);
    }

    #[test]
    fn a_multibyte_character_split_across_reads_survives() {
        let text = "data: héllo → ✓\n\n".as_bytes();
        for cut in 0..text.len() {
            let events = parse(&[&text[..cut], &text[cut..]]);
            assert_eq!(data(&events), ["héllo → ✓"], "cut at {cut}");
        }
    }

    #[test]
    fn comments_unknown_fields_and_empty_events_are_skipped() {
        let events = parse(&[b": keep-alive\n\nretry: 100\nfoo: bar\n\nevent: ping\n\ndata\n\n"]);
        // `data` with no colon is an empty data field: one empty event.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "");
    }

    #[test]
    fn event_and_id_fields_are_kept_and_reset() {
        let events = parse(&[b"event: x\nid: 7\ndata: a\n\ndata: b\n\n"]);
        assert_eq!(events[0].event.as_deref(), Some("x"));
        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[1].event, None);
        assert_eq!(events[1].id, None);
    }

    #[test]
    fn a_missing_final_blank_line_still_dispatches() {
        assert_eq!(data(&parse(&[b"data: last"])), ["last"]);
        assert_eq!(data(&parse(&[b"data: last\n"])), ["last"]);
    }

    #[test]
    fn a_byte_order_mark_is_dropped_once() {
        assert_eq!(data(&parse(&[b"\xEF\xBB\xBFdata: a\n\n"])), ["a"]);
        assert_eq!(data(&parse(&[b"\xEF", b"\xBB\xBFdata: a\n\n"])).len(), 1);
    }

    #[test]
    fn only_one_space_after_the_colon_is_dropped() {
        assert_eq!(data(&parse(&[b"data:  two\n\n"])), [" two"]);
        assert_eq!(data(&parse(&[b"data:none\n\n"])), ["none"]);
    }

    #[test]
    fn an_event_over_the_cap_fails_and_stays_failed() {
        let mut parser = SseParser::new();
        let mut count = 0;
        let big = vec![b'x'; MAX_EVENT + 1];
        let mut line = b"data: ".to_vec();
        line.extend_from_slice(&big);
        assert_eq!(
            parser.feed(&line, &mut |_| count += 1),
            Err(SseError::EventTooLarge)
        );
        assert_eq!(
            parser.feed(b"data: ok\n\n", &mut |_| count += 1),
            Err(SseError::EventTooLarge)
        );
        assert_eq!(count, 0);
        // Many small lines of one event count together.
        let mut parser = SseParser::new();
        let chunk = [b"data: ".as_slice(), &vec![b'y'; 60_000], b"\n"].concat();
        let mut failed = false;
        for _ in 0..40 {
            failed |= parser.feed(&chunk, &mut |_| count += 1).is_err();
        }
        assert!(failed);
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        let events = parse(&[b"data: a\xFFb\n\n"]);
        assert_eq!(events[0].data, "a\u{FFFD}b");
    }
}
