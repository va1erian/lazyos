//! Run, Stop and the Output tabs (`docs/messenger-playground-plan.md` D3,
//! D4, D7): the editor's text through the review gate to the run table, one
//! tab per run, errors as links back to the editor.

use xui_app::devtool::{review_dialog, ReviewAnswer};
use xui_app::trace::{self, Channel};
use xui_core::app::Ui;
use xui_core::arrange::{column, list, LayoutExt};
use xui_core::widget::{TaskDialog, TaskDialogAction, TaskDialogIcon};
use xui_core::Fill;
use xui_playground::runner::{too_many_live, Run, RunId, RunState, Stream, MAX_LIVE};

use crate::app::{PendingRun, Playground, RunPage};
use crate::msg::Msg;

impl Playground {
    // ---- the editor ------------------------------------------------------

    /// The buffer changed. A change that is the text we just set is ours.
    /// Anything else goes through `crate::paste`: a paste (the editor read
    /// the clipboard) or an edit that inserts more than a key can start the
    /// review over; typing and Undo/Redo to a vouched text do not (the Edit
    /// menu's Paste and Ctrl+V both read the clipboard).
    pub fn edited(&mut self, text: String) {
        let pasted = self.w.paste.take();
        if text == self.text {
            return;
        }
        crate::paste::apply_edit(&mut self.gate, &mut self.known, &self.text, &text, pasted);
        self.text = text;
        self.dirty = true;
        self.update_file_label();
    }

    /// The text to run and the editor line it starts on: the selection when
    /// asked and there is one, else the whole buffer.
    fn run_text(&self, selection: bool) -> (String, usize) {
        if selection {
            if let Some((start, end)) = self.editor.selection() {
                let (start, end) = (start.min(end), start.max(end));
                let chars: Vec<char> = self.text.chars().collect();
                if end <= chars.len() && start < end {
                    let line = chars[..start].iter().filter(|c| **c == '\n').count() + 1;
                    return (chars[start..end].iter().collect(), line);
                }
            }
        }
        (self.text.clone(), 1)
    }

    /// F5 / Ctrl+Enter / the Run button: the only ways a script starts.
    pub fn run_buffer(&mut self, ui: &mut Ui<Msg>, selection: bool) {
        let (script, first_line) = self.run_text(selection);
        if script.trim().is_empty() {
            self.note("Nothing to run: the editor is empty.");
            return;
        }
        let title = self.buffer_name();
        let pending = PendingRun {
            script,
            first_line,
            title,
        };
        if self.gate.needs_confirmation() {
            self.pending = Some(pending);
            let origin = format!("{:?}", self.gate.provenance()).to_lowercase();
            match review_dialog(ui, self.gate.provenance(), |a| Some(Msg::Reviewed(a))) {
                Ok(card) => {
                    self.review = Some(card);
                    // Evidence that the card was shown (a count of runs cannot
                    // tell "asked and confirmed" from "never asked"): printed
                    // once the card is open, so an ASK never stands for a card
                    // that failed to appear.
                    trace::line!(Channel::Playground, "REVIEW:ASK:{origin}");
                }
                Err(e) => self.note(&format!("review card: {e}")),
            }
            return;
        }
        self.start_run(&pending.script, pending.first_line, &pending.title);
    }

    /// The review card was answered.
    pub fn reviewed(&mut self, _ui: &mut Ui<Msg>, answer: ReviewAnswer) {
        self.review = None;
        let Some(pending) = self.pending.take() else {
            return;
        };
        if answer == ReviewAnswer::Run {
            self.gate.confirm();
            crate::paste::vouch(&self.gate, &mut self.known, &self.text);
            self.start_run(&pending.script, pending.first_line, &pending.title);
        } else {
            self.note("Not run.");
        }
        self.focus_editor();
    }

