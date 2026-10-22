//! Reply decoding against arbitrary bytes: nothing the broker (or a hostile
//! peer on a doorbell) sends may panic the client. Seeded and replayable like
//! the repo's other fuzz tests: `FUZZ_CASES` sets the count, `FUZZ_SEED` a
//! seed to replay.

use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use super::fake::{ready, Fake};
use super::{wire, Qos, Subscriber};

/// xorshift64*: enough randomness, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|text| {
            let text = text.trim();
            match text.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => text.parse().ok(),
            }
        })
        .unwrap_or(default)
}

/// A valid reply of each shape, to mutate.
fn seeds() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    let event = topics::NextEventReply {
        event: topics::Event {
            topic: "a/b".into(),
            publisher: 3,
            sequence: 9,
            retained: true,
            payload: vec![1, 2, 3, 4],
        },
    };
    let stats = topics::StatsReply {
        stats: topics::Stats::default(),
    };
    let bodies = [
        topics::encode_next_event_reply(&event).unwrap(),
        topics::encode_stats_reply(&stats).unwrap(),
        topics::encode_subscribe_reply(&topics::SubscribeReply { subscription: 4 }).unwrap(),
        Vec::new(),
    ];
    for body in bodies {
        let parcel = libmessenger::Parcel {
            body,
            ..libmessenger::Parcel::default()
        };
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).unwrap();
        seeds.push(bytes);
    }
    seeds.push(ready(4));
    seeds
}

/// One input: random bytes, or a seed with bytes flipped, cut or extended.
fn input(rng: &mut Rng, seeds: &[Vec<u8>]) -> Vec<u8> {
    if rng.below(4) == 0 {
        return (0..rng.below(300)).map(|_| rng.next() as u8).collect();
    }
    let mut bytes = seeds[rng.below(seeds.len())].clone();
    for _ in 0..1 + rng.below(4) {
        match rng.below(4) {
            0 if !bytes.is_empty() => {
                let at = rng.below(bytes.len());
                bytes[at] ^= 1 << rng.below(8);
            }
            1 if !bytes.is_empty() => {
                let at = rng.below(bytes.len());
                bytes[at] = rng.next() as u8;
            }
            2 => bytes.truncate(rng.below(bytes.len() + 1)),
            _ => bytes.extend((0..rng.below(16)).map(|_| rng.next() as u8)),
        }
    }
    bytes
}

fn exercise(bytes: &[u8]) {
    // The pure decoders.
    if let Ok(parcel) = wire::parse_reply(bytes) {
        let _ = wire::decode_subscription(&parcel);
        let _ = wire::decode_event(&parcel);
        let _ = wire::decode_stats(&parcel);
    }
    let _ = wire::ready_subscription(bytes);

    // The whole client, answered with these bytes by every call and queued on
    // the doorbell.
    let mut client = Subscriber::with_transport(Fake::new());
    let id = client.subscribe("a/#", Qos::Reliable, 4).unwrap();
    let bell = client.attach_bell(id).unwrap();
    client
        .transport
        .bells
        .entry(bell)
        .or_default()
        .push_back(bytes.to_vec());
    client.transport.garbage = Some(bytes.to_vec());
    let drained = client.drain(id, 8);
    assert!(drained.events.len() <= 8);
    let _ = client.stats(id);
    let _ = client.ack(id, 1);
    let _ = client.consume_ready();
    let _ = client.subscribe("x", Qos::Latest, 1);
    // After garbage the client still works against an honest broker.
    client.transport.garbage = None;
    let _ = client.drain(id, 1);
    let _ = client.consume_ready();
}

#[test]
fn garbage_never_panics_the_client() {
    let cases = env_u64("FUZZ_CASES", 3000);
    let seed = env_u64("FUZZ_SEED", 0x5eed_0f_f1ce);
    let mut rng = Rng(seed | 1);
    let seeds = seeds();
    for case in 0..cases {
        let bytes = input(&mut rng, &seeds);
        let outcome = std::panic::catch_unwind(|| exercise(&bytes));
        assert!(
            outcome.is_ok(),
            "case {case} seed {seed:#x} panicked on {bytes:02x?}"
        );
    }
}

#[test]
fn every_prefix_of_a_valid_reply_is_handled() {
    for seed in seeds() {
        for cut in 0..=seed.len() {
            exercise(&seed[..cut]);
        }
    }
}
