//! `os.lazy.print.v1`, the print spooler's interface (`idl/print.midl`): the
//! client an app prints through ([`PrintService`], a [`printd::Queue`]) and
//! the conversions between the generated wire types and `printd`'s, which
//! the `xui-printd` binary serves with.

use messenger_generated::os_lazy_print_v1 as wire;
use printd::{JobId, JobInfo, Queue, Request, State, Ticket};

use super::messenger::Service;
use crate::server::ERROR_FIELD;
use crate::sys::errno::ETIMEDOUT;

/// The service's registered name.
pub const NAME: &str = "os.lazy.print";
/// The interface id every call carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;
/// Most document bytes one `Write` carries; a longer write is split.
pub const MAX_WRITE: usize = 256 * 1024;
/// How long one call may take, in 100 Hz PIT ticks (10 s): LazyWriter calls
/// from its UI thread, so a stalled printd must not freeze the window.
const CALL_TICKS: u64 = 1000;

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

/// The spooler over Messenger.
#[derive(Clone, Copy, Debug, Default)]
pub struct PrintService;

impl PrintService {
    fn call(
        &self,
        method: u32,
        body: Result<Vec<u8>, libmessenger::Error>,
    ) -> Result<Vec<u8>, String> {
        let body = body.map_err(|_| "The print request could not be encoded".to_owned())?;
        let service = Service::connect(NAME).map_err(|_| {
            "Printing needs the print service (printd), which is not running".to_owned()
        })?;
        match service.call_detailed_within(INTERFACE, method, ERROR_FIELD, body, CALL_TICKS) {
            Ok(reply) => Ok(reply.body),
            Err(error) if !error.message.is_empty() => Err(error.message),
            Err(error) if error.code == -ETIMEDOUT => {
                Err("The print service is not answering".to_owned())
            }
            Err(error) => Err(format!(
                "The print service did not answer (error {})",
                -error.code
            )),
        }
    }
}

fn bad_reply(_: libmessenger::Error) -> String {
    "The print service sent a reply that does not read".to_owned()
}

impl Queue for PrintService {
    fn open(&self, request: &Request) -> Result<JobId, String> {
        // Checked here too, so no call is ever larger than printd reads.
        if !request.fields_fit() {
            return Err("A print job field is too long".into());
        }
        let body = wire::encode_open_args(&wire::OpenArgs {
            printer: request.printer.clone(),
            user: request.user.clone(),
            ticket: ticket_to_wire(&request.ticket),
        });
        let reply = self.call(wire::METHOD_OPEN, body)?;
        Ok(wire::decode_open_reply(&reply).map_err(bad_reply)?.job)
    }

    fn write(&self, job: JobId, bytes: &[u8]) -> Result<(), String> {
        for piece in bytes.chunks(MAX_WRITE) {
            let body = wire::encode_write_args(&wire::WriteArgs {
                job,
                bytes: piece.to_vec(),
            });
            self.call(wire::METHOD_WRITE, body)?;
        }
        Ok(())
    }

    fn close(&self, job: JobId) -> Result<(), String> {
        let body = wire::encode_close_args(&wire::CloseArgs { job });
        self.call(wire::METHOD_CLOSE, body).map(drop)
    }

    fn cancel(&self, job: JobId) -> Result<(), String> {
        let body = wire::encode_cancel_args(&wire::CancelArgs { job });
        self.call(wire::METHOD_CANCEL, body).map(drop)
    }

    fn status(&self, job: JobId) -> Result<JobInfo, String> {
        let body = wire::encode_status_args(&wire::StatusArgs { job });
        let reply = self.call(wire::METHOD_STATUS, body)?;
        info_from_wire(wire::decode_status_reply(&reply).map_err(bad_reply)?.info)
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
