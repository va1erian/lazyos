//! Sending one job: the printer's state and ink, Create-Job, Send-Document
//! streaming the spooled document, then Get-Job-Attributes until the printer
//! says the job is over.
//!
//! Once Create-Job has answered, the printer holds a job by its id, and any
//! way this ends short of the whole document (a cancel, a dropped
//! connection, a refused Send-Document) sends Cancel-Job for that id, so the
//! printer never waits on, or prints, half a document.

use std::fs::File;
use std::io::{self, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ipp::http::{self, Post};
use ipp::request::{self, Client, JobStatus};
use ipp::uri::PrinterUri;
use ipp::{Message, job_state, status_name, tag};

use crate::spooler::{Orphan, Spooler};
use crate::{JobId, State, Ticket};

/// How long to wait for a connection, and for each read or write.
const TIMEOUT: Duration = Duration::from_secs(30);
/// How much of the document goes in one chunk.
const CHUNK: usize = 64 * 1024;
/// How often the job's state is asked for once the document is sent.
const POLL: Duration = Duration::from_secs(2);
/// How long a sent job is followed before the spooler stops asking.
const FOLLOW: Duration = Duration::from_secs(600);
/// Unanswered state requests in a row after which the job is given up on.
const GIVE_UP: u32 = 5;

/// A queued job, as the sending thread takes it.
pub(crate) struct Order {
    pub id: JobId,
    /// `ipp://host:port/path`, checked when the job was opened.
    pub printer: String,
    pub user: String,
    pub ticket: Ticket,
    pub document: PathBuf,
    pub cancel: Arc<AtomicBool>,
}

/// The printer one job talks to.
struct Printer {
    uri: PrinterUri,
    ipp: String,
    user: String,
}

impl Printer {
    fn new(printer: &str, user: &str) -> Result<Printer, String> {
        let uri = PrinterUri::parse(printer)?;
        Ok(Printer {
            ipp: uri.to_ipp(),
            uri,
            user: user.to_owned(),
        })
    }

    fn client(&self) -> Client<'_> {
        Client {
            printer_uri: &self.ipp,
            user: &self.user,
        }
    }

    fn connect(&self) -> io::Result<TcpStream> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
        for addr in (self.uri.connect_host(), self.uri.port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, TIMEOUT) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(TIMEOUT))?;
                    stream.set_write_timeout(Some(TIMEOUT))?;
                    return Ok(stream);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// One request with no document, on its own connection.
    fn ask(&self, request: &Message) -> io::Result<Message> {
        let mut stream = self.connect()?;
        http::exchange(&mut stream, &self.uri.host_header(), &self.uri.path, request)
    }

    /// Cancel-Job, best effort: the printer may have ended the job already.
    fn cancel(&self, request_id: u32, printer_job: i32) {
        let _ = self.ask(&request::cancel_job(&self.client(), request_id, printer_job));
    }
}

/// Cancels a job an earlier run left at its printer.
pub(crate) fn cancel_orphan(orphan: &Orphan) {
    if let Ok(printer) = Printer::new(&orphan.printer, &orphan.user) {
        printer.cancel(1, orphan.printer_job);
    }
}

/// How the document stopped going out.
enum Stop {
    Canceled,
    Io(io::Error),
}

/// Sends `order` and follows it to its end: the job's final state and line.
pub(crate) fn run(spooler: &Spooler, order: &Order) -> (State, String) {
    let printer = match Printer::new(&order.printer, &order.user) {
        Ok(printer) => printer,
        Err(error) => return (State::Failed, error),
    };
    let run = Run {
        spooler,
        order,
        printer,
    };
    match run.send() {
        Ok(line) => (State::Done, line),
        Err((state, line)) => (state, line),
    }
}

struct Run<'a> {
    spooler: &'a Spooler,
    order: &'a Order,
    printer: Printer,
}

type Outcome = Result<String, (State, String)>;