    /// Ask before a call that can stop services (D7's first row).
    pub fn confirm_call(&mut self, ui: &mut Ui<Msg>, script: String, title: String, call: String) {
        let message = format!(
            "{call} can stop or restart services, apps or the machine. Unsaved work may be lost."
        );
        let card = TaskDialog::new(ui, "Call a method that can stop things?", &message)
            .and_then(|d| d.icon(TaskDialogIcon::Warning).command("Call"))
            .map(|d| {
                d.on_action(|a| {
                    Some(Msg::CallConfirmed(matches!(
                        a,
                        TaskDialogAction::Command(0)
                    )))
                })
            });
        match card {
            Ok(card) => {
                card.open();
                self.pending_call = Some((script, title, call));
                self.confirm_card = Some(card);
            }
            Err(e) => self.note(&format!("confirm card: {e}")),
        }
    }

    /// The confirm card was answered.
    pub fn call_confirmed(&mut self, yes: bool) {
        self.confirm_card = None;
        let Some((script, title, call)) = self.pending_call.take() else {
            return;
        };
        if yes {
            if let Some(id) = self.start_run(&script, 1, &title) {
                self.calls.push((id, call));
            }
        } else {
            self.note("Not called.");
        }
    }

    // ---- starting and stopping ------------------------------------------

    /// Start `script` as a new run with its own Output tab.
    pub fn start_run(&mut self, script: &str, first_line: usize, title: &str) -> Option<RunId> {
        if self.runs.live_count() >= MAX_LIVE {
            self.note(&too_many_live());
            return None;
        }
        self.reveal_bottom(false);
        let now = self.now_ms();
        let started = self.runs.start(script, title, first_line, now);
        let id = self.runs.runs().last().map(|r| r.id)?;
        self.add_page(id, title);
        self.trace_markers();
        self.refresh_page(id);
        started
            .err()
            .map(|e| self.note(&format!("cannot start: {e}")));
        Some(id)
    }

