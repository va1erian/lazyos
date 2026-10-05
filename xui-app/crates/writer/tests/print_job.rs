//! A document end to end: LazyWriter's pages, rendered as PWG Raster, go
//! into the print spooler (in process, as on a host run), which sends them
//! to a fake IPP printer as one Create-Job + Send-Document; the test decodes
//! what the printer got.

use std::time::{Duration, Instant};

use ipp::{op, tag};
use printd::fake::{Behaviour, FakePrinter};
use printd::{Local, Queue, Request, Spooler, State, Ticket};
use raster::ColorSpace;
use xui_core::backend::Backend;
use xui_rich_text::Document;
use xui_rich_text::model::PageSetup;
use xui_writer::print::render::{Format, render_page};

#[test]
fn a_document_prints_through_the_spooler() {
    let printer = FakePrinter::start(Behaviour::default());
    let dir = std::env::temp_dir().join(format!("writer-print-job-{}", std::process::id()));
    let queue = Local {
        spooler: Spooler::open(&dir).unwrap(),
        owner: 1000,
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

    let job = queue
        .open(&Request {
            printer: printer.address(),
            user: "alice".into(),
            ticket: Ticket {
                name: "Test document".into(),
                format: "image/pwg-raster".into(),
                copies: Some(2),
                media: Some("iso_a4_210x297mm".into()),
                color_mode: Some("monochrome".into()),
                quality: Some(4),
            },
        })
        .unwrap();
    let format = Format {
        color: ColorSpace::Sgray8,
        media: "iso_a4_210x297mm".into(),
        quality: 4,
        total_pages: pages as u32,
    };
    queue.write(job, raster::SYNC).unwrap();
    for page in 0..pages {
        let mut bytes = Vec::new();
        render_page(&out, page, &format, &mut |b| bytes.extend(b));
        queue.write(job, &bytes).unwrap();
    }
    queue.close(job).unwrap();
    let start = Instant::now();
    let info = loop {
        let info = queue.status(job).unwrap();
        if info.state.is_final() {
            break info;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "stuck: {info:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(info.state, State::Done, "{info:?}");
    let seen = printer.seen();
    assert_eq!(
        &seen.operations[..3],
        [
            op::GET_PRINTER_ATTRIBUTES,
            op::CREATE_JOB,
            op::SEND_DOCUMENT
        ]
    );
    let ticket = seen.ticket.as_ref().unwrap();
    assert_eq!(
        ticket.text(tag::OPERATION, "printer-uri"),
        Some(format!("ipp://{}/ipp/print", printer.address()).as_str())
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
