//! Pseudo-terminal flow control: a master write takes only what the slave's
//! input queue has room for (a short count, `EAGAIN`, a blocked writer woken
//! by a read), `POLLOUT` on the master follows that room, and nothing typed is
//! lost, in raw and canonical mode.

use super::termios::{openpty, read, tcgets, tcsets, write};
use super::*;
use crate::ipc::pipe::POLLOUT;
use crate::tty::ldisc::MAX_INPUT;
use crate::tty::pty::{self, Pty};
use crate::tty::{termios as t, Termios};
use alloc::sync::Arc;

/// Raw input: no line editing, echo, signals or CR mapping.
fn raw(mut termios: Termios) -> Termios {
    termios.lflag &= !(t::ICANON | t::ECHO | t::ISIG | t::IEXTEN);
    termios.iflag &= !(t::ICRNL | t::INLCR | t::IGNCR | t::IXON);
    termios
}

/// A pair with a non-blocking master and slave, the slave set by `mode`.
fn flow_pair(mode: impl FnOnce(Termios) -> Termios) -> Result<(u64, u64), String> {
    let (master, slave) = openpty()?;
    let termios = mode(tcgets(slave)?);
    check!(tcsets(slave, termios) == 0, "TCSETS");
    check!(
        task::fd_set_status(master as usize, true),
        "master O_NONBLOCK"
    );
    check!(
        task::fd_set_status(slave as usize, true),
        "slave O_NONBLOCK"
    );
    Ok((master, slave))
}

fn writable(fd: u64) -> bool {
    task::fd_poll(fd as usize, POLLOUT).unwrap_or(0) & POLLOUT != 0
}

/// Close a pair and check nothing outlives it.
fn close_pair(master: u64, slave: u64) -> Result<(), String> {
    sys(3, &[slave]);
    sys(3, &[master]);
    check!(Pty::live() == 0, "{} ptys leaked", Pty::live());
    for fd in 3..task::harness::fd_table_len() {
        check!(task::fd_kind(fd) == task::FdKind::Closed, "fd {fd} leaked");
    }
    Ok(())
}

/// Raw mode: a full queue gives a short count, then `EAGAIN` and no
/// `POLLOUT`; a read brings both back, and every byte arrives in order.
pub fn pty_flow_raw() -> Result<(), String> {
    fresh()?;
    let (master, slave) = flow_pair(raw)?;
    let data: Vec<u8> = (0..MAX_INPUT + 100).map(|i| (i % 251) as u8).collect();
    check!(writable(master), "an empty queue is not writable");
    check!(write(master, &data[..4000]) == 4000, "first write");
    let n = write(master, &data[4000..]);
    check!(n == (MAX_INPUT - 4000) as u64, "short write took {n:#x}");
    check!(
        write(master, &data[MAX_INPUT..]) == EAGAIN,
        "a full queue took more"
    );
    check!(!writable(master), "a full queue reports POLLOUT");
    let mut got = vec![0u8; 10];
    check!(read(slave, &mut got) == 10 && got == data[..10], "drain 10");
    check!(writable(master), "a drained queue reports no POLLOUT");
    let n = write(master, &data[MAX_INPUT..]);
    check!(n == 10, "the room a read made took {n:#x}");
    let mut rest = vec![0u8; MAX_INPUT + 64];
    let n = read(slave, &mut rest) as usize;
    check!(
        n == MAX_INPUT && rest[..n] == data[10..MAX_INPUT + 10],
        "drained {n} bytes, in order: {}",
        rest[..n] == data[10..MAX_INPUT + 10]
    );
    // With ISIG, ^C still gets through a full queue and flushes it.
    let mut signals = raw(tcgets(slave)?);
    signals.lflag |= t::ISIG;
    check!(tcsets(slave, signals) == 0, "TCSETS ISIG");
    let fill = [b'f'; MAX_INPUT];
    check!(write(master, &fill) == MAX_INPUT as u64, "refill");
    check!(write(master, b"x") == EAGAIN, "a refilled queue took more");
    check!(write(master, b"\x03") == 1, "^C on a full queue");
    check!(writable(master), "^C did not make room");
    check!(
        read(slave, &mut rest) == EAGAIN,
        "^C did not flush the input"
    );
    close_pair(master, slave)
}