    /// A short name for the buffer: the file's, or `script`.
    pub fn buffer_name(&self) -> String {
        self.file
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "script".to_string())
    }

    /// Stop the run of the selected Output tab, else the newest live one.
    pub fn stop_selected(&mut self) {
        let selected = self
            .pages
            .get(self.w.output.get().selected())
            .map(|p| p.id)
            .filter(|id| self.runs.get(*id).is_some_and(|r| r.state.is_live()));
        let target = selected.or_else(|| {
            self.runs
                .runs()
                .iter()
                .rev()
                .find(|r| r.state.is_live())
                .map(|r| r.id)
        });
        if let Some(id) = target {
            self.runs.stop(id);
        } else {
            self.note("No run is live.");
        }
    }

    // ---- output tabs -----------------------------------------------------

    fn add_page(&mut self, id: RunId, title: &str) {
        let handle = xui_core::arrange::Handle::new();
        let run_id = id.0;
        let page = column().child(
            list()
                .column("line", Fill)
                .on_activate(move |row| Msg::OutputRow(run_id, row))
                .bind(&handle)
                .fill(1),
        );
        let tabs = self.w.output.get();
        let name = tab_title(id, title, None);
        if tabs.add_layout_page(&name, page).is_ok() {
            if self.welcome_page {
                tabs.remove_page(0);
                self.welcome_page = false;
            }
            self.pages.push(RunPage {
                id,
                list: handle,
                signature: (u64::MAX, 0),
            });
            tabs.select(self.pages.len() - 1);
        }
        // Forget the tabs of runs the table forgot.
        while self.pages.len() > 1 && self.runs.get(self.pages[0].id).is_none() {
            self.pages.remove(0);
            tabs.remove_page(0);
        }
        self.probe_soon();
    }

    pub fn output_tab(&mut self, _index: usize) {
        self.probe_soon();
    }

    /// Output or an exit waits in the runner's channel.
    pub fn runner_wake(&mut self) {
        let now = self.now_ms();
        self.runs.poll(now);
        self.trace_markers();
        let ids: Vec<RunId> = self.pages.iter().map(|p| p.id).collect();
        for id in ids {
            self.refresh_page(id);
        }
    }

    /// Redraw run `id`'s tab if it changed.
    fn refresh_page(&mut self, id: RunId) {
        let Some(index) = self.pages.iter().position(|p| p.id == id) else {
            return;
        };
        let Some(run) = self.runs.get(id) else {
            return;
        };
        let signature = (run.lines_total, state_code(&run.state));
        if self.pages[index].signature == signature {
            return;
        }
        let now = self.now_ms();
        let rows = output_rows(run, now);
        let last = rows.len().saturating_sub(1);
        let title = tab_title(id, &run.title, run.state.status_text().as_deref());
        let list = self.pages[index].list.get();
        list.refresh_model(rows);
        list.ensure_visible(last);
        self.w.output.get().rename_page(index, &title);
        self.pages[index].signature = signature;
    }

    /// Double click on an error line: the editor goes to the line.
    pub fn output_row(&mut self, id: u32, row: usize) {
        let Some(run) = self.runs.get(RunId(id)) else {
            return;
        };
        let Some(line) = run.output.get(row) else {
            return;
        };
        match line
            .mark
            .as_ref()
            .and_then(|m| m.line.map(|l| (l, m.column)))
        {
            Some((line, column)) => {
                self.reveal_bottom(true);
                self.editor
                    .goto(line.saturating_sub(1), column.saturating_sub(1));
                self.focus_editor();
            }
            None => self.note("That line has no position in the editor."),
        }
    }

    /// Print the run markers and the call results since the last look.
    fn trace_markers(&mut self) {
        for marker in self.runs.take_markers() {
            trace::line!(Channel::Playground, "RUN:{}", marker.args());
            if marker.status.is_some() {
                self.call_result(marker.id);
            }
        }
        self.update_editor_marks();
    }

    /// `CALL:<interface>.<method>:OK|ERR:<errno>` for a Call-form run that ended.
    fn call_result(&mut self, id: RunId) {
        let Some(at) = self.calls.iter().position(|(run, _)| *run == id) else {
            return;
        };
        let (_, name) = self.calls.remove(at);
        let Some(run) = self.runs.get(id) else {
            return;
        };
        match run.state {
            RunState::Exited(0) => trace::line!(Channel::Playground, "CALL:{name}:OK"),
            _ => {
                let errno = run
                    .output
                    .iter()
                    .filter(|l| l.stream == Stream::Stderr)
                    .find_map(|l| errno_name(&l.text))
                    .unwrap_or("ERR");
                trace::line!(Channel::Playground, "CALL:{name}:ERR:{errno}");
            }
        }
    }

    /// Put squiggles on the editor lines of the newest run's errors.
    fn update_editor_marks(&mut self) {
        let mut marks = Vec::new();
        if let Some(run) = self.runs.runs().last() {
            for mark in run.marks() {
                if let Some(line) = mark.line {
                    marks.push(xui_code_editor::Marker::line(
                        line.saturating_sub(1),
                        xui_code_editor::MarkerKind::Error,
                    ));
                }
            }
        }
        self.editor.set_markers(marks);
    }

    pub fn live_runs(&self) -> usize {
        self.runs.live_count()
    }
}

fn state_code(state: &RunState) -> u8 {
    match state {
        RunState::Running => 0,
        RunState::Stopping => 1,
        RunState::Exited(code) => 2u8.wrapping_add((*code as u8).wrapping_mul(3)),
        RunState::Stopped => 3,
        RunState::Failed(_) => 4,
    }
}

fn tab_title(id: RunId, title: &str, status: Option<&str>) -> String {
    match status {
        None => format!("#{} {title} ...", id.0),
        Some("0") => format!("#{} {title} ok", id.0),
        Some(other) => format!("#{} {title} {other}", id.0),
    }
}