impl Run<'_> {
    fn canceled(&self) -> bool {
        self.order.cancel.load(Ordering::Relaxed)
    }

    fn line(&self, line: String) {
        self.spooler.update(self.order.id, false, |record, _| record.line = line);
    }

    fn send(&self) -> Outcome {
        let host = &self.printer.uri.host;
        let unreachable = |e: io::Error| {
            (
                State::Failed,
                format!("Could not reach the printer at {host}: {e}"),
            )
        };
        let client = self.printer.client();
        let info = self
            .printer
            .ask(&request::get_printer_attributes(
                &client,
                1,
                request::STATUS_ATTRIBUTES,
            ))
            .map_err(unreachable)?;
        let ink = ink(&info);
        self.spooler.update(self.order.id, false, |_, i| *i = ink);
        if self.canceled() {
            return Err((State::Canceled, "Printing canceled".into()));
        }

        let created = self
            .printer
            .ask(&request::create_job(&client, 2, &self.order.ticket))
            .map_err(unreachable)?;
        if !created.is_success() {
            return Err((State::Failed, refusal(&created)));
        }
        let Some(printer_job) = JobStatus::of(&created).id else {
            return Err((State::Failed, "The printer gave the job no number".into()));
        };
        // Recorded before any page leaves, so a restart can cancel it.
        self.spooler.update(self.order.id, true, |record, _| {
            record.printer_job = Some(printer_job);
            record.line = "Sending the pages...".into();
        });

        let reply = match self.send_document(&client, printer_job) {
            Ok(reply) => reply,
            Err(stop) => {
                self.printer.cancel(4, printer_job);
                return Err(match stop {
                    Stop::Canceled => (State::Canceled, "Printing canceled".into()),
                    Stop::Io(e) => (
                        State::Failed,
                        format!("The printer stopped taking the document: {e}"),
                    ),
                });
            }
        };
        if !reply.is_success() {
            self.printer.cancel(4, printer_job);
            return Err((State::Failed, refusal(&reply)));
        }
        self.spooler.update(self.order.id, true, |record, _| {
            record.state = State::Printing;
            record.line = "Printing...".into();
        });
        self.follow(&client, printer_job, JobStatus::of(&reply))
    }

    /// Send-Document with the spooled document streamed after it.
    fn send_document(&self, client: &Client, printer_job: i32) -> Result<Message, Stop> {
        let format = &self.order.ticket.format;
        let head = request::send_document(client, 3, printer_job, format, true)
            .encode()
            .map_err(|e| Stop::Io(io::Error::other(format!("{e:?}"))))?;
        let mut document = File::open(&self.order.document).map_err(Stop::Io)?;
        let mut stream = self.printer.connect().map_err(Stop::Io)?;
        let uri = &self.printer.uri;
        let mut post = Post::start(&mut stream, &uri.host_header(), &uri.path).map_err(Stop::Io)?;
        post.write(&head).map_err(Stop::Io)?;
        let mut chunk = vec![0u8; CHUNK];
        loop {
            if self.canceled() {
                return Err(Stop::Canceled);
            }
            let n = document.read(&mut chunk).map_err(Stop::Io)?;
            if n == 0 {
                break;
            }
            post.write(&chunk[..n]).map_err(Stop::Io)?;
        }
        post.finish().map_err(Stop::Io)?;
        http::read_reply(&mut stream)
            .and_then(|body| http::decode_reply(&body))
            .map_err(Stop::Io)
    }

    /// Asks for the job's state until it ends, canceling it if asked to.
    fn follow(&self, client: &Client, printer_job: i32, mut job: JobStatus) -> Outcome {
        let started = Instant::now();
        let mut request_id = 5;
        let mut unanswered = 0;
        loop {
            if let Some(state) = job.state {
                if job_state::is_final(state) {
                    return finished(state, &job);
                }
                self.line(describe(&job));
            }
            if self.canceled() {
                self.printer.cancel(request_id, printer_job);
                return Err((State::Canceled, "Printing canceled".into()));
            }
            if started.elapsed() > FOLLOW {
                return Ok("Sent; the printer is still working on it".into());
            }
            self.sleep(POLL);
            request_id += 1;
            match self
                .printer
                .ask(&request::get_job_attributes(client, request_id, printer_job))
            {
                Ok(reply) if reply.is_success() => {
                    unanswered = 0;
                    job = JobStatus::of(&reply);
                }
                // The printer forgets finished jobs after a while.
                Ok(_) => return Ok("Printed".into()),
                Err(e) if unanswered + 1 >= GIVE_UP => {
                    return Err((State::Failed, format!("The printer stopped answering: {e}")));
                }
                Err(e) => {
                    unanswered += 1;
                    self.line(format!("Printing (no answer from the printer: {e})"));
                }
            }
        }
    }

    /// Waits `time`, waking early for a cancel.
    fn sleep(&self, time: Duration) {
        let end = Instant::now() + time;
        while !self.canceled() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Ink levels from a Get-Printer-Attributes reply.
fn ink(info: &Message) -> String {
    request::markers(info)
        .iter()
        .map(|m| {
            // "black ink" reads as "black" after "ink:".
            let name = m.name.strip_suffix(" ink").unwrap_or(&m.name);
            match m.level {
                Some(level) => format!("{name} {level}%"),
                None => name.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The status line for a job still going.
fn describe(job: &JobStatus) -> String {
    let state = job.state.map_or("waiting", job_state::name);
    let state = match state {
        "printing" => "Printing",
        "pending" => "Waiting for the printer",
        other => other,
    };
    match (&job.message, job.reasons.first()) {
        (Some(message), _) => format!("{state}: {message}"),
        (None, Some(reason)) if reason != "job-printing" => format!("{state} ({reason})"),
        _ => format!("{state}..."),
    }
}

fn finished(state: i32, job: &JobStatus) -> Outcome {
    match state {
        job_state::COMPLETED => Ok("Printed".into()),
        job_state::CANCELED => Err((State::Canceled, "The job was canceled at the printer".into())),
        _ => Err((
            State::Failed,
            match &job.message {
                Some(message) => format!("The printer gave up: {message}"),
                None => format!("The printer gave up ({})", job.reasons.join(", ")),
            },
        )),
    }
}

/// Why the printer refused a request, in its words when it gave any.
fn refusal(reply: &Message) -> String {
    let detail = reply
        .text(tag::OPERATION, "status-message")
        .filter(|m| !m.is_empty())
        .map_or_else(|| status_name(reply.code).to_owned(), str::to_owned);
    format!("The printer refused the job: {detail}")
}
