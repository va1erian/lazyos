//! The Call form: a method's request as controls, and the edits that change
//! it (`docs/messenger-playground-plan.md` section 3, "Actions the browser
//! turns into script").
//!
//! [`xui_playground::form`] is the model (a tree of controls that turns into
//! Rhai text the codec accepts). This module walks that tree into flat
//! [`Row`]s, applies a [`FormEdit`] by the index path a row carries, and
//! draws the rows. A structural edit (an `Option` toggle, an array row) asks
//! for a new layout; a text edit does not.

use xui_core::arrange::{
    button, checkbox, column, combo_box, edit, flow_text, label, row, scroll, Align, Entry,
    Handle, IntoEntry, Layout, LayoutExt, ScrollBuild,
};
use xui_core::widget::{Edit, Run};
use xui_core::{Dip, Insets};
use xui_playground::form::{Control, Form};

use crate::msg::{FormEdit, Msg};

/// One control of the form, flattened.
pub struct Row {
    pub path: Vec<usize>,
    pub label: String,
    pub indent: u16,
    pub leaf: Leaf,
}

pub enum Leaf {
    /// Text typed in a box (strings, numbers, bytes).
    Text(String),
    Check(bool),
    Pick { variants: Vec<String>, selected: usize },
    /// A struct or array heading; `add` offers a new array row.
    Header { add: bool },
    /// The row's control is an array element that can be removed.
    Removable(String),
    /// A kernel object or too deep: shown, not editable.
    Note(String),
}

/// The form flattened in reading order.
pub fn rows(form: &Form) -> Vec<Row> {
    let mut out = Vec::new();
    for (i, field) in form.fields.iter().enumerate() {
        walk(&field.control, vec![i], &field.name, 0, &mut out);
    }
    out
}

fn walk(control: &Control, path: Vec<usize>, name: &str, indent: u16, out: &mut Vec<Row>) {
    let push = |out: &mut Vec<Row>, leaf: Leaf| {
        out.push(Row {
            path: path.clone(),
            label: name.to_string(),
            indent,
            leaf,
        })
    };
    match control {
        Control::Text(t) => push(out, Leaf::Text(t.clone())),
        Control::Int { text, .. } | Control::Float(text) | Control::Bytes { text, .. } => {
            push(out, Leaf::Text(text.clone()))
        }
        Control::Bool(b) => push(out, Leaf::Check(*b)),
        Control::Enum {
            variants, selected, ..
        } => push(
            out,
            Leaf::Pick {
                variants: variants.iter().map(|v| v.to_string()).collect(),
                selected: *selected,
            },
        ),
        Control::Option { on, inner } => {
            out.push(Row {
                path: path.clone(),
                label: format!("{name} (send a value; off sends ())"),
                indent,
                leaf: Leaf::Check(*on),
            });
            if *on {
                let mut inner_path = path.clone();
                inner_path.push(0);
                walk(inner, inner_path, name, indent + 1, out);
            }
        }
        Control::Group(fields) => {
            push(out, Leaf::Header { add: false });
            for (j, f) in fields.iter().enumerate() {
                let mut p = path.clone();
                p.push(j);
                walk(&f.control, p, &f.name, indent + 1, out);
            }
        }
        Control::Array { items, .. } => {
            push(out, Leaf::Header { add: true });
            for (k, item) in items.iter().enumerate() {
                let mut p = path.clone();
                p.push(k);
                out.push(Row {
                    path: p.clone(),
                    label: format!("{name}[{k}]"),
                    indent: indent + 1,
                    leaf: Leaf::Removable(format!("{name}[{k}]")),
                });
                walk(item, p, &format!("{name}[{k}]"), indent + 2, out);
            }
        }
        Control::Disabled { reason } => push(out, Leaf::Note(reason.clone())),
    }
}

/// Whether the form has a text box (the one `view` binds to its `first` handle).
pub fn has_text_row(form: &Form) -> bool {
    rows(form).iter().any(|r| matches!(r.leaf, Leaf::Text(_)))
}

/// The control at `path`.
fn at<'a>(form: &'a mut Form, path: &[usize]) -> Option<&'a mut Control> {
    let (first, rest) = path.split_first()?;
    let mut control = &mut form.fields.get_mut(*first)?.control;
    for &step in rest {
        control = match control {
            Control::Group(fields) => &mut fields.get_mut(step)?.control,
            Control::Option { inner, .. } => inner,
            Control::Array { items, .. } => items.get_mut(step)?,
            _ => return None,
        };
    }
    Some(control)
}

/// Apply `edit`. True when the set of rows changed (rebuild the layout).
pub fn apply(form: &mut Form, edit: &FormEdit) -> bool {
    match edit {
        FormEdit::Text(path, text) => {
            if let Some(control) = at(form, path) {
                match control {
                    Control::Text(t) => *t = text.clone(),
                    Control::Int { text: t, .. }
                    | Control::Float(t)
                    | Control::Bytes { text: t, .. } => *t = text.clone(),
                    _ => {}
                }
            }
            false
        }
        FormEdit::Check(path, on) => {
            match at(form, path) {
                Some(Control::Bool(b)) => {
                    *b = *on;
                    false
                }
                Some(Control::Option { on: slot, .. }) => {
                    *slot = *on;
                    true
                }
                _ => false,
            }
        }
        FormEdit::Pick(path, index) => {
            if let Some(Control::Enum { selected, variants, .. }) = at(form, path) {
                if *index < variants.len() {
                    *selected = *index;
                }
            }
            false
        }
        FormEdit::AddRow(path) => {
            if let Some(control) = at(form, path) {
                control.add_row();
            }
            true
        }
        FormEdit::RemoveRow(path) => {
            if let Some((&k, parent)) = path.split_last() {
                if let Some(control) = at(form, parent) {
                    control.remove_row(k);
                }
            }
            true
        }
    }
}

