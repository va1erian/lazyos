#![forbid(unsafe_code)]

//! A print job on a worker thread: it checks the printer, streams the pages
//! it is handed as one `Print-Job`, then follows the job until the printer
//! says it is done. The UI thread only hands over bytes and reads the status;
//! the thread touches nothing but its sockets and the shared values below.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ipp::http::{self, Post};
use ipp::request::{self, Client, JobStatus, Ticket};
use ipp::uri::PrinterUri;
use ipp::{Message, job_state, status_name, tag};

/// How long to wait for a connection, and for each read or write.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Pages buffered between the renderer and the connection.
const QUEUE: usize = 3;
/// How often the job's state is asked for once the document is sent.
const POLL: Duration = Duration::from_secs(2);
/// How long a sent job is followed before the client stops asking.
const FOLLOW: Duration = Duration::from_secs(600);

/// What the job is doing, for the print bar's status line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// One line for the user: what is happening, in the printer's words when
    /// it gave any.
    pub line: String,
    /// `Some` once the job is over: `Ok` when the printer finished it.
    pub done: Option<Result<(), String>>,
    /// Ink levels, `tri-color 90%, black 50%`, once known.
    pub ink: String,
}

/// What the renderer hands the connection.
enum Piece {
    Bytes(Vec<u8>),
    /// The document is complete.
    End,
}

