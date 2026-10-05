#![forbid(unsafe_code)]

//! The Print command: the print bar, and a job driven by the window's timer.
//! Each tick renders at most one page into the print spooler
//! ([`Host::print_queue`](crate::Host)), so the window stays responsive while
//! a long document prints. Once every page is there the job is closed and
//! the spooler owns it: the app may quit and the printer still gets the
//! whole document. Until then nothing has reached the printer, and quitting
//! asks first (Stop printing / Keep printing).
//!
//! Serial evidence: `WRITER:PRINT:QUEUED:<pages>` when the spooler has the
//! whole document, then `WRITER:PRINT:PASS:<pages>` when the printer reports
//! the job done, `WRITER:PRINT:FAIL:<reason>` otherwise.

use std::rc::Rc;

use printd::{JobId, Queue, Request, State, Ticket};
use raster::ColorSpace;
use xui_core::app::Ui;
use xui_core::backend::TimerId;
use xui_core::widget::HasText;
use xui_rich_text::Printout;

use crate::app::{Msg, Writer};
use crate::names;
use crate::print::render::{DPI, Format, render_page};
use crate::print::{media_name, parse_pages, user_name};

/// How often a running job is ticked.
const TICK_MILLIS: u32 = 50;
/// Ticks between two questions to the spooler once the job is queued.
const STATUS_TICKS: u32 = 10;

/// A job being rendered into the spooler, then followed there.
pub struct Session {
    out: Printout,
    /// The pages to print, from 0, and how many are rendered.
    pages: Vec<usize>,
    next: usize,
    format: Format,
    queue: Rc<dyn Queue>,
    job: JobId,
    /// Whether the spooler has the whole document (the job is closed).
    queued: bool,
    /// Ticks since the spooler was last asked.
    since_status: u32,
    timer: TimerId,
    printer: String,
}

impl Session {
    /// Whether the document is still being handed to the spooler: until it
    /// is, quitting loses the job.
    pub fn is_preparing(&self) -> bool {
        !self.queued
    }
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
    if let Some(session) = &app.printing {
        // A failed cancel leaves a job the spooler still ends by itself.
        let _ = session.queue.cancel(session.job);
        if session.queued {
            app.print_bar.set_status("Canceling...");
        } else {
            end(
                app,
                ui,
                Err("Printing canceled".into()),
                "Printing canceled",
            );
        }
        return;
    }
    app.print_bar.set_shown(ui, false);
    app.editor.focus();
}

/// Stops a job still being prepared, so the app can quit: nothing of it
/// reached the printer.
pub fn stop(app: &mut Writer, ui: &mut Ui<Msg>) {
    if app.printing.as_ref().is_some_and(Session::is_preparing) {
        close(app, ui);
    }
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
        Err(error) => {
            println!("WRITER:PRINT:FAIL:{error}");
            app.print_bar.set_status(&error);
        }
    }
}

fn session(app: &Writer, ui: &Ui<Msg>) -> Result<Session, String> {
    let options = app.print_bar.options();
    // Checked here too, so a typo is reported before any page is rendered.
    ipp::uri::PrinterUri::parse(&options.printer)?;
    let out = app.editor.printout(DPI);
    let pages = parse_pages(&options.pages, out.page_count())?;
    let page = app.editor.with_document(|d| *d.page());
    let media = media_name(&page);
    let request = Request {
        printer: options.printer.clone(),
        user: user_name(),
        ticket: Ticket {
            name: names::display_name(app.path.as_deref()),
            format: "image/pwg-raster".into(),
            copies: Some(options.copies as i32),
            media: Some(media.clone()),
            color_mode: Some(if options.grey { "monochrome" } else { "color" }.into()),
            quality: Some(options.quality),
        },
    };
    let queue = Rc::clone(&app.host.print_queue);
    let job = queue.open(&request)?;
    if let Err(error) = queue.write(job, raster::SYNC) {
        let _ = queue.cancel(job);
        return Err(error);
    }
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
        queue,
        job,
        queued: false,
        since_status: 0,
        timer: ui.set_timer(TICK_MILLIS),
        printer: options.printer,
    })
}

/// One timer tick: hand the spooler a page, close the job once it has them
/// all, then follow it.
pub fn tick(app: &mut Writer, ui: &mut Ui<Msg>) {
    let Some(session) = &mut app.printing else {
        return;
    };
    if !session.queued {
        prepare(app, ui);
        return;
    }
    session.since_status += 1;
    if session.since_status < STATUS_TICKS {
        return;
    }
    session.since_status = 0;
    let info = match session.queue.status(session.job) {
        Ok(info) => info,
        Err(error) => {
            end(app, ui, Err(error.clone()), &error);
            return;
        }
    };
    if !info.state.is_final() {
        app.print_bar.set_status(&info.line);
        return;
    }
    let ink = if info.ink.is_empty() {
        String::new()
    } else {
        format!(" (ink: {})", info.ink)
    };
    let shown = format!("{}{ink}", info.line);
    let outcome = if info.state == State::Done {
        Ok(())
    } else {
        Err(info.line)
    };
    end(app, ui, outcome, &shown);
}

/// Renders the next page into the job, or closes it once they are all
/// there: from then on the spooler owns the job.
fn prepare(app: &mut Writer, ui: &mut Ui<Msg>) {
    let Some(session) = &mut app.printing else {
        return;
    };
    let total = session.pages.len();
    let step = if session.next < total {
        let page = session.pages[session.next];
        let mut bytes = Vec::new();
        render_page(&session.out, page, &session.format, &mut |b| {
            bytes.extend(b)
        });
        session.next += 1;
        session.queue.write(session.job, &bytes)
    } else {
        session
            .queue
            .close(session.job)
            .map(|()| session.queued = true)
    };
    if let Err(error) = step {
        let _ = session.queue.cancel(session.job);
        end(app, ui, Err(error.clone()), &error);
    } else if session.queued {
        println!("WRITER:PRINT:QUEUED:{total}");
        app.print_bar
            .set_status("Queued: the printer gets it even if you quit now");
    } else if session.next < total {
        let status = format!("Preparing page {} of {total}...", session.next + 1);
        app.print_bar.set_status(&status);
    }
}

/// Ends the job: the bar shows `shown`, the serial log the outcome.
fn end(app: &mut Writer, ui: &mut Ui<Msg>, outcome: Result<(), String>, shown: &str) {
    let Some(session) = app.printing.take() else {
        return;
    };
    ui.kill_timer(session.timer);
    app.print_bar.set_busy(false);
    app.print_bar.set_status(shown);
    match outcome {
        Ok(()) => {
            (app.host.remember_printer)(&session.printer);
            println!("WRITER:PRINT:PASS:{}", session.pages.len());
        }
        Err(error) => println!("WRITER:PRINT:FAIL:{error}"),
    }
}
