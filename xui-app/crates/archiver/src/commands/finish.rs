//! What happens when a job ends: the status line, the report boxes and the
//! `ARCHIVER:*` evidence for each outcome.

use std::sync::Arc;

use lazyarc::extract::Report;
use lazyarc::Error;

use super::display_name;
use crate::app::ArchiverApp;
use crate::folder;
use crate::job::{Finished, Outcome};
use xui_host::report;

/// Skipped entries listed in a report box before "and N more".
const REPORT_LINES: usize = 8;

/// The job's answer: report it and forget the job.
pub(super) fn job_ended(app: &mut ArchiverApp, done: &Finished) {
    let Some(outcome) = done.take() else {
        return;
    };
    let job = app.job.take();
    let verb = job.as_ref().map(|job| job.task.verb()).unwrap_or("");
    let target = job
        .as_ref()
        .and_then(|job| job.task.target())
        .map(|path| format!(" {}", path.display()))
        .unwrap_or_default();
    match outcome {
        Ok(outcome) => finished(app, outcome),
        Err(Error::Cancelled) => {
            app.host.report("CANCEL:PASS");
            app.say("Cancelled");
        }
        Err(error) => {
            report!(app.host, "FAIL:{verb}:{error}:{target}");
            app.say(format!("{verb} failed"));
            app.tell("Archiver", &format!("{verb}{target} failed: {error}"));
        }
    }
    app.pending.clear();
}

fn finished(app: &mut ArchiverApp, outcome: Outcome) {
    match outcome {
        Outcome::Opened(archive) => {
            let count = archive.entries.len();
            report!(app.host, "OPEN:PASS:{}:{count}", archive.format.name());
            app.say(format!(
                "Opened {} ({count} entries)",
                display_name(&archive.path)
            ));
            app.archive = Some(Arc::new(archive));
            app.folder.clear();
            app.refresh_rows();
        }
        Outcome::Extracted { report, dest } => {
            report!(
                app.host,
                "EXTRACT:PASS:{}:{}",
                report.files,
                report.skipped.len()
            );
            app.say(format!(
                "{} to {}",
                report.summary("Extracted"),
                dest.display()
            ));
            if !report.skipped.is_empty() {
                let text = skipped_text(&report);
                app.tell("Some entries were not extracted", &text);
            }
        }
        Outcome::Tested(report) => {
            if report.skipped.is_empty() {
                report!(app.host, "TEST:PASS:{}", report.files);
                app.say("Test passed");
                let text = format!(
                    "No errors: {} files, {} checked.",
                    report.files,
                    crate::cells::size(report.bytes)
                );
                app.tell("Test", &text);
            } else {
                report!(app.host, "TEST:FAIL:{}", report.skipped.len());
                app.say(format!("Test found {} problem(s)", report.skipped.len()));
                let text = skipped_text(&report);
                app.tell("Test found problems", &text);
            }
        }
        Outcome::Changed {
            archive,
            report,
            verb,
        } => {
            report!(app.host, "{}:PASS:{}", verb.to_uppercase(), report.files);
            let keep = folder::exists(&archive.entries, &app.folder).then(|| app.folder.clone());
            app.say(format!(
                "{verb} {} - {}",
                display_name(&archive.path),
                report.summary("wrote")
            ));
            app.archive = Some(Arc::new(archive));
            app.folder = keep.unwrap_or_default();
            app.refresh_rows();
            if !report.skipped.is_empty() {
                let text = skipped_text(&report);
                app.tell("Some files were skipped", &text);
            }
        }
        Outcome::Unpacked(path) => match app.host.launch(&path) {
            Ok(()) => {
                app.host.report("OPENINSIDE:PASS");
                app.say(format!("Opened {}", display_name(&path)));
            }
            Err(error) => app.tell("Cannot open", &format!("{}: {error}", display_name(&path))),
        },
    }
}

fn skipped_text(report: &Report) -> String {
    let mut lines: Vec<String> = report
        .skipped
        .iter()
        .take(REPORT_LINES)
        .map(|(path, why)| format!("{path}: {why}"))
        .collect();
    if report.skipped.len() > REPORT_LINES {
        lines.push(format!("and {} more", report.skipped.len() - REPORT_LINES));
    }
    lines.join("\n")
}
