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

use messenger_generated::os_lazy_print_v1 as wire;
use printd::{JobInfo, Request, Spooler};
use xui_app::platform::print::{self, INTERFACE, MAX_WRITE, NAME};
use xui_app::server::{self, Server};

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
            let method = request.parcel.header.method;
            let reply = match dispatch(&spooler, request.origin.uid, &request.parcel) {
                Ok(body) => server::reply_parcel(INTERFACE, method, body),
                Err(message) => server::error_parcel(INTERFACE, method, 22, &message),
            };
            if let Some(txn) = request.txn {
                let _ = server.reply(txn, &reply);
            }
        }
    }
}

/// One call, as `owner`: the reply body, or the line the caller shows.
fn dispatch(
    spooler: &Spooler,
    owner: u32,
    parcel: &libmessenger::Parcel,
) -> Result<Vec<u8>, String> {
    if parcel.header.interface_id != INTERFACE {
        return Err("Not a print service call".into());
    }
    let bad = |_| "The print request does not read".to_owned();
    let encoded = |body: Result<Vec<u8>, libmessenger::Error>| {
        body.map_err(|_| "The reply could not be encoded".to_owned())
    };
    let body = &parcel.body;
    match parcel.header.method {
        wire::METHOD_OPEN => {
            let args = wire::decode_open_args(body).map_err(bad)?;
            let request = Request {
                printer: args.printer,
                user: args.user,
                ticket: print::ticket_from_wire(args.ticket),
            };
            let job = spooler.open_job(owner, &request)?;
            encoded(wire::encode_open_reply(&wire::OpenReply { job }))
        }
        wire::METHOD_WRITE => {
            let args = wire::decode_write_args(body).map_err(bad)?;
            if args.bytes.len() > MAX_WRITE {
                return Err("A print write is too large".into());
            }
            spooler.write(owner, args.job, &args.bytes)?;
            Ok(Vec::new())
        }
        wire::METHOD_CLOSE => {
            let args = wire::decode_close_args(body).map_err(bad)?;
            spooler.close(owner, args.job)?;
            Ok(Vec::new())
        }
        wire::METHOD_CANCEL => {
            let args = wire::decode_cancel_args(body).map_err(bad)?;
            spooler.cancel(owner, args.job)?;
            Ok(Vec::new())
        }
        wire::METHOD_STATUS => {
            let args = wire::decode_status_args(body).map_err(bad)?;
            let info = spooler.status(owner, args.job)?;
            encoded(wire::encode_status_reply(&wire::StatusReply {
                info: print::info_to_wire(&info),
            }))
        }
        wire::METHOD_JOBS => {
            let jobs = spooler
                .jobs(owner)
                .iter()
                .map(print::info_to_wire)
                .collect();
            encoded(wire::encode_jobs_reply(&wire::JobsReply { jobs }))
        }
        _ => Err("Unknown print service call".into()),
    }
}
