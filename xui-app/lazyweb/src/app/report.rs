//! Help > Report an issue: opens the form, builds the Markdown from it and the window's own
//! facts, and copies it or saves it (`crate::report`).
//!
//! Serial evidence: `LAZYWEB:REPORT:COPY` and `LAZYWEB:REPORT:SAVE` (no address, no text).

use xui_core::app::Ui;

use super::{now, Browser, Msg};
use crate::report::form::{Form, ReportMsg};
use crate::report::ring::{self, EngineLog};
use crate::report::{self, Facts, Typed, TRACE_LINES};

/// The form and the engine events this run saw.
pub(super) struct Report {
    pub(super) form: Form,
    pub(super) log: EngineLog,
}

impl Report {
    pub(super) fn new(form: Form) -> Report {
        Report {
            form,
            log: EngineLog::default(),
        }
    }
}

impl Browser {
    /// Notes an engine event for a report, with the seconds since the window started.
    pub(super) fn note_engine(&mut self, what: &str) {
        let seconds = self.started.elapsed().as_secs();
        self.report.log.note(seconds, what);
    }

    /// What the window knows that a report may carry.
    fn report_facts(&self) -> Facts {
        let url = if self.state.committed_url.is_empty() {
            &self.state.url
        } else {
            &self.state.committed_url
        };
        Facts {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            tabs: 1,
            platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            page_url: url.clone(),
            trace: ring::recent(TRACE_LINES),
            events: self.report.log.lines(),
        }
    }

    fn report_markdown(&self) -> String {
        let [happened, expected, steps] = self.report.form.texts();
        report::markdown(
            Typed {
                happened: &happened,
                expected: &expected,
                steps: &steps,
            },
            self.report.form.include(),
            &self.report_facts(),
        )
    }

    /// Help > Report an issue.
    pub(super) fn open_report(&mut self, ui: &Ui<Msg>) {
        self.hide_overlays(ui);
        let previews = report::previews(&self.report_facts());
        self.report.form.show(ui, &previews);
        self.announce(ui);
    }

    pub(super) fn report_msg(&mut self, ui: &Ui<Msg>, msg: ReportMsg) {
        match msg {
            ReportMsg::Toggled => {
                let previews = report::previews(&self.report_facts());
                self.report.form.set_previews(&previews);
            }
            ReportMsg::CopyMarkdown => {
                let markdown = self.report_markdown();
                ui.set_clipboard_text(&markdown);
                crate::marker("REPORT:COPY");
                self.set_status(&format!(
                    "Report copied to the clipboard ({} bytes)",
                    markdown.len()
                ));
            }
            ReportMsg::SaveMarkdown => {
                let markdown = self.report_markdown();
                // The Downloads folder, named and made the way a download is: the window has no
                // Save dialog of its own.
                let dir = self.transfers.folder().to_path_buf();
                match report::save(&dir, now(), &markdown) {
                    Ok(path) => {
                        crate::marker("REPORT:SAVE");
                        self.set_status(&format!("Report saved to {}", path.display()));
                    }
                    Err(e) => self.set_status(&format!("Report not saved: {e}")),
                }
            }
            ReportMsg::Hide => {
                self.report.form.hide(ui);
                self.page.focus();
            }
            ReportMsg::Cancel => {
                self.report.form.hide(ui);
                self.report.form.clear();
                self.page.focus();
            }
        }
    }
}
