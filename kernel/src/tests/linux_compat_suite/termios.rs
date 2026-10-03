//! Terminals: termios round trips on the console, the line discipline's
//! editing rules, and pseudo-terminals end to end.

use super::*;
use crate::tty::{termios as t, Ldisc, Signal, Termios};

const IOCTL: u64 = 16;
const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;
const TIOCGWINSZ: u64 = 0x5413;
const TIOCSWINSZ: u64 = 0x5414;
const TIOCGPTN: u64 = 0x8004_5430;
const TIOCSPTLCK: u64 = 0x4004_5431;
const ENOTTY: u64 = neg(25);
const ENXIO: u64 = neg(6);
const EIO: u64 = neg(5);

fn tcgets(fd: u64) -> Result<Termios, String> {
    let mut raw = [0u8; t::SIZE];
    let got = sys(IOCTL, &[fd, TCGETS, raw.as_mut_ptr() as u64]);
    check!(got == 0, "TCGETS({fd}) = {got:#x}");
    Ok(Termios::from_bytes(&raw))
}

fn tcsets(fd: u64, termios: Termios) -> u64 {
    let raw = termios.to_bytes();
    sys(IOCTL, &[fd, TCSETS, raw.as_ptr() as u64])
}

/// Open a pty pair: `(master, slave)`.
fn openpty() -> Result<(u64, u64), String> {
    let ptmx = cpath("/dev/ptmx");
    let master = sys(2, &[ptmx.as_ptr() as u64, 2, 0]);
    check!((master as i64) >= 3, "open /dev/ptmx: {master:#x}");
    let mut index = u32::MAX;
    check!(
        sys(IOCTL, &[master, TIOCGPTN, &mut index as *mut u32 as u64]) == 0,
        "TIOCGPTN"
    );
    let name = cpath(&format!("/dev/pts/{index}"));
    check!(
        sys(2, &[name.as_ptr() as u64, 2, 0]) == ENXIO,
        "a locked slave opened"
    );
    let unlock = 0i32;
    check!(
        sys(IOCTL, &[master, TIOCSPTLCK, &unlock as *const i32 as u64]) == 0,
        "unlockpt"
    );
    let slave = sys(2, &[name.as_ptr() as u64, 2 | 0o400, 0]);
    check!((slave as i64) >= 3, "open slave: {slave:#x}");
    Ok((master, slave))
}

fn write(fd: u64, data: &[u8]) -> u64 {
    sys(1, &[fd, data.as_ptr() as u64, data.len() as u64])
}

fn read(fd: u64, buf: &mut [u8]) -> u64 {
    sys(0, &[fd, buf.as_mut_ptr() as u64, buf.len() as u64])
}

/// The console answers `TCGETS` with sane defaults and keeps what `TCSETS`
/// stores; a pipe is not a terminal; a pty carries input, echo, output
/// post-processing, window size and hang-up.
pub fn termios_roundtrip() -> Result<(), String> {
    fresh()?;
    let console = tcgets(0)?;
    check!(
        console.canonical() && console.echo() && console.signals(),
        "console defaults {console:?}"
    );
    let mut raw = console;
    raw.lflag &= !(t::ICANON | t::ECHO);
    check!(tcsets(0, raw) == 0, "TCSETS");
    check!(tcgets(1)? == raw, "the console did not keep its settings");
    check!(tcsets(0, console) == 0, "restore");
    let (r, w) = pipe()?;
    let mut buf = [0u8; 64];
    check!(
        sys(IOCTL, &[r, TCGETS, buf.as_mut_ptr() as u64]) == ENOTTY,
        "a pipe is a tty"
    );
    sys(3, &[r]);
    sys(3, &[w]);
    let (master, slave) = openpty()?;
    check!(tcgets(slave)?.canonical(), "pty defaults");
    // Canonical input: nothing to read until the line ends; echo with CRLF.
    check!(write(master, b"ls -l") == 5, "type");
    check!(task::fd_set_status(slave as usize, true), "O_NONBLOCK");
    check!(read(slave, &mut buf) == EAGAIN, "a half line was readable");
    check!(write(master, b"\r") == 1, "enter");
    let n = read(slave, &mut buf);
    check!(
        n == 6 && &buf[..6] == b"ls -l\n",
        "line read {n}: {:?}",
        &buf[..n.min(64) as usize]
    );
    let n = read(master, &mut buf);
    check!(
        &buf[..n as usize] == b"ls -l\r\n",
        "echo {:?}",
        &buf[..n as usize]
    );
    // Output post-processing.
    check!(write(slave, b"a\nb") == 3, "slave write");
    let n = read(master, &mut buf);
    check!(
        &buf[..n as usize] == b"a\r\nb",
        "output {:?}",
        &buf[..n as usize]
    );
    // Window size round trip.
    let size = [30u16, 100, 0, 0];
    check!(
        sys(IOCTL, &[master, TIOCSWINSZ, size.as_ptr() as u64]) == 0,
        "TIOCSWINSZ"
    );
    let mut got = [0u16; 4];
    check!(
        sys(IOCTL, &[slave, TIOCGWINSZ, got.as_mut_ptr() as u64]) == 0 && got[..2] == [30, 100],
        "winsize {got:?}"
    );
    // Hang-up: closing the master ends the slave's reads, its writes are EIO.
    sys(3, &[master]);
    check!(read(slave, &mut buf) == 0, "no EOF after hang-up");
    check!(write(slave, b"x") == EIO, "write after hang-up");
    sys(3, &[slave]);
    check!(
        crate::tty::pty::Pty::live() == 0,
        "a closed pty is still live"
    );
    Ok(())
}

