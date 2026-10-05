//! What each message does. Every effect runs here, in `App::update`, after
//! the widget that raised it has returned.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lazyarc::extract::Report;
use lazyarc::format::{sniff, Format, Level};
use lazyarc::Error;
use xui_core::app::Ui;
use xui_core::widget::{DialogAction, TaskDialogAction};

use crate::app::{ArchiverApp, Msg};
use crate::folder::{self, RowKind};
use crate::job::{Job, Outcome, Task};
use crate::ui::{self, MENU_DELETE, MENU_EXTRACT, MENU_OPEN, MENU_TEST};

/// How often a running job's progress is read.
const TICK_MS: u32 = 100;
/// Skipped entries listed in a report box before "and N more".
const REPORT_LINES: usize = 8;

pub fn dispatch(app: &mut ArchiverApp, msg: Msg, ui: &mut Ui<Msg>) {
    match msg {
        Msg::Open => {
            // Re-pointing the picker re-lists it, so it shows today's files.
            app.dialogs.open.set_initial_dir(picker_dir(app));
            app.dialogs.open.open();
        }
        Msg::New => new_archive(app, Vec::new()),
        Msg::Add => add(app),
        Msg::Extract => extract(app, ui),
        Msg::Test => {
            if app.archive.is_some() {
                start(app, ui, Task::Test);
            }
        }
        Msg::Delete => delete(app, ui),
        Msg::Up => up(app),
        Msg::Level(index) => app.level = Level::ALL.get(index).copied().unwrap_or_default(),
        Msg::Selection(rows) => {
            app.previous = std::mem::replace(&mut app.selection, rows);
        }
        Msg::Activate(row) => activate(app, ui, row),
        Msg::Sort(column) => {
            app.sort = app.header_sort(column);
            folder::sort(&mut app.rows, app.sort.0, app.sort.1);
            app.selection.clear();
        }
        Msg::Context(row, at) => context(app, ui, row, at),
        Msg::Menu(id) => {
            if id == MENU_OPEN {
                if let Some(row) = app.context_row {
                    activate(app, ui, row);
                }
            } else if id == MENU_EXTRACT {
                extract(app, ui);
            } else if id == MENU_DELETE {
                delete(app, ui);
            } else if id == MENU_TEST && app.archive.is_some() {
                start(app, ui, Task::Test);
            }
        }
        Msg::OpenChosen(path) => start(app, ui, Task::Open(path)),
        Msg::NewChosen(path) => create(app, ui, path),
        Msg::AddChosen(path) => add_paths(app, ui, vec![path]),
        Msg::ExtractTo(DialogAction::Accept(text)) => extract_to(app, ui, text),
        Msg::ExtractTo(_) => {}
        Msg::DeleteConfirmed(TaskDialogAction::Command(0)) => {
            let paths = folder::selected_paths(&app.rows, &app.selection);
            start(app, ui, Task::Delete { paths });
        }
        Msg::DeleteConfirmed(_) => {}
        Msg::PickerClosed => app.pending.clear(),
        Msg::MessageClosed => {}
        Msg::Tick => tick(app, ui),
        Msg::Cancel => {
            if let Some(job) = &app.job {
                job.cancel();
                app.say("Cancelling...");
            }
        }
        Msg::DragEnter => app.hover = true,
        Msg::DragLeave => app.hover = false,
        Msg::Dropped(paths) => dropped(app, ui, paths),
        Msg::DragStarted(rows) => {
            (app.host.log)(&format!("ARCHIVER:DRAG:PASS:{}", rows.len()));
            app.selection = rows;
            app.say("Drop the items on a Files window to copy them out");
        }
        Msg::DragFailed(reason) => app.say(format!("Cannot drag: {reason}")),
        Msg::Close => {
            if let Some(job) = &app.job {
                job.cancel();
            }
            (app.host.log)("ARCHIVER:CLOSE:PASS");
            ui.quit();
        }
    }
}