/// The lines a tab shows: output (stderr marked), then the state.
fn output_rows(run: &Run, now_ms: u64) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(run.output.len() + 2);
    if run.dropped_lines > 0 {
        rows.push(vec![format!(
            "... {} earlier lines dropped",
            run.dropped_lines
        )]);
    }
    for line in &run.output {
        let text = match line.stream {
            Stream::Stdout => line.text.clone(),
            Stream::Stderr => format!("! {}", line.text),
        };
        rows.push(vec![text]);
    }
    // A refusal is shown as the service wrote it; say where to look for the rule.
    if run
        .output
        .iter()
        .any(|l| l.stream == Stream::Stderr && l.text.contains("EACCES"))
    {
        rows.push(vec![
            "hint: denied by policy. Build an image with LAZYOS_LABEL_TRACE=1 to see LABEL:DENY (docs/messenger.md, section 5)."
                .to_string(),
        ]);
    }
    let seconds = run.elapsed_ms(now_ms) as f64 / 1000.0;
    rows.push(vec![match &run.state {
        RunState::Running => format!("-- running ({seconds:.1} s, {} lines) --", run.lines_total),
        RunState::Stopping => "-- stopping --".to_string(),
        RunState::Exited(code) => format!("-- exit {code} after {seconds:.1} s --"),
        RunState::Stopped => format!("-- stopped after {seconds:.1} s --"),
        RunState::Failed(reason) => format!("-- not started: {reason} --"),
    }]);
    rows
}

/// The errno name in a refusal text, if it has one.
fn errno_name(text: &str) -> Option<&'static str> {
    const NAMES: &[&str] = &[
        "EACCES",
        "EPERM",
        "ENOENT",
        "ENOSYS",
        "EINVAL",
        "ETIMEDOUT",
        "EAGAIN",
        "EPIPE",
        "ENOMEM",
        "EBUSY",
        "EEXIST",
        "E2BIG",
        "ECANCELED",
    ];
    NAMES.iter().copied().find(|n| text.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_playground::runner::{OutputLine, Run, RunTable};
    use xui_playground::runner_fake::FakeRunner;

    #[test]
    fn tabs_say_how_a_run_ended() {
        assert_eq!(
            tab_title(RunId(2), "confd.Info", Some("0")),
            "#2 confd.Info ok"
        );
        assert_eq!(tab_title(RunId(3), "script", Some("1")), "#3 script 1");
        assert_eq!(
            tab_title(RunId(4), "script", Some("stopped")),
            "#4 script stopped"
        );
        assert_eq!(tab_title(RunId(5), "script", None), "#5 script ...");
    }

    #[test]
    fn errno_names_are_found_in_a_refusal_and_not_invented() {
        assert_eq!(
            errno_name("os.lazy.keyd.v1.Ping: permission denied (EACCES)"),
            Some("EACCES")
        );
        assert_eq!(errno_name("division by zero"), None);
    }

    fn run_with(lines: &[(Stream, &str)], state: RunState) -> Run {
        let mut table = RunTable::new(FakeRunner::new());
        table.start("print(1);", "script", 1, 0).unwrap();
        let mut run = table.runs()[0].clone();
        run.state = state;
        for (stream, text) in lines {
            run.output.push_back(OutputLine {
                stream: *stream,
                text: (*text).to_string(),
                mark: None,
            });
        }
        run
    }

    #[test]
    fn a_refusal_gets_the_policy_hint_and_every_run_a_status_line() {
        let refused = run_with(
            &[(
                Stream::Stderr,
                "rhai: os.lazy.keyd.v1.Ping: permission denied (EACCES)",
            )],
            RunState::Exited(1),
        );
        let rows = output_rows(&refused, 0);
        assert!(rows[0][0].starts_with("! "));
        assert!(rows
            .iter()
            .any(|r| r[0].starts_with("hint: denied by policy")));
        assert!(rows.last().unwrap()[0].starts_with("-- exit 1"));
        let fine = run_with(&[(Stream::Stdout, "hello")], RunState::Exited(0));
        let rows = output_rows(&fine, 0);
        assert_eq!(rows[0][0], "hello");
        assert!(!rows.iter().any(|r| r[0].starts_with("hint")));
    }
}
