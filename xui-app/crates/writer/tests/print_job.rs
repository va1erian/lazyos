//! A print job end to end against a fake IPP printer on the loopback: the
//! printer is asked for its state, receives one Print-Job whose document is
//! PWG Raster the test decodes, and is polled until the job completes.

mod common;

use std::io::Read;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ipp::request::Ticket;
use ipp::uri::PrinterUri;
use ipp::{Message, op, tag};
use raster::ColorSpace;
use xui_core::backend::Backend;
use xui_rich_text::Document;
use xui_rich_text::model::PageSetup;
use xui_writer::print::job::{Job, Status};
use xui_writer::print::render::{Format, render_page};

use common::printer::{Seen, fake_printer, read_request, reply};

fn wait(job: &Job) -> Status {
    let start = Instant::now();
    loop {
        let status = job.status();
        if status.done.is_some() {
            return status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "job stuck: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_document_prints_on_a_fake_ipp_printer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let server = {
        let seen = Arc::clone(&seen);
        std::thread::spawn(move || fake_printer(listener, seen))
    };

    let text = "Printed from LazyWriter\n".repeat(60);
    let doc = Document::from_plain_text(&text)
        .with_page(PageSetup::a4())
        .unwrap();
    let shaper = xui_canvas::OffscreenBackend::new().text_shaper();
    // 96 dpi keeps the test quick; the printer path is the same at 300.
    let out = xui_rich_text::Printout::new(&doc, shaper.as_ref(), 96);
    let pages = out.page_count();
    assert!(pages >= 2);

    let printer = PrinterUri::parse(&format!("127.0.0.1:{port}")).unwrap();
    let ticket = Ticket {
        name: "Test document".into(),
        format: "image/pwg-raster".into(),
        copies: Some(2),
        media: Some("iso_a4_210x297mm".into()),
        color_mode: Some("monochrome".into()),
        quality: Some(4),
    };
    let mut job = Job::start(printer, ticket, "alice".into()).unwrap();
    let format = Format {
        color: ColorSpace::Sgray8,
        media: "iso_a4_210x297mm".into(),
        quality: 4,
        total_pages: pages as u32,
    };
    for page in 0..pages {
        let mut bytes = Vec::new();
        render_page(&out, page, &format, &mut |b| bytes.extend(b));
        let mut pending = Some(bytes);
        while let Some(bytes) = pending.take() {
            pending = job.offer(bytes);
            if pending.is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    while !job.end() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = wait(&job);
    server.join().unwrap();

    assert_eq!(status.done, Some(Ok(())), "{status:?}");
    assert_eq!(status.line, "Printed");
    assert_eq!(status.ink, "tri-color 90%, black 50%");
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.operations,
        [
            op::GET_PRINTER_ATTRIBUTES,
            op::PRINT_JOB,
            op::GET_JOB_ATTRIBUTES,
            op::GET_JOB_ATTRIBUTES
        ]
    );
    let ticket = seen.ticket.as_ref().unwrap();
    assert_eq!(
        ticket.text(tag::OPERATION, "printer-uri"),
        Some(format!("ipp://127.0.0.1:{port}/ipp/print").as_str())
    );
    assert_eq!(
        ticket.text(tag::OPERATION, "document-format"),
        Some("image/pwg-raster")
    );
    assert_eq!(ticket.int(tag::JOB, "copies"), Some(2));
    let decoded = raster::decode(&seen.document, 1 << 30).unwrap();
    assert_eq!(decoded.len(), pages);
    for page in &decoded {
        assert_eq!(page.header.color, ColorSpace::Sgray8);
        assert_eq!(page.header.total_pages, pages as u32);
        assert!(page.pixels.iter().any(|&p| p < 128), "every page has text");
    }
}

#[test]
fn an_unreachable_printer_is_reported() {
    // A port nothing listens on: bind one and let it go.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let printer = PrinterUri::parse(&format!("127.0.0.1:{port}")).unwrap();
    let job = Job::start(printer, Ticket::default(), "alice".into()).unwrap();
    let status = wait(&job);
    let error = status.done.unwrap().unwrap_err();
    assert!(
        error.starts_with("Could not reach the printer at 127.0.0.1"),
        "{error}"
    );
}

#[test]
fn a_canceled_job_never_reaches_the_printer_whole() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        // Answer Get-Printer-Attributes, then read the abandoned Print-Job
        // until the client hangs up.
        let (mut first, _) = listener.accept().unwrap();
        let body = read_request(&mut first);
        let (request, _) = Message::decode(&body).unwrap();
        reply(&mut first, &Message::new(0, request.request_id));
        let (mut second, _) = listener.accept().unwrap();
        let mut all = Vec::new();
        let _ = second.read_to_end(&mut all);
        all
    });
    let printer = PrinterUri::parse(&format!("127.0.0.1:{port}")).unwrap();
    let ticket = Ticket {
        name: "x".into(),
        format: "image/pwg-raster".into(),
        ..Ticket::default()
    };
    let mut job = Job::start(printer, ticket, "alice".into()).unwrap();
    assert!(job.offer(vec![1, 2, 3]).is_none());
    job.cancel();
    let status = wait(&job);
    assert_eq!(status.done, Some(Err("Printing canceled".into())));
    let sent = server.join().unwrap();
    assert!(!sent.ends_with(b"0\r\n\r\n"), "the body was never finished");
}
