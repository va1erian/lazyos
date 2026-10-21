//! What the Explorer selects, what the doc pane then shows, and what the doc
//! pane's buttons do (`docs/messenger-playground-plan.md` section 3).
//!
//! Every button writes Rhai (D3): the text goes to the editor, or runs once
//! in an Output tab. Listen and Publish go to the Topic monitor window
//! ([`crate::topicshost`]). Nothing here talks to the fabric except the Live
//! and Topics reads.

use rhai_lazy::msg::schema::{Interface, Method, Topic};
use xui_app::devtool::Provenance;
use xui_app::fabric::{self, Topics};
use xui_app::trace::{self, Channel};
use xui_core::app::Ui;
use xui_core::arrange::{column, LayoutExt};
use xui_core::widget::HasText;
use xui_playground::catalog::{Item, NodeId};
use xui_playground::docs::{self, Block, Chip, ChipKind};
use xui_playground::rhai_names::scriptable;
use xui_playground::snippet::{self, CallOptions, TopicAction};

use crate::app::{Playground, Sel};
use crate::docview;
use crate::msg::Action;
use crate::tmsg::Request;

/// The topics list is read at most this often without the user asking (the
/// broker never frees a receive buffer; `fabricmon` reads every ten ticks).
const TOPICS_MIN_SECS: u64 = 10;

impl Playground {
    // ---- the Explorer ---------------------------------------------------

    pub fn explorer_tab(&mut self, ui: &mut Ui<crate::msg::Msg>, tab: usize) {
        let _ = ui;
        if self.tab != tab {
            self.tab = tab;
            self.w.explorer.get().select(tab);
            let filter = match tab {
                0 => self.ex.query.clone(),
                1 => self.ex.live_filter.clone(),
                2 => self.ex.topic_filter.clone(),
                _ => self.ex.gallery_filter.clone(),
            };
            self.w.search.get().set_text(&filter);
            self.probe_soon();
        }
        if tab == 2 {
            self.refresh_topics(false);
        }
    }

    pub fn search(&mut self, text: &str) {
        match self.tab {
            0 => {
                self.ex.set_query(text);
                self.refill_tree();
            }
            1 => {
                self.ex.live_filter = text.to_string();
                self.refill_live();
            }
            2 => {
                self.ex.topic_filter = text.to_string();
                self.refill_topics();
            }
            _ => {
                self.ex.gallery_filter = text.to_string();
                self.refill_gallery();
            }
        }
    }

    /// Rebuild the tree rows, keeping the selection when its row still exists.
    pub fn refill_tree(&mut self) {
        let rows = self.ex.tree_rows();
        let tree = self.w.tree.get();
        tree.set_rows(&rows);
        if let Sel::Node(id) = &self.sel {
            if let Some(row) = self.ex.row_of(id) {
                tree.select(Some(row));
            }
        }
    }

    pub fn refill_live(&mut self) {
        let rows = self.ex.live_table();
        if rows != self.last_live {
            self.w.live.get().refresh_model(rows.clone());
            self.last_live = rows;
        }
    }

    pub fn refill_topics(&mut self) {
        let rows = self.ex.topic_table();
        self.w.topics.get().refresh_model(rows);
    }

    pub fn refill_gallery(&mut self) {
        let rows = self.ex.gallery_table();
        self.w.gallery.get().refresh_model(rows);
    }

    /// Read the broker's topic list: when the user asks, or when the Topics tab
    /// opens and the last read is old.
    pub fn refresh_topics(&mut self, manual: bool) {
        let fresh = self
            .topics_read
            .is_some_and(|at| at.elapsed().as_secs() < TOPICS_MIN_SECS);
        if fresh && !manual {
            return;
        }
        self.topics_read = Some(std::time::Instant::now());
        match self.broker.topics() {
            Topics::Online(rows) => {
                self.ex.set_broker_topics(&rows);
                self.refill_topics();
                self.note(&format!("{} topics read from the broker", rows.len()));
            }
            Topics::Offline(code) => {
                self.note(&format!("topics broker: {}", fabric::errno_text(code)));
            }
        }
    }

    pub fn tree_select(&mut self, ui: &mut Ui<crate::msg::Msg>, row: usize) {
        if let Some(node) = self.ex.node_at(row) {
            let id = node.id.clone();
            self.select_node(ui, id);
        }
    }

    pub fn live_select(&mut self, ui: &mut Ui<crate::msg::Msg>, row: usize) {
        let Some(name) = self.ex.live_names.get(row).cloned() else {
            return;
        };
        if let Some(interface) = self.ex.interface_of_service(&name) {
            self.select_node(ui, NodeId::interface(interface));
        }
        self.selected_service = Some(name);
    }

    /// Double click on a live row: show its interface in the tree.
    pub fn live_activate(&mut self, ui: &mut Ui<crate::msg::Msg>, row: usize) {
        self.live_select(ui, row);
        if let Sel::Node(id) = self.sel.clone() {
            self.ex.reveal(&id);
            self.refill_tree();
            self.explorer_tab(ui, 0);
        }
    }

