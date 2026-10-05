#![forbid(unsafe_code)]

//! The Print command: the print bar, and a job driven by the window's timer.
//! Each tick renders at most one page and hands it to the job's thread, so
//! the window stays responsive while a long document prints.
//!
//! Serial evidence: `WRITER:PRINT:PASS:<pages>` when the printer reports the
//! job done, `WRITER:PRINT:FAIL:<reason>` otherwise.

use ipp::request::Ticket;
use ipp::uri::PrinterUri;
use raster::ColorSpace;
use xui_core::app::Ui;
use xui_core::backend::TimerId;
use xui_core::widget::HasText;
use xui_rich_text::Printout;

use crate::app::{Msg, Writer};
use crate::names;
use crate::print::job::Job;
use crate::print::render::{DPI, Format, render_page};
use crate::print::{media_name, parse_pages, user_name};

/// How often a running job is ticked.
const TICK_MILLIS: u32 = 50;

/// A job being rendered and sent.
pub struct Session {
    out: Printout,
    /// The pages to print, from 0, and how many are rendered.
    pages: Vec<usize>,
    next: usize,
    format: Format,
    job: Job,
    /// A rendered page the job's queue had no room for yet.
    pending: Option<Vec<u8>>,
    /// Whether the job has been told the document is complete.
    ended: bool,
    timer: TimerId,
    printer: String,
}

/// Print (Ctrl+P): shows the bar with the last printer filled in, or hides it
/// when it is shown and idle.
pub fn toggle_bar(app: &mut Writer, ui: &mut Ui<Msg>) {
    let bar = &app.print_bar;
    if bar.is_shown(ui) && app.printing.is_none() {
        close(app, ui);
        return;
    }
    let printer = bar.printer.get();
    if printer.text().trim().is_empty()
        && let Some(last) = (app.host.last_printer)()
    {
        printer.set_text(&last);
    }
    bar.set_shown(ui, true);
    printer.focus();
}

/// Close: cancels a running job, else hides the bar.
pub fn close(app: &mut Writer, ui: &mut Ui<Msg>) {
    if let Some(session) = &mut app.printing {
        session.job.cancel();
        app.print_bar.set_status("Canceling...");
        return;
    }
    app.print_bar.set_shown(ui, false);
    app.editor.focus();
}

/// The Print button: checks the choices and starts the job.
pub fn start(app: &mut Writer, ui: &mut Ui<Msg>) {
    if app.printing.is_some() {
        return;
    }
    match session(app, ui) {
        Ok(session) => {
            app.print_bar.set_busy(true);
            app.print_bar
                .set_status(&format!("Preparing page 1 of {}...", session.pages.len()));
            app.printing = Some(session);
        }
        Err(error) => app.print_bar.set_status(&error),
    }
}

fn session(app: &Writer, ui: &Ui<Msg>) -> Result<Session, String> {
    let options = app.print_bar.options();
    let printer = PrinterUri::parse(&options.printer)?;
    let out = app.editor.printout(DPI);
    let pages = parse_pages(&options.pages, out.page_count())?;
    let page = app.editor.with_document(|d| *d.page());
    let media = media_name(&page);
    let ticket = Ticket {
        name: names::display_name(app.path.as_deref()),
        format: "image/pwg-raster".into(),
        copies: Some(options.copies as i32),
        media: Some(media.clone()),
        color_mode: Some(if options.grey { "monochrome" } else { "color" }.into()),
        quality: Some(options.quality),
    };
    let job = Job::start(printer, ticket, user_name())?;
    Ok(Session {
        format: Format {
            color: if options.grey {
                ColorSpace::Sgray8
            } else {
                ColorSpace::Srgb8
            },
            media,
            quality: options.quality as u32,
            total_pages: pages.len() as u32,
        },
        out,
        pages,
        next: 0,
        job,
        pending: None,
        ended: false,
        timer: ui.set_timer(TICK_MILLIS),
        printer: options.printer,
    })
}

/// One timer tick: hand over a page, or follow the job once it is all sent.
pub fn tick(app: &mut Writer, ui: &mut Ui<Msg>) {
    let Some(session) = &mut app.printing else {
        return;
    };
    let status = session.job.status();
    if let Some(done) = status.done {
        ui.kill_timer(session.timer);
        let pages = session.pages.len();
        let printer = std::mem::take(&mut session.printer);
        app.printing = None;
        app.print_bar.set_busy(false);
        let ink = if status.ink.is_empty() {
            String::new()
        } else {
            format!(" (ink: {})", status.ink)
        };
        app.print_bar.set_status(&format!("{}{ink}", status.line));
        match done {
            Ok(()) => {
                (app.host.remember_printer)(&printer);
                println!("WRITER:PRINT:PASS:{pages}");
            }
            Err(error) => println!("WRITER:PRINT:FAIL:{error}"),
        }
        return;
    }
    if let Some(bytes) = session.pending.take() {
        session.pending = session.job.offer(bytes);
    } else if session.next < session.pages.len() {
        let page = session.pages[session.next];
        let mut bytes = Vec::new();
        render_page(&session.out, page, &session.format, &mut |b| {
            bytes.extend(b)
        });
        session.next += 1;
        session.pending = session.job.offer(bytes);
        app.print_bar.set_status(&format!(
            "Sending page {} of {}...",
            session.next,
            session.pages.len()
        ));
        return;
    } else if !session.ended {
        session.ended = session.job.end();
    }
    if session.ended {
        app.print_bar.set_status(&status.line);
    }
}
