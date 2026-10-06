//! What happens when a job ends: the status line, the report boxes and the
//! `ARCHIVER:*` evidence for each outcome.

use std::sync::Arc;

use lazyarc::extract::Report;
use lazyarc::Error;
use xui_core::app::Ui;

use super::display_name;
use crate::app::{ArchiverApp, Msg};
use crate::folder;
use crate::job::{Job, Outcome};

/// Skipped entries listed in a report box before "and N more".
const REPORT_LINES: usize = 8;

pub(super) fn tick(app: &mut ArchiverApp, ui: &mut Ui<Msg>) {
    let Some(outcome) = app.job.as_ref().and_then(Job::poll) else {
        return;
    };
    let job = app.job.take();
    if let Some(timer) = app.timer.take() {
        ui.kill_timer(timer);
    }
    let verb = job.as_ref().map(|job| job.task.verb()).unwrap_or("");
    let target = job
        .as_ref()
        .and_then(|job| job.task.target())
        .map(|path| format!(" {}", path.display()))
        .unwrap_or_default();
    match outcome {
        Ok(outcome) => finished(app, outcome),
        Err(Error::Cancelled) => {
            (app.host.log)("ARCHIVER:CANCEL:PASS");
            app.say("Cancelled");
        }
        Err(error) => {
            (app.host.log)(&format!("ARCHIVER:FAIL:{verb}:{error}:{target}"));
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
            (app.host.log)(&format!(
                "ARCHIVER:OPEN:PASS:{}:{count}",
                archive.format.name()
            ));
            app.say(format!(
                "Opened {} ({count} entries)",
                display_name(&archive.path)
            ));
            app.archive = Some(Arc::new(archive));
            app.folder.clear();
            app.refresh_rows();
        }
        Outcome::Extracted { report, dest } => {
            (app.host.log)(&format!(
                "ARCHIVER:EXTRACT:PASS:{}:{}",
                report.files,
                report.skipped.len()
            ));
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
                (app.host.log)(&format!("ARCHIVER:TEST:PASS:{}", report.files));
                app.say("Test passed");
                let text = format!(
                    "No errors: {} files, {} checked.",
                    report.files,
                    crate::cells::size(report.bytes)
                );
                app.tell("Test", &text);
            } else {
                (app.host.log)(&format!("ARCHIVER:TEST:FAIL:{}", report.skipped.len()));
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
            (app.host.log)(&format!(
                "ARCHIVER:{}:PASS:{}",
                verb.to_uppercase(),
                report.files
            ));
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
        Outcome::Unpacked(path) => match (app.host.launch)(&path) {
            Ok(()) => {
                (app.host.log)("ARCHIVER:OPENINSIDE:PASS");
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
