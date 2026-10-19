//! The cycle rule against a model (`ipc::pipe::reach`): random sends of
//! socket ends through each other and receives, every refusal predicted by a
//! reachability search over the queues the model keeps, then everything closed
//! and nothing left in flight. And the walk's bound: a pair that leads to more
//! listeners than a walk expands is refused, not walked for ever.

use alloc::collections::VecDeque;
use alloc::sync::Arc;

use crate::ipc::pipe::SocketPair;
use crate::task::Fd;

use super::*;

const SYS_SOCKET: u64 = 41;
const SYS_BIND: u64 = 49;
const SYS_LISTEN: u64 = 50;
const AF_UNIX: u64 = 1;
const SOCK_STREAM: u64 = 1;
const MSG_DONTWAIT: u64 = 0x40;

/// Stream pairs in the soak: well under `MAX_PIPES` (two pipes each).
const PAIRS: usize = 10;
const STEPS: u32 = 4000;

/// xorshift64*, so a failing seed replays.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % n
    }
}

/// The socket pair behind descriptor `fd` of the current task.
fn pair_of(fd: u64) -> Option<Arc<SocketPair>> {
    // The clone holds a side reference of its own, released when it drops.
    match &task::fd_clone(fd as usize)? {
        Fd::Socket { pair, .. } => Some(Arc::clone(pair)),
        _ => None,
    }
}

/// What the kernel should hold: `queue[pair][side]` lists, oldest first, the
/// pairs whose ends wait to be read on `side` of `pair`.
struct Model {
    queue: [[VecDeque<usize>; 2]; PAIRS],
}

impl Model {
    /// Whether `to` is `from` or reachable from it through what is queued.
    fn reaches(&self, from: usize, to: usize) -> bool {
        let mut seen = [false; PAIRS];
        let mut stack = alloc::vec![from];
        while let Some(pair) = stack.pop() {
            if pair == to {
                return true;
            }
            if core::mem::replace(&mut seen[pair], true) {
                continue;
            }
            for side in &self.queue[pair] {
                stack.extend(side.iter().copied());
            }
        }
        false
    }
}

/// 4000 random steps over ten stream pairs (both ends of each stay in the
/// table): a send of an end of `x` through `y` must be refused exactly when
/// `y` is `x` or reachable from `x`; a receive must deliver an end of the pair
/// the model queued there first. Both cases of issue #865 must happen: a
/// refusal through a longer path, and a send allowed while the carried pair's
/// queue holds descriptors (which the old rule refused). Closing every end at
/// the end frees everything: the graph never held a cycle.
pub fn scmrights_random_graph_soak() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    let mut ends = Vec::new();
    for _ in 0..PAIRS {
        let (a, b) = socketpair()?;
        ends.push([a, b]);
    }
    let mut model = Model {
        queue: core::array::from_fn(|_| [VecDeque::new(), VecDeque::new()]),
    };
    let pairs: Vec<Arc<SocketPair>> = ends
        .iter()
        .map(|end| pair_of(end[0]).ok_or("a pair end"))
        .collect::<Result<_, _>>()?;
    let mut rng = Rng(0x5eed_865);
    let (mut sent, mut refused, mut taken) = (0u32, 0u32, 0u32);
    let (mut long_refusals, mut carrying_sent) = (0u32, 0u32);
    for step in 0..STEPS {
        let y = rng.below(PAIRS);
        let side = rng.below(2);
        if rng.below(10) < 6 {
            let x = rng.below(PAIRS);
            let end = ends[x][rng.below(2)];
            let cycle = model.reaches(x, y);
            let got = send_fds(ends[y][side], b"s", &[end]);
            check!(
                got == if cycle { ETOOMANYREFS } else { 1 },
                "step {step}: an end of {x} through {y} gave {got:#x}, the model says cycle={cycle}"
            );
            if cycle {
                refused += 1;
                long_refusals += u32::from(x != y);
            } else {
                carrying_sent += u32::from(model.queue[x].iter().any(|q| !q.is_empty()));
                model.queue[y][1 - side].push_back(x);
                sent += 1;
            }
        } else if let Some(&want) = model.queue[y][side].front() {
            let mut buf = [0u8; 4];
            // Never blocks: a disagreement fails the test instead of hanging the suite.
            let got = recv_fds(ends[y][side], &mut buf, Some(4), MSG_DONTWAIT);
            check!(
                got.n == 1 && got.fds.len() == 1,
                "step {step}: receive on {y}: n={:#x} fds={}",
                got.n,
                got.fds.len()
            );
            let delivered = pair_of(got.fds[0]);
            close(got.fds[0]);
            check!(
                delivered.is_some_and(|pair| Arc::ptr_eq(&pair, &pairs[want])),
                "step {step}: receive on {y} delivered another pair than {want}"
            );
            model.queue[y][side].pop_front();
            taken += 1;
        }
        let queued: usize = model.queue.iter().flatten().map(VecDeque::len).sum();
        check!(
            pipe::in_flight() == queued,
            "step {step}: {} in flight, the model has {queued}",
            pipe::in_flight()
        );
    }
    check!(
        sent > 500 && refused > 100 && taken > 500 && long_refusals > 20 && carrying_sent > 100,
        "the soak exercised too little: {sent} sent ({carrying_sent} carrying),          {refused} refused ({long_refusals} through a longer path), {taken} taken"
    );
    drop(pairs);
    for pair in ends {
        close(pair[0]);
        close(pair[1]);
    }
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.child);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    task::harness::reset();
    settled(pipes_before)
}

/// A listening socket on an abstract name of its own.
fn listener(id: u32) -> Result<u64, String> {
    let name = alloc::format!("\0scm-graph-{id}");
    let mut sockaddr = [0u8; 110];
    sockaddr[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
    sockaddr[2..2 + name.len()].copy_from_slice(name.as_bytes());
    let len = (2 + name.len()) as u64;
    let fd = sys(SYS_SOCKET, &[AF_UNIX, SOCK_STREAM, 0]);
    check!((fd as i64) >= 0, "listener socket {fd:#x}");
    check!(
        sys(SYS_BIND, &[fd, sockaddr.as_ptr() as u64, len]) == 0,
        "bind {id}"
    );
    check!(sys(SYS_LISTEN, &[fd, 1]) == 0, "listen {id}");
    Ok(fd)
}

/// More listeners in flight behind one pair than a walk expands (1024): an
/// end of that pair is refused through an unrelated pair rather than walked
/// without bound, and goes once a few of them are taken out. Closing the
/// pair frees them all.
pub fn scmrights_walk_bound() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    for id in 0..1030u32 {
        let fd = listener(id)?;
        check!(send_fds(p.a, b"l", &[fd]) == 1, "listener {id} into (a, b)");
        close(fd);
    }
    check!(
        send_fds(p.sa, b"x", &[p.b]) == ETOOMANYREFS,
        "an end of a pair holding 1030 listeners"
    );
    let mut buf = [0u8; 4];
    for _ in 0..10 {
        let got = recv_fds(p.b, &mut buf, Some(4), 0);
        check!(got.fds.len() == 1, "take a listener out: n={:#x}", got.n);
        close(got.fds[0]);
    }
    check!(
        send_fds(p.sa, b"x", &[p.b]) == 1,
        "the same end once 1020 listeners are left"
    );
    let got = recv_fds(p.sb, &mut buf, Some(4), 0);
    check!(got.fds.len() == 1, "receive the end: n={:#x}", got.n);
    close(got.fds[0]);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.child);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    task::harness::reset();
    settled(pipes_before)
}
