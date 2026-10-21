//! The Call form's behaviour: open it for the selected method, apply edits,
//! and Call (run the generated line once, `Output::Show`) or Insert it.

use xui_core::app::Ui;
use xui_core::arrange::{column, LayoutExt};
use xui_playground::docs;
use xui_playground::form::Form;
use xui_playground::snippet::{self, CallOptions, Output};

use crate::app::{OpenForm, Playground, Sel};
use crate::msg::FormEdit;
use crate::selection::short;
use crate::{callform, msg};

impl Playground {
    pub fn open_form(&mut self, ui: &mut Ui<msg::Msg>) {
        let _ = ui;
        let Some((i, m)) = self.sel_method() else {
            return;
        };
        if let Err(e) = snippet::call(i, m, &CallOptions::default()) {
            self.note(&e.to_string());
            return;
        }
        self.form = Some(OpenForm {
            interface: i.name,
            method: m.name,
            form: Form::for_method(i, m),
            error: None,
        });
        self.render_form();
        self.enable_buttons();
    }

    pub fn render_form(&mut self) {
        let Some(open) = &self.form else {
            return;
        };
        let heading = format!("{}.{}", open.interface, open.method);
        let layout = column().child(
            callform::view(
                &heading,
                &open.form,
                open.error.as_deref(),
                &self.form_first,
            )
            .bind(&self.w.doc_scroll)
            .fill(1),
        );
        let _ = self.w.doc.get().set_layout(layout);
        if callform::has_text_row(&open.form) {
            self.form_first.get().focus();
        }
        self.probe_soon();
    }

    pub fn form_edit(&mut self, ui: &mut Ui<msg::Msg>, edit: &FormEdit) {
        let _ = ui;
        let Some(open) = self.form.as_mut() else {
            return;
        };
        open.error = None;
        if callform::apply(&mut open.form, edit) {
            self.render_form();
        }
    }

    /// Call: run the generated line once and show the reply tree in Output.
    pub fn form_call(&mut self, ui: &mut Ui<msg::Msg>) {
        let Some((i, m)) = self.sel_method() else {
            return;
        };
        let Some(open) = self.form.as_mut() else {
            return;
        };
        let texts = match open.form.argument_texts() {
            Ok(t) => t,
            Err(e) => {
                open.error = Some(e.to_string());
                self.render_form();
                return;
            }
        };
        let options = CallOptions {
            output: Output::Show,
            service: self
                .selected_service
                .as_deref()
                .filter(|s| self.ex.join.service(s).is_some_and(|svc| svc.serves(i))),
            ..CallOptions::default()
        };
        match snippet::call_with(i, m, &options, &texts) {
            Ok(s) => {
                let title = format!("{}.{}", short(i.name), m.name);
                let risky = docs::risks(i, m);
                if risky.contains(&docs::Risk::StopsServices) {
                    self.confirm_call(ui, s.text, title, format!("{}.{}", i.name, m.name));
                } else if let Some(id) = self.start_run(&s.text, 1, &title) {
                    self.calls.push((id, format!("{}.{}", i.name, m.name)));
                }
            }
            Err(e) => {
                if let Some(open) = self.form.as_mut() {
                    open.error = Some(e.to_string());
                }
                self.render_form();
            }
        }
    }

    pub fn form_insert(&mut self) {
        let Some((i, m)) = self.sel_method() else {
            return;
        };
        let Some(open) = self.form.as_ref() else {
            return;
        };
        match open.form.argument_texts() {
            Ok(texts) => match snippet::call_with(i, m, &CallOptions::default(), &texts) {
                Ok(s) => self.append_snippet(&s.text, &s.notes),
                Err(e) => self.note(&e.to_string()),
            },
            Err(e) => {
                if let Some(open) = self.form.as_mut() {
                    open.error = Some(e.to_string());
                }
                self.render_form();
            }
        }
    }

    pub fn form_close(&mut self, ui: &mut Ui<msg::Msg>) {
        let _ = ui;
        self.close_form_quiet();
        if let Sel::Node(id) = self.sel.clone() {
            let catalog = self.ex.catalog.clone();
            if let Some(node) = catalog.get(&id) {
                let blocks = docs::blocks(&catalog, node);
                self.show_blocks(&blocks);
            }
        }
        self.enable_buttons();
    }

    pub fn close_form_quiet(&mut self) {
        self.form = None;
    }
}
