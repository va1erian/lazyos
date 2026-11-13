//! `os.lazy.print.v1`, the print spooler's interface (`idl/print.midl`): the
//! client an app prints through ([`PrintService`], a [`printd::Queue`]) and
//! the conversions between the generated wire types and `printd`'s, which
//! the `xui-printd` binary serves with.

use libmessenger::client::Errno;
use messenger_generated::os_lazy_print_v1 as wire;
use printd::{JobId, JobInfo, Queue, Request, State, Ticket};

use super::messenger::{client, CallError, Observed};

/// The service's registered name (the spooler serves it).
pub const NAME: &str = wire::SERVICE_NAME;
/// The interface id the spooler dispatches on.
pub const INTERFACE: u64 = wire::INTERFACE_ID;
/// Most document bytes one `Write` carries; a longer write is split.
pub const MAX_WRITE: usize = 256 * 1024;

/// The ticket on the wire: an unset choice is `0` or empty.
pub fn ticket_to_wire(ticket: &Ticket) -> wire::Ticket {
    wire::Ticket {
        name: ticket.name.clone(),
        format: ticket.format.clone(),
        copies: ticket.copies.map_or(0, |c| c.max(0) as u32),
        media: ticket.media.clone().unwrap_or_default(),
        color_mode: ticket.color_mode.clone().unwrap_or_default(),
        quality: ticket.quality.map_or(0, |q| q.max(0) as u32),
    }
}

/// The ticket a request carried. A number too large for IPP is kept large,
/// so the spooler refuses it rather than this wrapping it.
pub fn ticket_from_wire(ticket: wire::Ticket) -> Ticket {
    let number = |n: u32| (n != 0).then(|| i32::try_from(n).unwrap_or(i32::MAX));
    let text = |s: String| (!s.is_empty()).then_some(s);
    Ticket {
        name: ticket.name,
        format: ticket.format,
        copies: number(ticket.copies),
        media: text(ticket.media),
        color_mode: text(ticket.color_mode),
        quality: number(ticket.quality),
    }
}

fn state_to_wire(state: State) -> u32 {
    match state {
        State::Open => wire::STATE_OPEN,
        State::Queued => wire::STATE_QUEUED,
        State::Sending => wire::STATE_SENDING,
        State::Printing => wire::STATE_PRINTING,
        State::Done => wire::STATE_DONE,
        State::Failed => wire::STATE_FAILED,
        State::Canceled => wire::STATE_CANCELED,
    }
}

fn state_from_wire(state: u32) -> Option<State> {
    Some(match state {
        wire::STATE_OPEN => State::Open,
        wire::STATE_QUEUED => State::Queued,
        wire::STATE_SENDING => State::Sending,
        wire::STATE_PRINTING => State::Printing,
        wire::STATE_DONE => State::Done,
        wire::STATE_FAILED => State::Failed,
        wire::STATE_CANCELED => State::Canceled,
        _ => return None,
    })
}

pub fn info_to_wire(info: &JobInfo) -> wire::JobInfo {
    wire::JobInfo {
        job: info.id,
        name: info.name.clone(),
        printer: info.printer.clone(),
        state: state_to_wire(info.state),
        line: info.line.clone(),
        ink: info.ink.clone(),
    }
}

fn info_from_wire(info: wire::JobInfo) -> Result<JobInfo, String> {
    Ok(JobInfo {
        id: info.job,
        state: state_from_wire(info.state).ok_or("The print service sent an unknown job state")?,
        name: info.name,
        printer: info.printer,
        line: info.line,
        ink: info.ink,
    })
}

/// The spooler over Messenger. Every call waits what `idl/print.midl` states
/// (LazyWriter calls from its UI thread, so a stalled printd must not freeze
/// the window).
#[derive(Clone, Copy, Debug, Default)]
pub struct PrintService;

impl PrintService {
    fn call<R>(
        &self,
        run: impl FnOnce(&wire::Client<Observed>) -> Result<R, CallError>,
    ) -> Result<R, String> {
        let printd = client(wire::Client::connect).map_err(|_| {
            "Printing needs the print service (printd), which is not running".to_owned()
        })?;
        run(&printd).map_err(|error| match error {
            CallError::Remote { message, .. } if !message.is_empty() => message,
            CallError::Transport(errno) if errno == Errno::TIMEDOUT => {
                "The print service is not answering".to_owned()
            }
            CallError::Transport(errno) if errno == Errno::INVAL => {
                "The print request could not be encoded".to_owned()
            }
            CallError::Transport(errno) if errno == Errno::BADMSG => {
                "The print service sent a reply that does not read".to_owned()
            }
            error => format!("The print service did not answer (error {})", -error.code()),
        })
    }
}

impl Queue for PrintService {
    fn open(&self, request: &Request) -> Result<JobId, String> {
        // Checked here too, so no call is ever larger than printd reads.
        if !request.fields_fit() {
            return Err("A print job field is too long".into());
        }
        let ticket = ticket_to_wire(&request.ticket);
        self.call(|printd| printd.open(&request.printer, &request.user, &ticket))
    }

    fn write(&self, job: JobId, bytes: &[u8]) -> Result<(), String> {
        for piece in bytes.chunks(MAX_WRITE) {
            self.call(|printd| printd.write(job, piece))?;
        }
        Ok(())
    }

    fn close(&self, job: JobId) -> Result<(), String> {
        self.call(|printd| printd.close(job))
    }

    fn cancel(&self, job: JobId) -> Result<(), String> {
        self.call(|printd| printd.cancel(job))
    }

    fn status(&self, job: JobId) -> Result<JobInfo, String> {
        info_from_wire(self.call(|printd| printd.status(job))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_and_states_round_trip_through_the_wire_types() {
        let ticket = Ticket {
            name: "Letter".into(),
            format: "image/pwg-raster".into(),
            copies: Some(3),
            media: Some("iso_a4_210x297mm".into()),
            color_mode: None,
            quality: Some(5),
        };
        assert_eq!(ticket_from_wire(ticket_to_wire(&ticket)), ticket);
        let bare = Ticket::default();
        assert_eq!(ticket_from_wire(ticket_to_wire(&bare)), bare);
        let huge = wire::Ticket {
            copies: u32::MAX,
            ..wire::Ticket::default()
        };
        assert_eq!(ticket_from_wire(huge).copies, Some(i32::MAX));
        for state in [
            State::Open,
            State::Queued,
            State::Sending,
            State::Printing,
            State::Done,
            State::Failed,
            State::Canceled,
        ] {
            assert_eq!(state_from_wire(state_to_wire(state)), Some(state));
        }
        assert_eq!(state_from_wire(99), None);
    }
}
