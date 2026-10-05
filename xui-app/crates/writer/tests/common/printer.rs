//! A fake IPP printer on the loopback, for the print tests: it answers
//! Get-Printer-Attributes with two ink levels, keeps the Print-Job request
//! and its document, and reports the job processing, then completed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use ipp::{Attribute, Group, Message, Value, job_state, op, tag};

/// What the fake printer saw.
#[derive(Default)]
pub struct Seen {
    pub operations: Vec<u16>,
    pub ticket: Option<Message>,
    pub document: Vec<u8>,
}

/// Reads one HTTP request with a chunked body; returns the body.
pub fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
    }
    let mut body = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let size = usize::from_str_radix(line.trim(), 16).unwrap();
        if size == 0 {
            reader.read_line(&mut line).unwrap();
            return body;
        }
        let mut chunk = vec![0; size + 2];
        reader.read_exact(&mut chunk).unwrap();
        body.extend_from_slice(&chunk[..size]);
    }
}

pub fn reply(stream: &mut TcpStream, message: &Message) {
    let body = message.encode().unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
}

fn job_group(state: i32) -> Group {
    let mut job = Group::new(tag::JOB);
    job.attributes
        .push(Attribute::new("job-id", Value::Integer(7)));
    job.attributes
        .push(Attribute::new("job-state", Value::Enum(state)));
    job
}

/// Serves until the job has been polled to completion.
pub fn fake_printer(listener: TcpListener, seen: Arc<Mutex<Seen>>) {
    let mut polls = 0;
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let body = read_request(&mut stream);
        let (request, end) = Message::decode(&body).unwrap();
        let mut seen = seen.lock().unwrap();
        seen.operations.push(request.code);
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
            op::PRINT_JOB => {
                seen.document = body[end..].to_vec();
                seen.ticket = Some(request);
                answer.groups.push(job_group(job_state::PENDING));
            }
            op::GET_JOB_ATTRIBUTES => {
                polls += 1;
                answer.groups.push(job_group(if polls < 2 {
                    job_state::PROCESSING
                } else {
                    job_state::COMPLETED
                }));
            }
            other => panic!("unexpected operation {other:#x}"),
        }
        drop(seen);
        reply(&mut stream, &answer);
        if polls >= 2 {
            return;
        }
    }
}
