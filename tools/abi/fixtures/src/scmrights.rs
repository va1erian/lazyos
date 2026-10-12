//! `scmrights` — descriptors and shared memory between processes, the way
//! WebKit's IPC, Wayland and D-Bus use them: `SCM_RIGHTS` over a
//! `socketpair` with `sendmsg`/`recvmsg`, a `memfd_create` file mapped
//! `MAP_SHARED` on both sides, and a `MAP_SHARED | MAP_ANONYMOUS` mapping a
//! `fork` child keeps sharing.
//!
//! The parent sends a pipe's write end and a memfd to a forked child in one
//! message (received `MSG_CMSG_CLOEXEC`), then two more write ends to a child
//! whose control buffer holds one (`MSG_CTRUNC`). The child maps the memfd,
//! reads what the parent wrote, answers through the mapping and the pipe, and
//! exits. The parent checks the answer, and that every write end is closed
//! afterwards (the pipe reaches end-of-file): a descriptor leaked in flight,
//! or one the truncated receive failed to close, would hold it open.

mod common;

use std::ffi::{c_char, c_int, c_long, c_void};

const NAME: &str = "scmrights";

const AF_UNIX: c_int = 1;
const SOCK_STREAM: c_int = 1;
const SOL_SOCKET: c_int = 1;
const SCM_RIGHTS: c_int = 1;
const MSG_CTRUNC: c_int = 0x8;
const MSG_CMSG_CLOEXEC: c_int = 0x4000_0000;
const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const MAP_SHARED: c_int = 1;
const MAP_ANONYMOUS: c_int = 0x20;
const F_GETFD: c_int = 1;
const F_SETFL: c_int = 4;
const FD_CLOEXEC: c_int = 1;
const O_NONBLOCK: c_int = 0o4000;
const SYS_MEMFD_CREATE: c_long = 319;
const PAGE: usize = 4096;

#[repr(C)]
struct IoVec {
    base: *mut c_void,
    len: usize,
}

#[repr(C)]
struct MsgHdr {
    name: *mut c_void,
    namelen: u32,
    iov: *mut IoVec,
    iovlen: usize,
    control: *mut c_void,
    controllen: usize,
    flags: c_int,
}

unsafe extern "C" {
    fn socketpair(domain: c_int, kind: c_int, protocol: c_int, sv: *mut c_int) -> c_int;
    fn pipe(fds: *mut c_int) -> c_int;
    fn fork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, len: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, len: usize) -> isize;
    fn sendmsg(fd: c_int, msg: *const MsgHdr, flags: c_int) -> isize;
    fn recvmsg(fd: c_int, msg: *mut MsgHdr, flags: c_int) -> isize;
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn ftruncate(fd: c_int, len: i64) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn syscall(number: c_long, ...) -> c_long;
    fn _exit(code: c_int) -> !;
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `CMSG_SPACE(n descriptors)` in 8-byte words.
fn control_words(fds: usize) -> usize {
    (16 + ((fds * 4 + 7) & !7)) / 8
}

/// `sendmsg` of `data` carrying `fds` as one `SCM_RIGHTS` message.
fn send_fds(sock: c_int, data: &[u8], fds: &[c_int]) -> Result<(), String> {
    let mut control = vec![0u64; control_words(fds.len())];
    control[0] = (16 + fds.len() * 4) as u64; // cmsg_len
    control[1] = (SOL_SOCKET as u32 as u64) | ((SCM_RIGHTS as u32 as u64) << 32);
    // SAFETY: the buffer is `control_words` long, which holds the header and
    // every descriptor; the data starts 16 bytes (two words) in.
    unsafe {
        let data_ptr = control.as_mut_ptr().add(2) as *mut c_int;
        for (index, fd) in fds.iter().enumerate() {
            data_ptr.add(index).write(*fd);
        }
    }
    let mut iov = IoVec {
        base: data.as_ptr() as *mut c_void,
        len: data.len(),
    };
    let msg = MsgHdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: control.as_mut_ptr().cast(),
        controllen: control.len() * 8,
        flags: 0,
    };
    // SAFETY: `msg` points at live buffers for the call.
    let sent = unsafe { sendmsg(sock, &msg, 0) };
    if sent == data.len() as isize {
        Ok(())
    } else {
        Err(format!("sendmsg returned {sent}, errno {}", errno()))
    }
}