const LABEL_WIDTH: i32 = 150;
const INDENT: f32 = 12.0;

/// The label column of a row, pushed right by its depth.
fn name_cell(r: &Row, text: &str, caption: bool) -> Entry<Msg> {
    let mut name = label(text);
    if caption {
        name = name.caption();
    }
    column()
        .padding(Insets::new(
            Dip(INDENT * f32::from(r.indent)),
            Dip(0.0),
            Dip(0.0),
            Dip(0.0),
        ))
        .child(name)
        .width(LABEL_WIDTH + 12 * i32::from(r.indent))
}

fn row_of(r: &Row, control: Entry<Msg>) -> Layout<Msg> {
    row()
        .gap(8)
        .align(Align::Center)
        .children((name_cell(r, &r.label, false), control))
}

fn line(r: &Row, first: &Handle<Edit<Msg>>, bound: &mut bool) -> Layout<Msg> {
    let path = r.path.clone();
    match &r.leaf {
        Leaf::Text(text) => {
            let mut field = edit()
                .text(text.clone())
                .on_change(move |s| Msg::Form(FormEdit::Text(path.clone(), s)));
            if !*bound {
                field = field.bind(first);
                *bound = true;
            }
            row_of(r, field.fill(1))
        }
        Leaf::Check(on) => column().child(
            checkbox(r.label.clone())
                .checked(*on)
                .on_toggle(move |b| Msg::Form(FormEdit::Check(path.clone(), b))),
        ),
        Leaf::Pick { variants, selected } => {
            let items: Vec<&str> = variants.iter().map(String::as_str).collect();
            let selected = *selected;
            row_of(
                r,
                combo_box(&items)
                    .then(move |combo| {
                        combo.select(selected);
                        combo
                    })
                    .on_select(move |i| Msg::Form(FormEdit::Pick(path.clone(), i)))
                    .fill(1),
            )
        }
        Leaf::Header { add } => {
            let head = format!("{} ({})", r.label, if *add { "rows" } else { "fields" });
            let mut out = row().gap(8).align(Align::Center).child(name_cell(r, &head, true));
            if *add {
                out = out.child(button("Add row").on_click(Msg::Form(FormEdit::AddRow(path))));
            }
            out
        }
        Leaf::Removable(name) => row().gap(8).align(Align::Center).children((
            name_cell(r, name, true),
            button("Remove")
                .on_click(Msg::Form(FormEdit::RemoveRow(path)))
                .into_entry(),
        )),
        Leaf::Note(reason) => column().child(
            flow_text().run(Run::normal(format!("{}: {reason}", r.label))),
        ),
    }
}

/// The form's body. `first` is bound to the first text box so the window can
/// focus it. `heading` and `error` are the lines above and below the rows.
pub fn view(
    heading: &str,
    form: &Form,
    error: Option<&str>,
    first: &Handle<Edit<Msg>>,
) -> ScrollBuild<Msg> {
    let mut body = column().padding(10).gap(6).child(label(heading).title());
    let mut bound = false;
    let rows = rows(form);
    if rows.is_empty() {
        body = body.child(flow_text().run(Run::normal("This call takes no arguments.")));
    }
    for r in &rows {
        body = body.child(line(r, first, &mut bound));
    }
    if let Some(blocked) = form.blocked() {
        body = body.child(flow_text().run(Run::normal(format!("Cannot send: {blocked}"))));
    }
    if let Some(error) = error {
        body = body.child(flow_text().run(Run::normal(format!("Not sent: {error}")).weight(700)));
    }
    scroll(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_playground::catalog::Catalog;
    use xui_playground::catalog::Item;

    fn method_form(interface: &str, method: &str) -> Form {
        let catalog = Catalog::from_schema();
        let node = catalog
            .get(&xui_playground::catalog::NodeId::method(interface, method))
            .expect("method");
        match node.item {
            Item::Method(i, m) => Form::for_method(i, m),
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_confd_set_form_has_a_text_row_per_leaf_and_edits_reach_the_text() {
        let mut form = method_form("os.lazy.confd.v1", "Get");
        let flat = rows(&form);
        assert!(!flat.is_empty());
        let path = flat[0].path.clone();
        assert!(!apply(&mut form, &FormEdit::Text(path, "sys/ui/theme".into())));
        let texts = form.argument_texts().unwrap();
        assert!(texts[0].contains("sys/ui/theme"));
    }

    #[test]
    fn option_toggles_and_array_rows_ask_for_a_new_layout() {
        let mut form = method_form("os.lazy.confd.v1", "Get");
        let before = rows(&form).len();
        let again = rows(&form).len();
        assert_eq!(before, again);
        assert!(!apply(&mut form, &FormEdit::Pick(vec![99], 0)));
    }
}