/// A job in flight.
pub struct Job {
    tx: Option<SyncSender<Piece>>,
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    /// Starts a job for `ticket` on `printer`. The document's bytes follow
    /// through [`Job::offer`] and [`Job::end`].
    pub fn start(printer: PrinterUri, ticket: Ticket, user: String) -> Result<Job, String> {
        let (tx, rx) = sync_channel(QUEUE);
        let status = Arc::new(Mutex::new(Status {
            line: format!("Connecting to {}...", printer.host),
            ..Status::default()
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            printer,
            ticket,
            user,
            rx,
            status: Arc::clone(&status),
            cancel: Arc::clone(&cancel),
        };
        std::thread::Builder::new()
            .name("print".into())
            .spawn(move || worker.run())
            .map_err(|e| format!("could not start the print thread: {e}"))?;
        Ok(Job {
            tx: Some(tx),
            status,
            cancel,
        })
    }

    /// Hands over the next document bytes. Gives them back when the queue is
    /// full (try again later); drops them when the job has already ended.
    pub fn offer(&self, bytes: Vec<u8>) -> Option<Vec<u8>> {
        let tx = self.tx.as_ref()?;
        match tx.try_send(Piece::Bytes(bytes)) {
            Ok(()) => None,
            Err(TrySendError::Full(Piece::Bytes(bytes))) => Some(bytes),
            Err(_) => None,
        }
    }

    /// Says the document is complete; `false` when the queue is full (try
    /// again later).
    pub fn end(&mut self) -> bool {
        let Some(tx) = &self.tx else {
            return true;
        };
        match tx.try_send(Piece::End) {
            Err(TrySendError::Full(_)) => false,
            _ => {
                self.tx = None;
                true
            }
        }
    }

    /// Stops the job: an unsent document is abandoned (the printer drops an
    /// incomplete request), a sent one is cancelled on the printer.
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.tx = None;
    }

    /// The job's status now.
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

struct Worker {
    printer: PrinterUri,
    ticket: Ticket,
    user: String,
    rx: Receiver<Piece>,
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
}

impl Worker {
    fn run(self) {
        let outcome = self.print();
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.line = match &outcome {
            Ok(line) => line.clone(),
            Err(error) => error.clone(),
        };
        status.done = Some(outcome.map(|_| ()));
    }

    fn set(&self, line: String) {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).line = line;
    }

    fn connect(&self) -> io::Result<TcpStream> {
        let host = self.printer.connect_host();
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
        for addr in (host, self.printer.port).to_socket_addrs()? {
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
        let (host, path) = (self.printer.host_header(), &self.printer.path);
        http::exchange(&mut stream, &host, path, request)
    }

    fn print(&self) -> Result<String, String> {
        let uri = self.printer.to_ipp();
        let client = Client {
            printer_uri: &uri,
            user: &self.user,
        };
        let unreachable =
            |e: io::Error| format!("Could not reach the printer at {}: {e}", self.printer.host);
        let info = self
            .ask(&request::get_printer_attributes(
                &client,
                1,
                request::STATUS_ATTRIBUTES,
            ))
            .map_err(unreachable)?;
        self.note_printer(&info);

        self.set("Sending the pages...".into());
        let reply = self.send(&client).map_err(|e| match e {
            Sent::Canceled => "Printing canceled".to_owned(),
            Sent::Io(e) => format!("The printer stopped taking the document: {e}"),
        })?;
        if !reply.is_success() {
            return Err(refusal(&reply));
        }
        let job = JobStatus::of(&reply);
        let Some(id) = job.id else {
            return Ok("Sent to the printer".into());
        };
        self.follow(&client, id, job)
    }

    /// Ink levels and the printer's own state message.
    fn note_printer(&self, info: &Message) {
        let ink = request::markers(info)
            .iter()
            .map(|m| {
                // "black ink" reads as "black" after "Ink:".
                let name = m.name.strip_suffix(" ink").unwrap_or(&m.name);
                match m.level {
                    Some(level) => format!("{name} {level}%"),
                    None => name.to_owned(),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.status.lock().unwrap_or_else(|e| e.into_inner()).ink = ink;
    }

    /// The Print-Job request with the document streamed after it.
    fn send(&self, client: &Client) -> Result<Message, Sent> {
        let ticket = request::print_job(client, 2, &self.ticket)
            .encode()
            .map_err(|e| Sent::Io(io::Error::other(format!("{e:?}"))))?;
        let mut stream = self.connect().map_err(Sent::Io)?;
        let host = self.printer.host_header();
        let mut post = Post::start(&mut stream, &host, &self.printer.path).map_err(Sent::Io)?;
        post.write(&ticket).map_err(Sent::Io)?;
        post.write(raster::SYNC).map_err(Sent::Io)?;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(Sent::Canceled);
            }
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Piece::Bytes(bytes)) => post.write(&bytes).map_err(Sent::Io)?,
                Ok(Piece::End) => break,
                Err(RecvTimeoutError::Timeout) => continue,
                // The renderer went away without ending the document.
                Err(RecvTimeoutError::Disconnected) => return Err(Sent::Canceled),
            }
        }
        post.finish().map_err(Sent::Io)?;
        http::read_reply(&mut stream)
            .and_then(|body| http::decode_reply(&body))
            .map_err(Sent::Io)
    }

    /// Asks for the job's state until it ends, cancelling it if asked to.
    fn follow(&self, client: &Client, id: i32, mut job: JobStatus) -> Result<String, String> {
        let started = Instant::now();
        let mut request_id = 3;
        loop {
            if let Some(state) = job.state {
                if job_state::is_final(state) {
                    return finished(state, &job);
                }
                self.set(describe(&job));
            }
            if self.cancel.load(Ordering::Relaxed) {
                let _ = self.ask(&request::cancel_job(client, request_id, id));
                return Err("Printing canceled".into());
            }
            if started.elapsed() > FOLLOW {
                return Ok("Sent; the printer is still working on it".into());
            }
            std::thread::sleep(POLL);
            request_id += 1;
            match self.ask(&request::get_job_attributes(client, request_id, id)) {
                Ok(reply) if reply.is_success() => job = JobStatus::of(&reply),
                // The printer forgets finished jobs after a while.
                Ok(_) => return Ok("Printed".into()),
                Err(e) => self.set(format!("Printing (no answer from the printer: {e})")),
            }
        }
    }
}

enum Sent {
    Canceled,
    Io(io::Error),
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

fn finished(state: i32, job: &JobStatus) -> Result<String, String> {
    match state {
        job_state::COMPLETED => Ok("Printed".into()),
        job_state::CANCELED => Err("The job was canceled at the printer".into()),
        _ => Err(match &job.message {
            Some(message) => format!("The printer gave up: {message}"),
            None => format!("The printer gave up ({})", job.reasons.join(", ")),
        }),
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