/// Canonical mode: complete lines hold the writer back, but the line being
/// edited can always be ended, `^U` and `^C` always pass, and an overlong
/// line with nothing to read drops its excess instead of blocking.
pub fn pty_flow_canonical() -> Result<(), String> {
    fresh()?;
    let (master, slave) = flow_pair(|mut termios| {
        termios.lflag &= !t::ECHO;
        termios
    })?;
    let mut buf = vec![0u8; MAX_INPUT + 64];
    // An overlong line: nothing could drain it, so it never blocks.
    let long = vec![b'x'; MAX_INPUT];
    check!(
        write(master, &long) == MAX_INPUT as u64,
        "an overlong line blocked"
    );
    check!(writable(master), "an unended line reports no POLLOUT");
    check!(write(master, b"\n") == 1, "the overlong line could not end");
    let n = read(slave, &mut buf) as usize;
    check!(
        n == MAX_INPUT && buf[..n - 1].iter().all(|&b| b == b'x') && buf[n - 1] == b'\n',
        "overlong line read {n}"
    );
    // 63 complete lines of 64 bytes, then the 64th line's text.
    let line = |i: usize| {
        let mut l = vec![b'a' + (i % 26) as u8; 63];
        l.push(b'\n');
        l
    };
    for i in 0..63 {
        check!(write(master, &line(i)) == 64, "line {i}");
    }
    let mut last = line(63);
    last.insert(63, b'q');
    let n = write(master, &last);
    check!(n == 63, "the last line's text took {n:#x}");
    check!(
        write(master, b"q\n") == EAGAIN,
        "a full edit line took a char"
    );
    check!(!writable(master), "a full queue reports POLLOUT");
    check!(write(master, b"\n") == 1, "the line could not be ended");
    check!(
        write(master, b"\n") == EAGAIN,
        "a full queue took a line end"
    );
    check!(write(master, b"\x15") == 1, "^U on a full queue");
    // A read makes room for exactly the next line.
    for i in 0..64 {
        let n = read(slave, &mut buf) as usize;
        check!(buf[..n] == line(i)[..], "line {i} read {n}");
        if i == 0 {
            check!(writable(master), "a read line gave no POLLOUT");
            check!(write(master, b"partial") == 7, "type after a read");
            check!(write(master, b"\x15ok\n") == 4, "^U and a new line");
        }
    }
    let n = read(slave, &mut buf) as usize;
    check!(&buf[..n] == b"ok\n", "the line after ^U: {:?}", &buf[..n]);
    check!(read(slave, &mut buf) == EAGAIN, "stray input");
    // ^C passes a full queue and flushes it.
    for i in 0..64 {
        check!(write(master, &line(i)) == 64, "refill line {i}");
    }
    check!(write(master, b"z") == EAGAIN, "a full queue took a char");
    check!(write(master, b"\x03") == 1, "^C on a full queue");
    check!(read(slave, &mut buf) == EAGAIN, "^C did not flush");
    close_pair(master, slave)
}

/// A pair made directly, both sides held, the slave raw or canonical.
fn direct_pair(raw_mode: bool) -> Result<Arc<Pty>, String> {
    let pty = Pty::open_master((0, 0)).ok_or("open_master")?;
    pty.acquire(true);
    pty.set_locked(false);
    pty.acquire(false);
    if raw_mode {
        pty.with_ldisc(|l| l.set_termios(raw(l.termios)));
    }
    Ok(pty)
}