    pub fn topic_select(&mut self, ui: &mut Ui<crate::msg::Msg>, row: usize) {
        let Some(label) = self.ex.topic_labels.get(row).cloned() else {
            return;
        };
        let declared = self
            .ex
            .topic_entry(&label)
            .and_then(|t| t.declared)
            .map(|d| NodeId::topic(d.interface.name, d.topic.pattern));
        match declared {
            Some(id) => self.select_node(ui, id),
            None => {
                self.close_form_quiet();
                self.sel = Sel::LiveTopic(label.clone());
                let blocks = vec![
                    Block::Title(label.clone()),
                    Block::Paragraph(
                        "No interface declares this topic, so its payload is bytes. Listen shows them as a blob.".to_string(),
                    ),
                    Block::Heading("Permissions".to_string()),
                    Block::List(xui_playground::live::permissions(&label)),
                ];
                self.show_blocks(&blocks);
                self.enable_buttons();
            }
        }
    }

    pub fn gallery_select(&mut self, ui: &mut Ui<crate::msg::Msg>, row: usize) {
        let _ = ui;
        let Some(id) = self.ex.gallery_ids.get(row).copied() else {
            return;
        };
        let Some(example) = self.ex.example(id) else {
            return;
        };
        let mut blocks = vec![Block::Title(example.title.clone())];
        let mut facts = vec![format!("Section: {}", example.section.label())];
        if !example.needs.is_empty() {
            let needs: Vec<&str> = example.needs.iter().map(|n| n.label()).collect();
            facts.push(format!("Needs: {}", needs.join(", ")));
        }
        if let Some(after) = &example.after {
            facts.push(format!("Follows example {after} (run it first)"));
        }
        if example.runs_until_stopped() {
            facts.push("Keeps running until you press Stop".to_string());
        }
        blocks.push(Block::List(facts));
        if !example.expect.is_empty() {
            blocks.push(Block::Heading("Expected output".to_string()));
            blocks.push(Block::List(example.expect.clone()));
        }
        blocks.push(Block::Code {
            label: "Script".to_string(),
            text: example.source.lines().take(40).collect::<Vec<_>>().join("\n"),
        });
        blocks.push(Block::Chips(vec![Chip {
            kind: ChipKind::Info,
            text: "Load example puts the script in the editor; it runs only when you press Run."
                .to_string(),
        }]));
        self.close_form_quiet();
        self.sel = Sel::Example(id);
        self.show_blocks(&blocks);
        self.enable_buttons();
    }

    // ---- the doc pane ----------------------------------------------------

    pub fn select_node(&mut self, ui: &mut Ui<crate::msg::Msg>, id: NodeId) {
        let _ = ui;
        let catalog = self.ex.catalog.clone();
        let Some(node) = catalog.get(&id) else {
            return;
        };
        self.close_form_quiet();
        let blocks = docs::blocks(&catalog, node);
        trace::line!(Channel::Playground, "OPEN:{}", node.item.interface().name);
        self.show_blocks(&blocks);
        self.sel = Sel::Node(id);
        self.enable_buttons();
    }

    pub fn show_blocks(&mut self, blocks: &[Block]) {
        let layout = column().child(docview::view(blocks).bind(&self.w.doc_scroll).fill(1));
        let _ = self.w.doc.get().set_layout(layout);
        self.probe_soon();
    }

    pub fn show_help(&mut self, ui: &mut Ui<crate::msg::Msg>) {
        let _ = ui;
        self.close_form_quiet();
        self.sel = Sel::Help;
        let layout = column()
            .child(xui_app::devtool::help_view(&crate::help::page()).bind(&self.w.doc_scroll).fill(1));
        let _ = self.w.doc.get().set_layout(layout);
        self.enable_buttons();
        self.probe_soon();
    }

    /// The method the selection is, if it is one.
    pub fn sel_method(&self) -> Option<(&'static Interface, &'static Method)> {
        let Sel::Node(id) = &self.sel else {
            return None;
        };
        match self.ex.catalog.get(id)?.item {
            Item::Method(i, m) => Some((i, m)),
            _ => None,
        }
    }

