//! `xui-printd` (`/system/bin/printd`): the print spooler, serving
//! `os.lazy.print.v1` (`idl/print.midl`, docs/printing-plan.md P6).
//!
//! `init` starts it on desktop images with the network stack. Apps hand it
//! whole documents; it keeps them in [`fhs::state::PRINT_SPOOL`] until their
//! printer has them, so a job outlives the app that printed it and a
//! printer never gets half a request. The queue itself is the `printd`
//! crate; this file publishes it on Messenger, answering each call as the
//! kernel-stamped uid of its sender.
//!
//! It is a static musl program (threads and `std::net` for the sending
//! thread) with no window, running as `init`'s identity like the other
//! platform services.
//!
//! Serial evidence: `PRINTD:UP:PASS` once it serves, `PRINTD:FAIL:<why>`
//! when it cannot, and `PRINTD:JOB:<state>:<id>:<line>` as each job ends.

use std::path::Path;
use std::process::ExitCode;

use libmessenger::client::Errno;
use libmessenger::server::{Caller, Fault};
use messenger_generated::os_lazy_print_v1 as wire;
use printd::{JobInfo, Request, Spooler};
use xui_app::platform::print::{self, INTERFACE, MAX_WRITE, NAME};
use xui_app::server::Server;

fn main() -> ExitCode {
    let report: printd::Report = Box::new(|info: &JobInfo| {
        println!("PRINTD:JOB:{}:{}:{}", info.state.word(), info.id, info.line);
    });
    let spooler = match Spooler::open_reporting(Path::new(fhs::state::PRINT_SPOOL), Some(report)) {
        Ok(spooler) => spooler,
        Err(error) => {
            println!("PRINTD:FAIL:spool {}: {error}", fhs::state::PRINT_SPOOL);
            return ExitCode::FAILURE;
        }
    };
    let server = match Server::register(NAME, &[INTERFACE], &[wire::INTERFACE_NAME]) {
        Ok(server) => server,
        Err(code) => {
            println!("PRINTD:FAIL:register {code}");
            return ExitCode::FAILURE;
        }
    };
    println!("PRINTD:UP:PASS");
    // One receive buffer: a full `Write` and its framing.
    let mut buf = vec![0u8; MAX_WRITE + 64 * 1024];
    loop {
        if let Err(code) = server.wait(0) {
            println!("PRINTD:FAIL:wait {code}");
            return ExitCode::FAILURE;
        }
        loop {
            let request = match server.poll(&mut buf) {
                Ok(Some(request)) => request,
                Ok(None) => break,
                // A call too large for the buffer was dropped; go on.
                Err(_) => continue,
            };
            let reply = wire::dispatch(&mut Spool(&spooler), &request);
            if let Some(txn) = request.txn {
                let _ = server.reply(txn, &reply);
            }
        }
    }
}

/// The spooler, answering `os.lazy.print.v1` as the kernel-stamped uid of each
/// caller (the generated server). A refusal is `EINVAL` with the line the
/// caller shows.
struct Spool<'a>(&'a Spooler);

fn refused(message: impl AsRef<str>) -> Fault {
    Fault::refuse(Errno::INVAL, message.as_ref())
}

impl wire::Server for Spool<'_> {
    fn open(
        &mut self,
        cx: &Caller,
        printer: String,
        user: String,
        ticket: wire::Ticket,
    ) -> Result<u32, Fault> {
        let request = Request {
            printer,
            user,
            ticket: print::ticket_from_wire(ticket),
        };
        self.0.open_job(cx.uid, &request).map_err(refused)
    }

    fn write(&mut self, cx: &Caller, job: u32, bytes: Vec<u8>) -> Result<(), Fault> {
        if bytes.len() > MAX_WRITE {
            return Err(refused("A print write is too large"));
        }
        self.0.write(cx.uid, job, &bytes).map_err(refused)
    }

    fn close(&mut self, cx: &Caller, job: u32) -> Result<(), Fault> {
        self.0.close(cx.uid, job).map_err(refused)
    }

    fn cancel(&mut self, cx: &Caller, job: u32) -> Result<(), Fault> {
        self.0.cancel(cx.uid, job).map_err(refused)
    }

    fn status(&mut self, cx: &Caller, job: u32) -> Result<wire::JobInfo, Fault> {
        let info = self.0.status(cx.uid, job).map_err(refused)?;
        Ok(print::info_to_wire(&info))
    }

    fn jobs(&mut self, cx: &Caller) -> Result<Vec<wire::JobInfo>, Fault> {
        Ok(self
            .0
            .jobs(cx.uid)
            .iter()
            .map(print::info_to_wire)
            .collect())
    }
}