fn feed(ldisc: &mut Ldisc, bytes: &[u8]) -> (Vec<u8>, Vec<Signal>) {
    let mut echo = Vec::new();
    let mut signals = Vec::new();
    for &byte in bytes {
        signals.extend(ldisc.input(byte, &mut echo));
    }
    (echo, signals)
}

fn take(ldisc: &mut Ldisc) -> Vec<u8> {
    let mut buf = [0u8; 256];
    let n = ldisc.read(&mut buf);
    buf[..n].to_vec()
}

/// The canonical editing rules, end-of-file, literal-next, signals and the
/// switch to raw mode.
pub fn termios_canonical_line() -> Result<(), String> {
    let mut l = Ldisc::new();
    let (echo, _) = feed(&mut l, b"hellp\x7f\x7flo\r");
    check!(take(&mut l) == b"hello\n", "erase");
    check!(
        echo == b"hellp\x08 \x08\x08 \x08lo\r\n",
        "erase echo {:?}",
        echo
    );
    feed(&mut l, b"junk\x15ok\n");
    check!(take(&mut l) == b"ok\n", "kill");
    feed(&mut l, b"one two\x17three\n");
    check!(take(&mut l) == b"one three\n", "werase");
    feed(&mut l, b"partial\x04");
    check!(take(&mut l) == b"partial", "VEOF with text");
    feed(&mut l, b"\x04");
    check!(
        l.readable() && take(&mut l).is_empty() && !l.readable(),
        "VEOF on an empty line is EOF"
    );
    feed(&mut l, b"\x16\x03\n");
    check!(take(&mut l) == b"\x03\n", "literal next");
    let (echo, signals) = feed(&mut l, b"abc\x03");
    check!(signals == [Signal::Interrupt] && !l.readable(), "^C");
    check!(echo.ends_with(b"^C"), "^C echo {:?}", echo);
    // Two lines are read one per call, a short buffer splits a line.
    feed(&mut l, b"first\nsecond\n");
    let mut small = [0u8; 3];
    check!(l.read(&mut small) == 3 && &small == b"fir", "partial line");
    check!(
        take(&mut l) == b"st\n" && take(&mut l) == b"second\n",
        "line boundaries"
    );
    // Raw mode: bytes pass through at once, unedited, unechoed.
    let mut raw = l.termios;
    raw.lflag &= !(t::ICANON | t::ECHO | t::ISIG);
    raw.iflag &= !t::ICRNL;
    feed(&mut l, b"half");
    l.set_termios(raw);
    check!(
        take(&mut l) == b"half",
        "the edited line on leaving canonical mode"
    );
    let (echo, signals) = feed(&mut l, b"\x7f\r\x03");
    check!(echo.is_empty() && signals.is_empty(), "raw echo/signals");
    check!(take(&mut l) == b"\x7f\r\x03", "raw bytes");
    let mut out = Vec::new();
    l.output(b"x\ny", &mut out);
    check!(out == b"x\r\ny", "ONLCR");
    Ok(())
}

/// Many pty pairs opened, used and closed: nothing leaks, every line arrives.
pub fn termios_soak() -> Result<(), String> {
    fresh()?;
    let mut buf = [0u8; 128];
    for round in 0..300usize {
        let (master, slave) = openpty()?;
        for line in 0..4usize {
            let text = format!("r{round}l{line}\r");
            check!(
                write(master, text.as_bytes()) == text.len() as u64,
                "round {round}: type"
            );
            let n = read(slave, &mut buf) as usize;
            check!(
                &buf[..n] == format!("r{round}l{line}\n").as_bytes(),
                "round {round}: line {line}"
            );
            let n = read(master, &mut buf) as usize;
            check!(n == text.len() + 1, "round {round}: echo {n}");
        }
        sys(3, &[slave]);
        let n = sys(0, &[master, buf.as_mut_ptr() as u64, 16]);
        check!(
            n == EIO,
            "round {round}: master read after the slave closed: {n:#x}"
        );
        sys(3, &[master]);
    }
    check!(
        crate::tty::pty::Pty::live() == 0,
        "{} ptys leaked",
        crate::tty::pty::Pty::live()
    );
    for fd in 3..task::harness::fd_table_len() {
        check!(task::fd_kind(fd) == task::FdKind::Closed, "fd {fd} leaked");
    }
    Ok(())
}