    pub fn sel_interface(&self) -> Option<&'static Interface> {
        let Sel::Node(id) = &self.sel else {
            return None;
        };
        Some(self.ex.catalog.get(id)?.item.interface())
    }

    pub fn sel_topic(&self) -> Option<(&'static Interface, &'static Topic)> {
        let Sel::Node(id) = &self.sel else {
            return None;
        };
        match self.ex.catalog.get(id)?.item {
            Item::Topic(i, t) => Some((i, t)),
            _ => None,
        }
    }

    pub fn enable_buttons(&self) {
        let method = self.sel_method().is_some_and(|(i, m)| scriptable(i, m));
        let interface = self.sel_interface().is_some();
        let declared = self.sel_topic().is_some();
        let any_topic = declared || matches!(self.sel, Sel::LiveTopic(_));
        let example = matches!(self.sel, Sel::Example(_));
        let a = &self.w.actions;
        a.insert_call.get().set_enabled(method);
        a.call_form.get().set_enabled(method);
        a.serve.get().set_enabled(interface);
        a.connect.get().set_enabled(interface);
        a.listen.get().set_enabled(any_topic);
        a.peek.get().set_enabled(any_topic);
        a.publish.get().set_enabled(any_topic);
        a.insert_on.get().set_enabled(declared);
        a.load.get().set_enabled(example);
        let open = self.form.is_some();
        self.w.form_call.get().set_enabled(open);
        self.w.form_insert.get().set_enabled(open);
        self.w.form_close.get().set_enabled(open);
    }

    pub fn open_link(&mut self, path: &str) {
        if let Err(e) = xui_app::platform::launcher::open_path(path) {
            self.note(&format!("cannot open {path}: {e}"));
        }
    }

    // ---- the buttons -----------------------------------------------------

    pub fn act(&mut self, ui: &mut Ui<crate::msg::Msg>, action: Action) {
        match action {
            Action::InsertCall => {
                if let Some((i, m)) = self.sel_method() {
                    match snippet::call(i, m, &CallOptions::default()) {
                        Ok(s) => self.append_snippet(&s.text, &s.notes),
                        Err(e) => self.note(&e.to_string()),
                    }
                }
            }
            Action::CallForm => self.open_form(ui),
            Action::ServeSkeleton => {
                if let Some(i) = self.sel_interface() {
                    let s = snippet::serve(i);
                    self.append_snippet(&s.text, &s.notes);
                }
            }
            Action::Connect => {
                if let Some(i) = self.sel_interface() {
                    let name = self.selected_service.clone().unwrap_or_else(|| i.default_service().to_string());
                    let s = snippet::connect(i, &name);
                    self.append_snippet(&s.text, &s.notes);
                }
            }
            Action::Listen => {
                if let Some(filter) = self.topic_filter_text() {
                    self.topics_request(ui, Request::Listen(filter));
                }
            }
            Action::Peek => {
                let text = match (self.sel_topic(), &self.sel) {
                    (Some((i, t)), _) => Some(snippet::topic(i, t, TopicAction::Peek)),
                    (None, Sel::LiveTopic(name)) => Some(snippet::raw_topic(name, TopicAction::Peek)),
                    _ => None,
                };
                if let Some(s) = text {
                    self.start_run(&s.text, 1, "peek");
                }
            }
            Action::Publish => {
                if let Some((topic, payload)) = self.publish_fields() {
                    self.topics_request(ui, Request::FillPublish { topic, payload });
                }
            }
            Action::InsertOn => {
                if let Some((i, t)) = self.sel_topic() {
                    let s = snippet::topic(i, t, TopicAction::On);
                    self.append_snippet(&s.text, &s.notes);
                }
            }
            Action::LoadExample => self.load_example(),
        }
    }

    /// The topic filter the selection stands for.
    fn topic_filter_text(&self) -> Option<String> {
        match (&self.sel, self.sel_topic()) {
            (_, Some((_, t))) => Some(t.pattern.to_string()),
            (Sel::LiveTopic(name), _) => Some(name.clone()),
            _ => None,
        }
    }

    /// Append `text` to the editor (a blank line apart), typed provenance:
    /// it is the app's text next to the user's own.
    pub fn append_snippet(&mut self, text: &str, notes: &[String]) {
        let mut next = self.text.clone();
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        if !next.is_empty() {
            next.push('\n');
        }
        next.push_str(text);
        self.set_buffer(&next, None);
        if let Some(note) = notes.first() {
            self.note(note);
        }
        let lines = next.lines().count();
        self.editor.goto(lines.saturating_sub(1), 0);
    }

    /// Replace the buffer. `from` is where a whole new text came from; `None`
    /// keeps the buffer the user's (the app added to it).
    pub fn set_buffer(&mut self, text: &str, from: Option<Provenance>) {
        self.reveal_bottom(true);
        self.text = text.to_string();
        self.editor.set_text(text);
        match from {
            Some(from) => self.gate.arrive(from, text),
            None => self.gate.edited(text),
        }
        crate::paste::vouch(&self.gate, &mut self.known, text);
        self.dirty = from.is_none_or(|f| f != Provenance::Gallery && f != Provenance::File);
        self.update_file_label();
    }

    fn load_example(&mut self) {
        let Sel::Example(id) = self.sel.clone() else {
            return;
        };
        let Some(example) = self.ex.example(id) else {
            return;
        };
        let source = example.source.to_string();
        let title = example.title.clone();
        self.file = None;
        self.set_buffer(&source, Some(Provenance::Gallery));
        self.file_label_override = Some(format!("Gallery: {title}"));
        self.update_file_label();
        self.editor.goto(0, 0);
        self.focus_editor();
        self.note(&format!("Loaded '{title}'. It runs only when you press Run."));
    }
}

/// `os.lazy.confd.v1` -> `confd`.
pub fn short(interface: &str) -> String {
    interface
        .strip_prefix("os.lazy.")
        .unwrap_or(interface)
        .trim_end_matches(".v1")
        .to_string()
}
