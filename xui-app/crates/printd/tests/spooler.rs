//! The spooler against the fake printer: a closed job is sent whole as
//! Create-Job + Send-Document, a job never closed never reaches the printer,
//! a cancel mid-send ends in Cancel-Job, and a restart cleans up after a run
//! that died while sending.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ipp::{op, tag};
use printd::fake::{Behaviour, FIRST_JOB, FakePrinter};
use printd::{JobInfo, Local, Queue, Request, Spooler, State, Ticket};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("printd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Dir(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn request(printer: &FakePrinter) -> Request {
    Request {
        printer: printer.address(),
        user: "alice".into(),
        ticket: Ticket {
            name: "Letter".into(),
            format: "image/pwg-raster".into(),
            copies: Some(2),
            media: Some("iso_a4_210x297mm".into()),
            color_mode: Some("monochrome".into()),
            quality: Some(4),
        },
    }
}

fn wait(queue: &dyn Queue, job: u32, until: impl Fn(&JobInfo) -> bool) -> JobInfo {
    let start = Instant::now();
    loop {
        let info = queue.status(job).unwrap();
        if until(&info) {
            return info;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "stuck: {info:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn done(queue: &dyn Queue, job: u32) -> JobInfo {
    wait(queue, job, |i| i.state.is_final())
}

fn local(dir: &Dir, owner: u32) -> Local {
    Local {
        spooler: Spooler::open(&dir.0).unwrap(),
        owner,
    }
}

#[test]
fn a_closed_job_is_sent_whole_and_followed_to_the_end() {
    let printer = FakePrinter::start(Behaviour::default());
    let dir = Dir::new("whole");
    let queue = local(&dir, 1000);
    let job = queue.open(&request(&printer)).unwrap();
    queue.write(job, b"RaS2").unwrap();
    queue.write(job, &[7u8; 200_000]).unwrap();
    // Nothing goes out before Close.
    std::thread::sleep(Duration::from_millis(200));
    assert!(printer.seen().operations.is_empty());
    queue.close(job).unwrap();
    let info = done(&queue, job);
    assert_eq!(info.state, State::Done, "{info:?}");
    assert_eq!(info.line, "Printed");
    assert_eq!(info.ink, "tri-color 90%, black 50%");

    let seen = printer.seen();
    assert_eq!(
        seen.operations,
        [
            op::GET_PRINTER_ATTRIBUTES,
            op::CREATE_JOB,
            op::SEND_DOCUMENT,
            op::GET_JOB_ATTRIBUTES,
            op::GET_JOB_ATTRIBUTES
        ]
    );
    let created = seen.ticket.clone().unwrap();
    assert_eq!(
        created.text(tag::OPERATION, "requesting-user-name"),
        Some("alice")
    );
    assert_eq!(created.int(tag::JOB, "copies"), Some(2));
    let send = seen.send.clone().unwrap();
    assert_eq!(send.int(tag::OPERATION, "job-id"), Some(FIRST_JOB));
    assert_eq!(
        send.text(tag::OPERATION, "document-format"),
        Some("image/pwg-raster")
    );
    assert_eq!(seen.document.len(), 200_004);
    assert!(seen.document.starts_with(b"RaS2"));
    assert_eq!(seen.truncated, 0);
    assert!(seen.open_jobs().is_empty());
    // The document is gone from the spool once sent.
    assert!(!dir.0.join(format!("{job}.doc")).exists());
}

#[test]
fn a_job_never_closed_never_reaches_the_printer() {
    let printer = FakePrinter::start(Behaviour::default());
    let dir = Dir::new("unclosed");
    let queue = local(&dir, 1000);
    let job = queue.open(&request(&printer)).unwrap();
    queue.write(job, b"RaS2 half a page").unwrap();
    queue.cancel(job).unwrap();
    assert_eq!(queue.status(job).unwrap().state, State::Canceled);
    // A second job left open when its app dies is dropped on restart.
    let other = queue.open(&request(&printer)).unwrap();
    queue.write(other, b"RaS2").unwrap();
    drop(queue);
    let queue = local(&dir, 1000);
    assert!(queue.status(other).is_err());
    std::thread::sleep(Duration::from_millis(300));
    assert!(printer.seen().operations.is_empty());
}

#[test]
fn canceling_mid_send_cancels_the_job_at_the_printer() {
    let printer = FakePrinter::start(Behaviour {
        stall_after: Some(64 * 1024),
        ..Behaviour::default()
    });
    let dir = Dir::new("cancel");
    let queue = local(&dir, 1000);
    let job = queue.open(&request(&printer)).unwrap();
    queue.write(job, &vec![1u8; 4 * 1024 * 1024]).unwrap();
    queue.close(job).unwrap();
    wait(&queue, job, |_| printer.seen().document_bytes >= 64 * 1024);
    queue.cancel(job).unwrap();
    printer.release();
    let info = done(&queue, job);
    assert_eq!(info.state, State::Canceled, "{info:?}");
    let seen = printer.seen();
    assert_eq!(seen.canceled, [FIRST_JOB]);
    assert!(seen.open_jobs().is_empty());
}

#[test]
fn a_refused_document_cancels_the_created_job() {
    let printer = FakePrinter::start(Behaviour {
        refuse_document: true,
        ..Behaviour::default()
    });
    let dir = Dir::new("refused");
    let queue = local(&dir, 1000);
    let job = queue.open(&request(&printer)).unwrap();
    queue.write(job, b"RaS2").unwrap();
    queue.close(job).unwrap();
    let info = done(&queue, job);
    assert_eq!(info.state, State::Failed);
    assert!(
        info.line.starts_with("The printer refused the job"),
        "{}",
        info.line
    );
    assert_eq!(printer.seen().canceled, [FIRST_JOB]);
}

#[test]
fn a_restart_while_sending_cancels_the_job_at_the_printer() {
    let printer = FakePrinter::start(Behaviour::default());
    let dir = Dir::new("restart");
    std::fs::create_dir_all(&dir.0).unwrap();
    // What a run that died mid-send leaves: the printer's job number and
    // half a document.
    std::fs::write(
        dir.0.join("4.job"),
        format!(
            "owner=1000\nstate=sending\nprinter=ipp://{}/ipp/print\nuser=alice\n\
             name=Letter\nformat=image/pwg-raster\nprinter-job=31\nline=Sending\n",
            printer.address()
        ),
    )
    .unwrap();
    std::fs::write(dir.0.join("4.doc"), b"RaS2 half").unwrap();
    let queue = local(&dir, 1000);
    let info = queue.status(4).unwrap();
    assert_eq!(info.state, State::Failed);
    let start = Instant::now();
    while printer.seen().canceled != [31] {
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!dir.0.join("4.doc").exists());
    // New jobs get numbers after the old ones.
    let job = queue.open(&request(&printer)).unwrap();
    assert_eq!(job, 5);
}

#[test]
fn a_queued_job_survives_a_restart_and_is_sent() {
    let printer = FakePrinter::start(Behaviour::default());
    let dir = Dir::new("queued");
    std::fs::create_dir_all(&dir.0).unwrap();
    std::fs::write(
        dir.0.join("2.job"),
        format!(
            "owner=1000\nstate=queued\nprinter=ipp://{}/ipp/print\nuser=alice\n\
             name=Letter\nformat=image/pwg-raster\nline=Waiting\n",
            printer.address()
        ),
    )
    .unwrap();
    std::fs::write(dir.0.join("2.doc"), b"RaS2 whole").unwrap();
    let queue = local(&dir, 1000);
    let info = done(&queue, 2);
    assert_eq!(info.state, State::Done, "{info:?}");
    assert_eq!(printer.seen().document, b"RaS2 whole");
}

#[test]
fn jobs_answer_only_to_their_owner_and_root() {
    let printer = FakePrinter::start(Behaviour::default());
    let dir = Dir::new("owner");
    let spooler = Spooler::open(&dir.0).unwrap();
    let alice = Local {
        spooler: Arc::clone(&spooler),
        owner: 1000,
    };
    let job = alice.open(&request(&printer)).unwrap();
    let error = "There is no such print job";
    assert_eq!(spooler.status(1001, job).unwrap_err(), error);
    assert_eq!(spooler.write(1001, job, b"x").unwrap_err(), error);
    assert_eq!(spooler.cancel(1001, job).unwrap_err(), error);
    assert_eq!(spooler.status(1000, job).unwrap().state, State::Open);
    assert_eq!(spooler.status(0, job).unwrap().state, State::Open);
    assert!(spooler.jobs(1001).is_empty());
    assert_eq!(spooler.jobs(1000).len(), 1);
}

#[test]
fn bad_requests_and_limits_are_refused() {
    let printer = FakePrinter::start(Behaviour::default());
    let dir = Dir::new("limits");
    let queue = local(&dir, 1000);
    let mut bad = request(&printer);
    bad.printer = "ipps://printer".into();
    assert!(queue.open(&bad).is_err());
    let mut bad = request(&printer);
    bad.ticket.copies = Some(0);
    assert!(queue.open(&bad).is_err());
    let mut bad = request(&printer);
    bad.ticket.name = "x".repeat(300);
    assert!(queue.open(&bad).is_err());
    let job = queue.open(&request(&printer)).unwrap();
    assert!(queue.close(job).is_err(), "an empty document");
    for _ in 1..printd::MAX_OPEN_PER_OWNER {
        queue.open(&request(&printer)).unwrap();
    }
    assert!(queue.open(&request(&printer)).is_err());
    queue.write(job, b"RaS2").unwrap();
    queue.close(job).unwrap();
    assert!(
        queue.write(job, b"more").is_err(),
        "a closed job takes no more"
    );
}