/// Start `task` on a worker thread, unless one is already running.
fn start(app: &mut ArchiverApp, ui: &mut Ui<Msg>, task: Task) {
    if app.job.is_some() {
        app.say("Busy: wait for the current operation or cancel it");
        return;
    }
    match Job::start(task, app.archive.clone()) {
        Ok(job) => {
            app.say(format!("{}...", job.task.verb()));
            app.job = Some(job);
            app.timer = Some(ui.set_timer(TICK_MS));
        }
        Err(error) => app.tell("Archiver", &error),
    }
}

fn tick(app: &mut ArchiverApp, ui: &mut Ui<Msg>) {
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

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn up(app: &mut ArchiverApp) {
    if app.archive.is_some() && !app.folder.is_empty() {
        app.folder = folder::parent(&app.folder);
        app.refresh_rows();
    }
}

fn activate(app: &mut ArchiverApp, ui: &mut Ui<Msg>, index: usize) {
    let Some(row) = app.rows.get(index).cloned() else {
        return;
    };
    match row.kind {
        RowKind::Parent => up(app),
        RowKind::Folder => {
            app.folder = row.path;
            app.refresh_rows();
        }
        RowKind::File | RowKind::Link => {
            let scratch = app.host.temp_dir.join(format!("open-{}", unique()));
            start(
                app,
                ui,
                Task::OpenInside {
                    path: row.path,
                    scratch,
                },
            );
        }
    }
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn context(app: &mut ArchiverApp, ui: &mut Ui<Msg>, row: usize, at: xui_core::geometry::Point) {
    let real = app.rows.get(row).is_some_and(|r| r.kind != RowKind::Parent);
    app.context_row = Some(row);
    let writable = app
        .archive
        .as_ref()
        .is_some_and(|a| a.format.writable() && !a.format.single_file());
    app.w.menu.set_enabled(MENU_OPEN, real);
    app.w.menu.set_enabled(MENU_EXTRACT, app.archive.is_some());
    app.w.menu.set_enabled(MENU_DELETE, real && writable);
    app.w.menu.set_enabled(MENU_TEST, app.archive.is_some());
    // A right click outside the selection acts on that row alone.
    if real && !app.selection.contains(&row) {
        app.previous = std::mem::replace(&mut app.selection, vec![row]);
    }
    let bounds = ui.bounds(app.w.list.id());
    app.w
        .menu
        .show_context(bounds.left + at.x, bounds.top + at.y);
}

/// What the selection is called in a dialog.
fn describe(app: &ArchiverApp, paths: &[String]) -> String {
    match paths {
        [] if app.folder.is_empty() => "everything".to_owned(),
        [] => "this folder".to_owned(),
        [one] => format!("\"{}\"", one.rsplit('/').next().unwrap_or(one)),
        many => format!("{} items", many.len()),
    }
}

fn extract(app: &mut ArchiverApp, ui: &mut Ui<Msg>) {
    let Some(archive) = app.archive.clone() else {
        app.say("Open an archive first");
        return;
    };
    let paths = folder::selected_paths(&app.rows, &app.selection);
    let stem = archive.format.strip_extension(&display_name(&archive.path));
    let base = archive
        .path
        .parent()
        .filter(|dir| app.host.suggests(dir))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| app.host.start_dir.clone());
    let dest = base.join(stem);
    let what = describe(app, &paths);
    match ui::extract_dialog(ui, &what, &dest.to_string_lossy()) {
        Ok(dialog) => {
            dialog.open();
            app.dialogs.extract = Some(dialog);
        }
        Err(error) => app.tell("Archiver", &error.to_string()),
    }
}

fn extract_to(app: &mut ArchiverApp, ui: &mut Ui<Msg>, text: String) {
    let text = text.trim();
    if text.is_empty() {
        app.say("No folder given");
        return;
    }
    let mut paths = folder::selected_paths(&app.rows, &app.selection);
    // Nothing selected below the root means "this folder", not everything.
    if paths.is_empty() && !app.folder.is_empty() {
        paths.push(app.folder.clone());
    }
    let strip = app.folder.clone();
    start(
        app,
        ui,
        Task::Extract {
            dest: PathBuf::from(text),
            paths,
            strip,
        },
    );
}

fn writable(app: &ArchiverApp) -> bool {
    app.archive
        .as_ref()
        .is_some_and(|a| a.format.writable() && !a.format.single_file())
}

fn delete(app: &mut ArchiverApp, ui: &mut Ui<Msg>) {
    let paths = folder::selected_paths(&app.rows, &app.selection);
    if paths.is_empty() {
        app.say("Select what to delete first");
        return;
    }
    if !writable(app) {
        app.say("This archive cannot be changed");
        return;
    }
    let what = describe(app, &paths);
    match ui::delete_dialog(ui, &what) {
        Ok(dialog) => {
            dialog.open();
            app.dialogs.delete = Some(dialog);
        }
        Err(error) => app.tell("Archiver", &error.to_string()),
    }
}

fn add(app: &mut ArchiverApp) {
    if app.archive.is_some() && !writable(app) {
        app.say("This archive cannot be changed");
        return;
    }
    app.dialogs.add.set_initial_dir(picker_dir(app));
    app.dialogs.add.open();
}

/// Where the Open and Add pickers start: beside the open archive when that
/// folder is a sensible place, else the start folder.
fn picker_dir(app: &ArchiverApp) -> PathBuf {
    app.archive
        .as_ref()
        .and_then(|archive| archive.path.parent())
        .filter(|dir| app.host.suggests(dir))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| app.host.start_dir.clone())
}