/// Park the current task as a blocked master writer, run `drain`, and check
/// the writer was woken (`Woken`) by it.
fn woken_by(pty: &Pty, what: &str, drain: impl FnOnce()) -> Result<(), String> {
    let me = task::current();
    check!(
        pty.master_write(b"x", true) == Err(pty::Error::WouldBlock),
        "{what}: the queue was not full"
    );
    pty.park_master_writer(me);
    check!(
        matches!(
            task::harness::state(me),
            Some(task::TaskState::Blocked { .. })
        ),
        "{what}: the writer did not block"
    );
    drain();
    check!(
        task::harness::state(me) == Some(task::TaskState::Runnable),
        "{what}: the writer stayed blocked: {:?}",
        task::harness::state(me)
    );
    check!(
        task::harness::take_wake_reason(me) == Some(task::WakeReason::Woken),
        "{what}: wake reason is not Woken"
    );
    check!(
        pty.master_write(b"x", true) == Ok(1),
        "{what}: the woken writer found no room"
    );
    Ok(())
}

/// A writer blocked on a full queue is woken by a slave read (raw and
/// canonical), by a flush, and by the slave's hang-up, which then ends its
/// write with `EIO` (`HungUp`) instead of a wait that never ends.
pub fn pty_flow_blocked_writer_woken() -> Result<(), String> {
    fresh()?;
    let mut one = [0u8; 1];
    let pty = direct_pair(true)?;
    let fill = vec![b'r'; MAX_INPUT];
    check!(pty.master_write(&fill, true) == Ok(MAX_INPUT), "raw fill");
    woken_by(&pty, "raw read", || {
        let _ = pty.slave_read(&mut one, true);
    })?;
    woken_by(&pty, "flush", || pty.with_ldisc(|l| l.flush_input()))?;
    check!(pty.master_write(&fill, true).is_ok(), "refill");
    pty.release(false);
    check!(
        pty.master_write(b"x", false) == Err(pty::Error::HungUp),
        "a full write to a hung-up slave did not fail"
    );
    check!(
        pty.poll_gen(true, POLLOUT).0 & POLLOUT != 0,
        "a hung-up slave hides POLLOUT from its writer"
    );
    pty.release(true);
    drop(pty);

    let pty = direct_pair(false)?;
    let mut line = vec![b'c'; 63];
    line.push(b'\n');
    for i in 0..64 {
        check!(
            pty.master_write(&line, true) == Ok(64),
            "canonical line {i}"
        );
    }
    let mut buf = [0u8; 64];
    woken_by(&pty, "canonical read", || {
        let _ = pty.slave_read(&mut buf, true);
    })?;
    pty.release(false);
    pty.release(true);
    drop(pty);
    check!(Pty::live() == 0, "a pty leaked");
    Ok(())
}

/// xorshift64: the soak's byte stream, reproducible on both ends.
struct Stream(u64);

