//! The seeded fuzz entry point for the SSE parser and the chunk reader
//! (docs/ai-agent-plan.md §5.2, §10.1).
//!
//! [`run`] is what a cargo-fuzz target calls; the in-tree tests mutate the
//! seeds below with a seeded generator (`FUZZ_CASES`, `FUZZ_SEED` override
//! the run, as in the other fuzzed libraries). The invariants: any bytes in
//! any chunking never panic; the same events come out whatever the chunking;
//! an event never exceeds the cap; and every dispatched `data` payload
//! reaches the chunk reader without a panic.

use crate::provider::ToolCallAssembler;
use crate::sse::{SseError, SseEvent, SseParser, MAX_EVENT};

fn collect(chunks: &[&[u8]]) -> (Vec<SseEvent>, Result<(), SseError>) {
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    let mut result = Ok(());
    for chunk in chunks {
        if let Err(e) = parser.feed(chunk, &mut |e| events.push(e)) {
            result = Err(e);
            break;
        }
    }
    if result.is_ok() {
        result = parser.finish(&mut |e| events.push(e));
    }
    (events, result)
}

/// Checks the parser invariants on `data`; the first byte picks a chunk size.
pub fn run(data: &[u8]) {
    let Some((&size, rest)) = data.split_first() else {
        return;
    };
    let (whole, whole_result) = collect(&[rest]);
    let step = usize::from(size).max(1);
    let pieces: Vec<&[u8]> = rest.chunks(step).collect();
    let (split, split_result) = collect(&pieces);
    let one_by_one: Vec<&[u8]> = rest.chunks(1).collect();
    let (single, single_result) = collect(&one_by_one);
    assert_eq!(whole_result, split_result, "chunking changed the verdict");
    assert_eq!(
        whole_result, single_result,
        "byte-wise feeding changed the verdict"
    );
    if whole_result.is_ok() {
        assert_eq!(whole, split, "chunking changed the events");
        assert_eq!(whole, single, "byte-wise feeding changed the events");
    }
    let mut assembler = ToolCallAssembler::default();
    for event in &whole {
        assert!(event.data.len() <= MAX_EVENT, "an event over the cap");
        crate::provider::fuzz_chunk(&event.data, &mut assembler);
    }
    let _ = assembler.finish();
}

pub const SEEDS: &[&[u8]] = &[
    b"\x20data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n\n",
    b"\x03data: a\r\ndata: b\r\n\r\n: comment\r\nevent: x\r\nid: 1\r\n\r\n",
    b"\x01\xEF\xBB\xBFdata: \xFF\xFE\n\ndata\n\ndata:\n\n",
    b"\x07data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"n\",\"arguments\":\"{\\\"x\"}}]}}]}\n\n",
    b"\x10data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\ndata: {\"error\":{\"message\":\"x\"}}\n\n",
    b"\x02\r\r\n\n\r\rdata:x\r",
];

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            // xorshift64*
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    fn env_number(name: &str, default: u64) -> u64 {
        std::env::var(name)
            .ok()
            .and_then(|v| {
                let v = v.trim();
                match v.strip_prefix("0x") {
                    Some(hex) => u64::from_str_radix(hex, 16).ok(),
                    None => v.parse().ok(),
                }
            })
            .unwrap_or(default)
    }

    fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
        let mut data = base.to_vec();
        for _ in 0..1 + rng.below(6) {
            match rng.below(5) {
                0 if !data.is_empty() => {
                    let i = rng.below(data.len());
                    data[i] = rng.next() as u8;
                }
                1 if !data.is_empty() => {
                    data.remove(rng.below(data.len()));
                }
                2 => {
                    let i = rng.below(data.len() + 1);
                    data.insert(i, b"\r\n:da"[rng.below(5)]);
                }
                3 if !data.is_empty() => {
                    let (a, b) = (rng.below(data.len()), rng.below(data.len()));
                    let piece = data[a.min(b)..a.max(b)].to_vec();
                    let at = rng.below(data.len() + 1);
                    data.splice(at..at, piece);
                }
                _ => data.push(rng.next() as u8),
            }
        }
        data
    }

    #[test]
    fn the_seeds_themselves_hold() {
        for seed in SEEDS {
            run(seed);
        }
    }

    #[test]
    fn seeded_mutations_never_panic_or_depend_on_chunking() {
        let seed = env_number("FUZZ_SEED", 0xa63a_0001);
        let mut rng = Rng(seed);
        for case in 0..env_number("FUZZ_CASES", 3000) {
            let pick = rng.below(SEEDS.len());
            let data = mutate(&mut rng, SEEDS[pick]);
            let outcome = std::panic::catch_unwind(|| run(&data));
            assert!(
                outcome.is_ok(),
                "case {case} failed; replay with FUZZ_SEED=0x{seed:x}"
            );
        }
    }

    #[test]
    fn noise_never_panics() {
        let mut rng = Rng(env_number("FUZZ_SEED", 0xa63a_0001) ^ 0xbeef);
        for _ in 0..env_number("FUZZ_CASES", 3000) / 4 {
            let len = rng.below(400);
            let data: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            run(&data);
        }
    }

    #[test]
    fn a_giant_event_is_refused_in_every_chunking() {
        let mut data = vec![4u8];
        data.extend_from_slice(b"data: ");
        data.extend(std::iter::repeat_n(b'z', MAX_EVENT + 10));
        run(&data);
    }
}
