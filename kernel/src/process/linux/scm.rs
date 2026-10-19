//! `SCM_RIGHTS` for `sendmsg`/`recvmsg`: the control-message codec and the
//! install-on-receive step.
//!
//! ```text
//! struct cmsghdr { size_t cmsg_len; int cmsg_level; int cmsg_type; /* data */ };
//! ```
//!
//! Sending parses the caller's control buffer into a
//! [`Rights`](crate::ipc::pipe::Rights): the descriptors are cloned out of the
//! caller's table (so closing them straight after `sendmsg` is fine) and ride
//! the socket's queue beside the message's bytes (`ipc/pipe/rights.rs`).
//! Receiving installs them in the receiver's table, lowest free numbers
//! first, `FD_CLOEXEC` when `MSG_CMSG_CLOEXEC` was asked, and writes one
//! `SCM_RIGHTS` control message. Descriptors that do not fit the control
//! buffer, or the table, are closed and `MSG_CTRUNC` is set.
//!
//! Any open descriptor may be sent, as on Linux. Two kinds deserve a note: a
//! Messenger endpoint watch (`Fd::Endpoint`) is only a pollable view, with no
//! authority (it cannot receive, send or close, and re-checks on every poll
//! that its owner still holds the handle), and a `fork` child already inherits
//! the same view, so passing one widens nothing; an epoll instance holds its
//! own references to what it watches, as on Linux.
//!
//! `SCM_CREDENTIALS` and every other control message are refused: the former
//! with `EOPNOTSUPP` (credentials are not carried), the rest with `EINVAL`.

use alloc::vec::Vec;

use crate::ipc::pipe::{Rights, RightsError, SCM_MAX_FD};
use crate::task::{self, Fd};
use crate::user_ptr;

use super::errno::{err, EBADF, EFAULT, EINVAL, ENOMEM, EOPNOTSUPP, ETOOMANYREFS};

const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;
const SCM_CREDENTIALS: i32 = 2;

/// `sizeof(struct cmsghdr)`.
const HEADER: usize = 16;

/// `msg_flags` bit: the control buffer was too small (`MSG_CTRUNC`).
pub(super) const MSG_CTRUNC: i32 = 0x8;

/// Most control bytes one `sendmsg` may pass (Linux's `optmem_max`).
const CONTROL_MAX: u64 = 20480;
const ENOBUFS: u64 = 105;

fn align8(len: usize) -> usize {
    (len + 7) & !7
}

/// Parse a `sendmsg` control buffer of `controllen` bytes at `control`. The
/// descriptors of every `SCM_RIGHTS` message are cloned from the caller's
/// table into one [`Rights`]; `None` when there are none.
pub(super) fn parse_send(control: u64, controllen: u64) -> Result<Option<Rights>, u64> {
    if controllen > CONTROL_MAX {
        return Err(err(ENOBUFS));
    }
    let bytes = user_ptr::try_bytes(control, controllen as usize).map_err(|_| err(EFAULT))?;
    let mut numbers: Vec<i32> = Vec::new();
    let mut at = 0usize;
    while at + HEADER <= bytes.len() {
        let word = |from: usize| -> u64 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[from..from + 8]);
            u64::from_le_bytes(raw)
        };
        let len = usize::try_from(word(at)).map_err(|_| err(EINVAL))?;
        let (level, kind) = (
            word(at + 8) as u32 as i32,
            (word(at + 8) >> 32) as u32 as i32,
        );
        if len < HEADER || len > bytes.len() - at {
            return Err(err(EINVAL));
        }
        match (level, kind) {
            (SOL_SOCKET, SCM_RIGHTS) => {
                let count = (len - HEADER) / 4;
                if numbers.len() + count > SCM_MAX_FD {
                    return Err(err(EINVAL));
                }
                for index in 0..count {
                    let mut raw = [0u8; 4];
                    raw.copy_from_slice(&bytes[at + HEADER + index * 4..][..4]);
                    numbers.push(i32::from_le_bytes(raw));
                }
            }
            (SOL_SOCKET, SCM_CREDENTIALS) => return Err(err(EOPNOTSUPP)),
            _ => return Err(err(EINVAL)),
        }
        at += align8(len);
    }
    if numbers.is_empty() {
        return Ok(None);
    }
    let mut fds: Vec<Fd> = Vec::new();
    fds.try_reserve_exact(numbers.len())
        .map_err(|_| err(ENOMEM))?;
    for number in numbers {
        let fd = usize::try_from(number)
            .ok()
            .and_then(task::fd_clone)
            .ok_or(err(EBADF))?;
        fds.push(fd);
    }
    let uid = crate::ipc::credentials::of(task::current()).uid;
    match Rights::new(fds, uid) {
        Ok(rights) => Ok(Some(rights)),
        Err(RightsError::TooMany) => Err(err(EINVAL)),
        Err(RightsError::Quota) => Err(err(ETOOMANYREFS)),
    }
}