impl Stream {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Raw: `total` pseudo-random bytes (every value, ^C included with ISIG
/// off) pushed through non-blocking writes against a reader that takes a
/// few hundred bytes at a time. Every byte must arrive, in order.
fn soak_raw(total: usize) -> Result<(usize, usize), String> {
    let (master, slave) = flow_pair(raw)?;
    let (mut source, mut check_stream, mut sizes) = (
        Stream(0x9e37_79b9_7f4a_7c15),
        Stream(0x9e37_79b9_7f4a_7c15),
        Stream(7),
    );
    let mut pending: Vec<u8> = Vec::new();
    let (mut sent, mut received, mut stalls, mut shorts) = (0usize, 0usize, 0usize, 0usize);
    let mut buf = vec![0u8; 1024];
    while received < total {
        while pending.len() < 3000 && sent + pending.len() < total {
            pending.push(source.next() as u8);
        }
        if !pending.is_empty() {
            match write(master, &pending) {
                EAGAIN => {
                    stalls += 1;
                    check!(!writable(master), "EAGAIN with POLLOUT at {sent}");
                }
                n if (n as usize) <= pending.len() => {
                    let n = n as usize;
                    shorts += usize::from(n < pending.len());
                    pending.drain(..n);
                    sent += n;
                }
                n => return Err(format!("write returned {n:#x} at {sent}")),
            }
        }
        // The slow reader: 1..=700 bytes per read, and one round in three
        // it does not read at all.
        let roll = sizes.next();
        if roll % 3 == 0 {
            continue;
        }
        let want = 1 + (roll / 3 % 700) as usize;
        let n = read(slave, &mut buf[..want]);
        if n == EAGAIN {
            continue;
        }
        for &byte in &buf[..n as usize] {
            let expect = check_stream.next() as u8;
            check!(byte == expect, "byte {received}: {byte:#x} != {expect:#x}");
            received += 1;
        }
    }
    check!(
        sent == total && pending.is_empty(),
        "sent {sent} of {total}"
    );
    close_pair(master, slave)?;
    Ok((stalls, shorts))
}

/// Canonical: at least `total` bytes of lines of 1..=200 printable bytes,
/// read a line at a time; every line must arrive whole and in order.
fn soak_canonical(total: usize) -> Result<usize, String> {
    let (master, slave) = flow_pair(|mut termios| {
        termios.lflag &= !(t::ECHO | t::ISIG | t::IEXTEN);
        termios
    })?;
    let mut stream = Stream(0x1234_5678_9abc_def1);
    let (mut data, mut ends) = (Vec::new(), Vec::new());
    while data.len() < total {
        let len = 1 + (stream.next() % 200) as usize;
        data.extend((0..len).map(|_| b'!' + (stream.next() % 90) as u8));
        data.push(b'\n');
        ends.push(data.len());
    }
    let (mut sent, mut received, mut stalls) = (0usize, 0usize, 0usize);
    let mut lines = ends.iter();
    let mut buf = vec![0u8; 512];
    for round in 0usize.. {
        if received >= data.len() {
            break;
        }
        if sent < data.len() {
            let chunk = &data[sent..data.len().min(sent + 2000)];
            match write(master, chunk) {
                EAGAIN => stalls += 1,
                n if (n as usize) <= chunk.len() => sent += n as usize,
                n => return Err(format!("write returned {n:#x} at {sent}")),
            }
        }
        // Two lines per round at most, and none one round in three: the
        // writer outruns the reader and meets a full queue.
        let reads = if round % 3 == 0 { 0 } else { 2 };
        for _ in 0..reads {
            let n = read(slave, &mut buf);
            if n == EAGAIN {
                break;
            }
            let end = *lines.next().ok_or("more lines than were sent")?;
            check!(
                buf[..n as usize] == data[received..end],
                "line at byte {received}: {n} bytes, wanted {}",
                end - received
            );
            received = end;
        }
    }
    check!(sent == data.len(), "sent {sent} of {}", data.len());
    close_pair(master, slave)?;
    Ok(stalls)
}

/// Many MiB through a pty with a slow reader, raw and canonical: no byte
/// lost or reordered, the writer really held back, nothing leaked.
pub fn pty_flow_soak() -> Result<(), String> {
    fresh()?;
    let (stalls, shorts) = soak_raw(4 << 20)?;
    check!(
        stalls > 0 && shorts > 0,
        "raw: stalls {stalls} shorts {shorts}"
    );
    let stalls = soak_canonical(1 << 20)?;
    check!(stalls > 0, "canonical: the writer never stalled");
    for round in 0..200 {
        let (master, slave) = flow_pair(raw)?;
        check!(
            write(master, &[b'f'; MAX_INPUT]) == MAX_INPUT as u64,
            "round {round}"
        );
        check!(write(master, b"x") == EAGAIN, "round {round}: not full");
        close_pair(master, slave)?;
    }
    Ok(())
}