/// Add `paths` to the open archive, or start a new one with them.
fn add_paths(app: &mut ArchiverApp, ui: &mut Ui<Msg>, paths: Vec<PathBuf>) {
    if app.archive.is_none() {
        new_archive(app, paths);
        return;
    }
    let (folder, level) = (app.folder.clone(), app.level);
    start(
        app,
        ui,
        Task::Add {
            sources: paths,
            folder,
            level,
        },
    );
}

/// Ask where a new archive of `sources` goes.
fn new_archive(app: &mut ArchiverApp, sources: Vec<PathBuf>) {
    let dir = sources
        .first()
        .and_then(|path| path.parent())
        .filter(|dir| !dir.as_os_str().is_empty() && app.host.suggests(dir))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| app.host.start_dir.clone());
    app.dialogs.new.set_initial_dir(dir);
    app.dialogs
        .new
        .set_suggested_name(&ui::suggested_name(&sources, Format::Zip));
    app.pending = sources;
    app.dialogs.new.open();
}

fn create(app: &mut ArchiverApp, ui: &mut Ui<Msg>, path: PathBuf) {
    let (path, format) = match Format::for_name(&display_name(&path)) {
        Some(format) => (path, format),
        None => {
            let mut named = path.into_os_string();
            named.push(".zip");
            (PathBuf::from(named), Format::Zip)
        }
    };
    if !format.writable() {
        app.tell(
            "Archiver",
            &format!("{} archives can be read but not written.", format.name()),
        );
        return;
    }
    let sources = std::mem::take(&mut app.pending);
    let level = app.level;
    start(
        app,
        ui,
        Task::Create {
            dest: path,
            format,
            level,
            sources,
        },
    );
}

/// Whether `path` looks like an archive this app reads.
pub fn looks_like_archive(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if Format::for_name(&display_name(path)).is_some() {
        return true;
    }
    let mut head = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(512).read_to_end(&mut head))
        .is_ok_and(|_| sniff(&head).is_some())
}

fn dropped(app: &mut ArchiverApp, ui: &mut Ui<Msg>, paths: Vec<PathBuf>) {
    app.hover = false;
    (app.host.log)(&format!("ARCHIVER:DROP:{}", paths.len()));
    if paths.is_empty() {
        return;
    }
    if app.job.is_some() {
        app.say("Busy: wait for the current operation or cancel it");
        return;
    }
    let one_archive = paths.len() == 1 && looks_like_archive(&paths[0]);
    match &app.archive {
        Some(_) if writable(app) => add_paths(app, ui, paths),
        Some(_) if one_archive => start(app, ui, Task::Open(paths[0].clone())),
        Some(_) => app.say("This archive cannot be changed; drop an archive to open it"),
        None if one_archive => start(app, ui, Task::Open(paths[0].clone())),
        None => new_archive(app, paths),
    }
}