/// What `recvmsg` reports about its control buffer: the bytes used and the
/// flags to or into `msg_flags`.
pub(super) struct Delivered {
    pub(super) used: u64,
    pub(super) flags: i32,
}

/// Install `rights` (the descriptors a receive took out of the queue) in the
/// caller's table and write them as one `SCM_RIGHTS` message into the control
/// buffer `[control, control + controllen)`. What does not fit is closed.
pub(super) fn deliver(
    mut rights: Vec<Fd>,
    control: (u64, u64),
    cloexec: bool,
) -> Result<Delivered, u64> {
    let (control, controllen) = control;
    if rights.is_empty() {
        return Ok(Delivered { used: 0, flags: 0 });
    }
    let total = rights.len();
    let room = if control != 0 && controllen as usize > HEADER {
        (controllen as usize - HEADER) / 4
    } else {
        0
    };
    // Closed when dropped, here or on the way out, with nothing locked.
    let overflow = rights.split_off(room.min(total));
    let mut numbers: Vec<i32> = Vec::new();
    numbers
        .try_reserve_exact(rights.len())
        .map_err(|_| err(ENOMEM))?;
    for fd in rights {
        let Some(number) = task::fd_open(fd) else {
            break; // the table is full: this one and the rest are closed
        };
        if cloexec {
            task::fd_set_cloexec(number, true);
        }
        numbers.push(number as i32);
    }
    drop(overflow);
    let mut flags = 0;
    if numbers.len() < total {
        flags |= MSG_CTRUNC;
    }
    if numbers.is_empty() {
        return Ok(Delivered { used: 0, flags });
    }
    let len = HEADER + numbers.len() * 4;
    let mut message: Vec<u8> = Vec::new();
    message.try_reserve_exact(len).map_err(|_| err(ENOMEM))?;
    message.extend_from_slice(&(len as u64).to_le_bytes());
    message.extend_from_slice(&SOL_SOCKET.to_le_bytes());
    message.extend_from_slice(&SCM_RIGHTS.to_le_bytes());
    for number in &numbers {
        message.extend_from_slice(&number.to_le_bytes());
    }
    if user_ptr::try_copy_to(control, &message).is_err() {
        // The caller cannot learn the numbers: do not leave them open.
        for number in numbers {
            task::fd_close(number as usize);
        }
        return Err(err(EFAULT));
    }
    Ok(Delivered {
        used: align8(len).min(controllen as usize) as u64,
        flags,
    })
}

/// Refused sends reported so far (every one is counted, the first
/// [`REFUSALS_SHOWN`] and then every 1024th are printed).
static REFUSALS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
const REFUSALS_SHOWN: u64 = 32;

/// `code` as it is, after printing `PROC:SCM:REFUSE` when it is the
/// `ETOOMANYREFS` of a refused `SCM_RIGHTS` send (`why`: `quota` for the
/// in-flight budget, `cycle` for `ipc::pipe::reach`). A program usually just
/// loses the message (musl calls the error "No error information"), so the
/// kernel says which process and descriptor it was (issue #865).
pub(super) fn note_refusal(code: u64, fd: u64, why: &str) -> u64 {
    if code != err(ETOOMANYREFS) {
        return code;
    }
    let count = REFUSALS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    if count <= REFUSALS_SHOWN || count % 1024 == 0 {
        let (comm, len) = task::linuxstate::comm(task::current());
        crate::trace::line!(
            tracefmt::Channel::Proc,
            "SCM:REFUSE why={} tgid={} fd={} count={} comm={}",
            why,
            task::linuxstate::tgid(),
            fd,
            count,
            core::str::from_utf8(&comm[..len]).unwrap_or("?")
        );
    }
    code
}