/// What one `recvmsg` returned.
struct Received {
    data: Vec<u8>,
    fds: Vec<c_int>,
    flags: c_int,
}

/// `recvmsg` with room for `room` descriptors.
fn recv_fds(sock: c_int, room: usize, flags: c_int) -> Result<Received, String> {
    let mut buf = [0u8; 64];
    let mut control = vec![0u64; control_words(room)];
    let mut iov = IoVec {
        base: buf.as_mut_ptr().cast(),
        len: buf.len(),
    };
    let mut msg = MsgHdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: control.as_mut_ptr().cast(),
        // CMSG_LEN, not CMSG_SPACE: the padding would hold one more descriptor.
        controllen: 16 + room * 4,
        flags: 0,
    };
    // SAFETY: `msg` points at live buffers for the call.
    let got = unsafe { recvmsg(sock, &mut msg, flags) };
    if got < 0 {
        return Err(format!("recvmsg failed, errno {}", errno()));
    }
    let mut fds = Vec::new();
    if msg.controllen >= 16 {
        let (len, level, kind) = (
            control[0] as usize,
            control[1] as u32 as c_int,
            (control[1] >> 32) as u32 as c_int,
        );
        if level != SOL_SOCKET || kind != SCM_RIGHTS || len < 16 || len > msg.controllen {
            return Err(format!("unexpected control message {len}/{level}/{kind}"));
        }
        // SAFETY: the kernel wrote `len - 16` bytes of descriptors after the
        // header, inside the buffer (checked against `controllen` above).
        unsafe {
            let data = control.as_ptr().add(2) as *const c_int;
            for index in 0..(len - 16) / 4 {
                fds.push(data.add(index).read());
            }
        }
    }
    Ok(Received {
        data: buf[..got as usize].to_vec(),
        fds,
        flags: msg.flags,
    })
}

/// Map `len` bytes of `fd` (or anonymous memory for `-1`) shared, read/write.
fn map_shared(fd: c_int, len: usize) -> Result<*mut u8, String> {
    let flags = MAP_SHARED | if fd < 0 { MAP_ANONYMOUS } else { 0 };
    // SAFETY: a fresh mapping at a kernel-chosen address.
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            flags,
            fd,
            0,
        )
    };
    if ptr as isize == -1 || ptr.is_null() {
        Err(format!("mmap(MAP_SHARED, fd {fd}) failed, errno {}", errno()))
    } else {
        Ok(ptr.cast())
    }
}

/// Copy `text` to `ptr + at`.
///
/// # Safety
/// `ptr + at .. + text.len()` must be inside a live writable mapping.
unsafe fn put(ptr: *mut u8, at: usize, text: &[u8]) {
    std::ptr::copy_nonoverlapping(text.as_ptr(), ptr.add(at), text.len());
}

/// The `len` bytes at `ptr + at`.
///
/// # Safety
/// The range must be inside a live readable mapping.
unsafe fn get(ptr: *const u8, at: usize, len: usize) -> Vec<u8> {
    std::slice::from_raw_parts(ptr.add(at), len).to_vec()
}

