//! A fake IPP printer on the loopback, for the spooler's and LazyWriter's
//! tests (feature `fake`). It answers Get-Printer-Attributes with two ink
//! levels, Create-Job with a job number, keeps the document Send-Document
//! brings, reports the job processing and then completed, and honours
//! Cancel-Job. Each connection is served on its own thread, as a printer
//! takes a Cancel-Job while a document is still arriving.
//!
//! It also records what a real printer suffers from: a request whose body
//! ended before its last chunk ([`Seen::truncated`]) and a job created but
//! never given its document nor canceled ([`Seen::open_jobs`]).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ipp::{Attribute, Group, Message, Value, job_state, op, tag};

/// The first job number the fake gives.
pub const FIRST_JOB: i32 = 7;

/// How the fake behaves.
#[derive(Clone, Debug)]
pub struct Behaviour {
    /// Get-Job-Attributes answers before the job reads completed.
    pub polls: u32,
    /// Stop reading a document after this many bytes until released with
    /// [`FakePrinter::release`], as a printer whose buffer is full does.
    pub stall_after: Option<usize>,
    /// Refuse Send-Document (the printer did not like the document).
    pub refuse_document: bool,
}

impl Default for Behaviour {
    fn default() -> Behaviour {
        Behaviour {
            polls: 2,
            stall_after: None,
            refuse_document: false,
        }
    }
}

/// What the fake printer saw.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    /// Every operation, in arrival order.
    pub operations: Vec<u16>,
    /// The last Create-Job request.
    pub ticket: Option<Message>,
    /// The last Send-Document request.
    pub send: Option<Message>,
    /// The last whole document.
    pub document: Vec<u8>,
    /// Jobs canceled with Cancel-Job.
    pub canceled: Vec<i32>,
    /// Requests whose body was cut off.
    pub truncated: usize,
    /// Bytes of document received so far, whole or not.
    pub document_bytes: usize,
    created: Vec<i32>,
    finished: Vec<i32>,
    polls: u32,
}

impl Seen {
    /// Jobs created and neither given their whole document nor canceled:
    /// what a real printer would be left waiting on.
    pub fn open_jobs(&self) -> Vec<i32> {
        self.created
            .iter()
            .copied()
            .filter(|j| !self.finished.contains(j) && !self.canceled.contains(j))
            .collect()
    }
}

struct Shared {
    seen: Mutex<Seen>,
    behaviour: Behaviour,
    released: AtomicBool,
    stop: AtomicBool,
}

/// A running fake printer; it stops when dropped.
pub struct FakePrinter {
    pub port: u16,
    shared: Arc<Shared>,
}

impl FakePrinter {
    pub fn start(behaviour: Behaviour) -> FakePrinter {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = Arc::new(Shared {
            seen: Mutex::new(Seen::default()),
            behaviour,
            released: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let serving = Arc::clone(&shared);
        std::thread::spawn(move || {
            while !serving.stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shared = Arc::clone(&serving);
                        std::thread::spawn(move || serve(stream, &shared));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        FakePrinter { port, shared }
    }

    /// The address to print to.
    pub fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// What it saw so far.
    pub fn seen(&self) -> Seen {
        self.shared.seen.lock().unwrap().clone()
    }

    /// Lets a stalled document go on.
    pub fn release(&self) {
        self.shared.released.store(true, Ordering::Relaxed);
    }
}

impl Drop for FakePrinter {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.release();
    }
}

/// Reads one request's head and chunked body; `None` when the body was cut
/// off.
fn read_request(stream: &TcpStream, shared: &Shared) -> Option<Vec<u8>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line == "\r\n" {
            break;
        }
    }
    let mut body = Vec::new();
    let mut head_end = None;
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let size = usize::from_str_radix(line.trim(), 16).ok()?;
        if size == 0 {
            reader.read_line(&mut line).ok()?;
            return Some(body);
        }
        let mut chunk = vec![0; size + 2];
        reader.read_exact(&mut chunk).ok()?;
        body.extend_from_slice(&chunk[..size]);
        if head_end.is_none() {
            head_end = Message::decode(&body).ok().map(|(m, end)| (m.code, end));
        }
        if let Some((op::SEND_DOCUMENT, end)) = head_end {
            let document = body.len() - end;
            shared.seen.lock().unwrap().document_bytes = document;
            if let Some(limit) = shared.behaviour.stall_after {
                while document >= limit
                    && !shared.released.load(Ordering::Relaxed)
                    && !shared.stop.load(Ordering::Relaxed)
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
}

fn reply(mut stream: &TcpStream, message: &Message) {
    let body = message.encode().unwrap();
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
}

fn job_group(id: i32, state: i32) -> Group {
    let mut job = Group::new(tag::JOB);
    job.attributes
        .push(Attribute::new("job-id", Value::Integer(id)));
    job.attributes
        .push(Attribute::new("job-state", Value::Enum(state)));
    job
}

fn serve(stream: TcpStream, shared: &Shared) {
    let Some(body) = read_request(&stream, shared) else {
        shared.seen.lock().unwrap().truncated += 1;
        return;
    };
    let Ok((request, end)) = Message::decode(&body) else {
        return;
    };
    let mut seen = shared.seen.lock().unwrap();
    seen.operations.push(request.code);
    let job_id = request.int(tag::OPERATION, "job-id").unwrap_or(0);
    let mut answer = Message::new(0, request.request_id);
    match request.code {
        op::GET_PRINTER_ATTRIBUTES => {
            let mut printer = Group::new(tag::PRINTER);
            printer.attributes.push(Attribute::set(
                "marker-names",
                vec![Value::name("tri-color ink"), Value::name("black ink")],
            ));
            printer.attributes.push(Attribute::set(
                "marker-levels",
                vec![Value::Integer(90), Value::Integer(50)],
            ));
            answer.groups.push(printer);
        }
        op::CREATE_JOB => {
            let id = FIRST_JOB + seen.created.len() as i32;
            seen.created.push(id);
            seen.ticket = Some(request);
            answer.groups.push(job_group(id, job_state::PENDING));
        }
        op::SEND_DOCUMENT if shared.behaviour.refuse_document => {
            answer.code = 0x040A;
        }
        op::SEND_DOCUMENT => {
            seen.document = body[end..].to_vec();
            seen.finished.push(job_id);
            seen.send = Some(request);
            answer
                .groups
                .push(job_group(job_id, job_state::PROCESSING));
        }
        op::GET_JOB_ATTRIBUTES => {
            seen.polls += 1;
            let state = if seen.canceled.contains(&job_id) {
                job_state::CANCELED
            } else if seen.polls < shared.behaviour.polls {
                job_state::PROCESSING
            } else {
                job_state::COMPLETED
            };
            answer.groups.push(job_group(job_id, state));
        }
        op::CANCEL_JOB => seen.canceled.push(job_id),
        _ => answer.code = 0x0501,
    }
    drop(seen);
    reply(&stream, &answer);
}