/// The child: receive, map, answer, exit with a code naming what failed.
fn child(sock: c_int, shared: *mut u8) -> Result<(), (i32, String)> {
    let first = recv_fds(sock, 2, MSG_CMSG_CLOEXEC).map_err(|e| (10, e))?;
    if first.data != b"A" || first.fds.len() != 2 || first.flags & MSG_CTRUNC != 0 {
        return Err((11, format!("first message: {:?} {} fds, flags {:#x}", first.data, first.fds.len(), first.flags)));
    }
    for fd in &first.fds {
        // SAFETY: plain query.
        if unsafe { fcntl(*fd, F_GETFD) } & FD_CLOEXEC == 0 {
            return Err((12, format!("descriptor {fd} lacks FD_CLOEXEC")));
        }
    }
    let (pipe_end, memfd) = (first.fds[0], first.fds[1]);
    let map = map_shared(memfd, PAGE).map_err(|e| (13, e))?;
    // SAFETY: both mappings are `PAGE` bytes.
    unsafe {
        if get(map, 0, 12) != b"parent-wrote" {
            return Err((14, format!("memfd holds {:?}", get(map, 0, 12))));
        }
        put(map, 64, b"child-wrote");
        put(shared, 0, b"inherited");
    }
    // SAFETY: a valid descriptor and a live buffer.
    if unsafe { write(pipe_end, b"pipe-data".as_ptr().cast(), 9) } != 9 {
        return Err((15, format!("pipe write, errno {}", errno())));
    }
    let second = recv_fds(sock, 1, 0).map_err(|e| (16, e))?;
    if second.data != b"B" || second.fds.len() != 1 || second.flags & MSG_CTRUNC == 0 {
        return Err((17, format!("second message: {:?} {} fds, flags {:#x}", second.data, second.fds.len(), second.flags)));
    }
    for fd in [pipe_end, memfd, second.fds[0]] {
        // SAFETY: descriptors this process owns.
        unsafe { close(fd) };
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let mut sv = [0 as c_int; 2];
    let mut p = [0 as c_int; 2];
    // SAFETY: each call gets a two-element array.
    unsafe {
        if socketpair(AF_UNIX, SOCK_STREAM, 0, sv.as_mut_ptr()) != 0 {
            return Err(format!("socketpair, errno {}", errno()));
        }
        if pipe(p.as_mut_ptr()) != 0 {
            return Err(format!("pipe, errno {}", errno()));
        }
        if fcntl(p[0], F_SETFL, O_NONBLOCK) != 0 {
            return Err(format!("O_NONBLOCK, errno {}", errno()));
        }
    }
    // SAFETY: a raw syscall with a NUL-terminated name and flags 0.
    let memfd = unsafe { syscall(SYS_MEMFD_CREATE, c"abi-scmrights".as_ptr() as *const c_char, 0) };
    if memfd < 0 {
        return Err(format!("memfd_create, errno {}", errno()));
    }
    let memfd = memfd as c_int;
    // SAFETY: a valid descriptor.
    if unsafe { ftruncate(memfd, PAGE as i64) } != 0 {
        return Err(format!("ftruncate, errno {}", errno()));
    }
    let map = map_shared(memfd, PAGE)?;
    let inherited = map_shared(-1, PAGE)?;
    // SAFETY: both mappings are `PAGE` bytes.
    unsafe { put(map, 0, b"parent-wrote") };

    // SAFETY: `fork` in a single-threaded process.
    let pid = unsafe { fork() };
    if pid < 0 {
        return Err(format!("fork, errno {}", errno()));
    }
    if pid == 0 {
        // SAFETY: closing this process's copies of ends it does not use.
        unsafe {
            close(sv[0]);
            close(p[0]);
        }
        let code = match child(sv[1], inherited) {
            Ok(()) => 0,
            Err((code, why)) => {
                println!("ABI:{NAME}:CHILD:{why}");
                code
            }
        };
        // SAFETY: the child never returns into the parent's code.
        unsafe { _exit(code) }
    }
    // SAFETY: closing the parent's copy of the child's end.
    unsafe { close(sv[1]) };
    send_fds(sv[0], b"A", &[p[1], memfd])?;
    send_fds(sv[0], b"B", &[p[1], p[1]])?;
    // The sender may close its own copies right away: the ones in flight
    // keep the pipe and the file open.
    // SAFETY: descriptors this process owns.
    unsafe {
        close(p[1]);
        close(memfd);
    }

    let mut status = 0;
    // SAFETY: `status` is a live int.
    if unsafe { waitpid(pid, &mut status, 0) } != pid || status != 0 {
        return Err(format!("the child ended with status {status:#x}"));
    }
    // SAFETY: the mappings are `PAGE` bytes and the child is gone.
    let (answer, inherited_text) = unsafe { (get(map, 64, 11), get(inherited, 0, 9)) };
    if answer != b"child-wrote" {
        return Err(format!("the memfd mapping holds {answer:?}, not the child's bytes"));
    }
    if inherited_text != b"inherited" {
        return Err(format!("the inherited shared mapping holds {inherited_text:?}"));
    }
    let mut buf = [0u8; 16];
    // SAFETY: a valid descriptor and buffer.
    let got = unsafe { read(p[0], buf.as_mut_ptr().cast(), 16) };
    if got != 9 || &buf[..9] != b"pipe-data" {
        return Err(format!("the pipe delivered {got} bytes"));
    }
    // SAFETY: as above.
    let eof = unsafe { read(p[0], buf.as_mut_ptr().cast(), 16) };
    if eof != 0 {
        return Err(format!(
            "the pipe is not at end-of-file ({eof}, errno {}): a write end is still open",
            errno()
        ));
    }
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => common::pass(NAME),
        Err(reason) => common::fail(NAME, &reason),
    }
}
