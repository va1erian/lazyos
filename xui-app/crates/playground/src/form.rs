//! The call form and the publish form: a payload type as a tree of controls,
//! and back and forth between the controls and Rhai value text.
//!
//! The form owns no widgets. It is the model the window draws (a text box per
//! `Text`, a combo per `Enum`, a toggle per `Option`, rows for an `Array`);
//! its text is what [`Form::argument_texts`] turns into the arguments of a
//! generated call. A value a form accepts is always one the codec accepts: a
//! test checks that over every method of the table.

use rhai::{Array, Blob, Dynamic, ImmutableString, Map};
use rhai_lazy::msg::schema::{Field, Interface, Method, Topic, Ty};

use crate::form_parse::{format_hex, int_literal, int_range, parse_float, parse_hex, parse_int};
use crate::show;
use crate::util::{rhai_str, ty_label, Iface};

/// Structs nest at most this deep in a form before a field is disabled.
const MAX_DEPTH: u8 = 8;

/// How a bytes field is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytesMode {
    /// Hexadecimal pairs.
    Hex,
    /// Text, sent as its UTF-8 bytes.
    Utf8,
}

/// One input.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    Text(String),
    Bool(bool),
    Int {
        text: String,
        min: i128,
        max: i128,
    },
    Float(String),
    Bytes {
        text: String,
        mode: BytesMode,
    },
    /// A combo of variant names.
    Enum {
        name: &'static str,
        variants: Vec<&'static str>,
        selected: usize,
    },
    /// A toggle: off sends `()`.
    Option {
        on: bool,
        inner: Box<Control>,
    },
    /// Rows of one element type.
    Array {
        interface: Iface,
        elem: Ty,
        items: Vec<Control>,
    },
    /// A struct's fields.
    Group(Vec<FormField>),
    /// A kernel object (a channel, a shared buffer) a script cannot make.
    Disabled {
        reason: String,
    },
}

/// A named control with its type, as the label reads.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub name: String,
    pub ty: Ty,
    pub control: Control,
}

impl FormField {
    /// The type as the IDL spells it, for the label.
    pub fn type_label(&self) -> String {
        ty_label(self.ty)
    }
}

/// A form value that cannot be sent, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormError {
    /// The field path, `value.kind` or `entries[2].name`.
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for FormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

type Res<T> = Result<T, FormError>;

fn err(path: &str, message: impl Into<String>) -> FormError {
    FormError {
        path: path.to_string(),
        message: message.into(),
    }
}

impl Control {
    /// The control for a value of `ty`, at its zero value.
    pub fn default_for(interface: &'static Interface, ty: Ty) -> Control {
        Self::build(interface, ty, 0)
    }

    fn build(interface: &'static Interface, ty: Ty, depth: u8) -> Control {
        match ty {
            Ty::Bool => Control::Bool(false),
            Ty::I32 | Ty::U32 | Ty::I64 | Ty::U64 => {
                let (min, max) = int_range(ty).unwrap_or((0, 0));
                Control::Int {
                    text: "0".to_string(),
                    min,
                    max,
                }
            }
            Ty::F64 => Control::Float("0".to_string()),
            Ty::String => Control::Text(String::new()),
            Ty::Bytes => Control::Bytes {
                text: String::new(),
                mode: BytesMode::Hex,
            },
            Ty::Channel => Control::Disabled {
                reason: "a channel end is a kernel object; a script cannot create one".to_string(),
            },
            Ty::Buffer => Control::Disabled {
                reason: "a shared buffer is a kernel object; a script cannot create one"
                    .to_string(),
            },
            Ty::Enum(name) => match interface.find_enum(name) {
                Some(def) if !def.variants.is_empty() => Control::Enum {
                    name: def.name,
                    variants: def.variants.to_vec(),
                    selected: 0,
                },
                _ => Control::Disabled {
                    reason: format!("enum {name} has no variants in the table"),
                },
            },
            Ty::Array(elem) => Control::Array {
                interface: Iface(interface),
                elem: *elem,
                items: Vec::new(),
            },
            Ty::Option(inner) => Control::Option {
                on: false,
                inner: Box::new(if depth >= MAX_DEPTH {
                    Control::Disabled {
                        reason: "nested too deeply".to_string(),
                    }
                } else {
                    Self::build(interface, *inner, depth + 1)
                }),
            },
            Ty::Struct(name) => match interface.find_struct(name) {
                Some(def) if depth < MAX_DEPTH => Control::Group(
                    def.fields
                        .iter()
                        .map(|f| FormField::build(interface, f, depth + 1))
                        .collect(),
                ),
                Some(_) => Control::Disabled {
                    reason: "nested too deeply".to_string(),
                },
                None => Control::Disabled {
                    reason: format!("struct {name} is not in the table"),
                },
            },
        }
    }

    /// Whether the control (or one it holds that is switched on) is a
    /// disabled one: the form cannot be sent.
    pub fn blocked(&self) -> Option<&str> {
        match self {
            Control::Disabled { reason } => Some(reason),
            Control::Option { on: true, inner } => inner.blocked(),
            Control::Array { items, .. } => items.iter().find_map(Control::blocked),
            Control::Group(fields) => fields.iter().find_map(|f| f.control.blocked()),
            _ => None,
        }
    }

    /// Append a row to an `Array` control.
    pub fn add_row(&mut self) {
        if let Control::Array {
            interface,
            elem,
            items,
        } = self
        {
            items.push(Control::default_for(interface.0, *elem));
        }
    }

    /// Remove row `index` of an `Array` control.
    pub fn remove_row(&mut self, index: usize) {
        if let Control::Array { items, .. } = self {
            if index < items.len() {
                items.remove(index);
            }
        }
    }

    /// The Rhai source of the control's value.
    pub fn to_rhai(&self) -> Result<String, FormError> {
        self.source("", "")
    }

    fn source(&self, path: &str, pad: &str) -> Res<String> {
        match self {
            Control::Text(text) => Ok(rhai_str(text)),
            Control::Bool(value) => Ok(value.to_string()),
            Control::Int { text, min, max } => parse_int(text, *min, *max)
                .map(int_literal)
                .map_err(|m| err(path, m)),
            Control::Float(text) => parse_float(text)
                .map(|f| show::to_rhai(&Dynamic::from_float(f)))
                .map_err(|m| err(path, m)),
            Control::Bytes { text, mode } => {
                let bytes = match mode {
                    BytesMode::Hex => parse_hex(text).map_err(|m| err(path, m))?,
                    BytesMode::Utf8 => text.as_bytes().to_vec(),
                };
                Ok(show::to_rhai(&Dynamic::from_blob(bytes)))
            }
            Control::Enum {
                variants, selected, ..
            } => variants
                .get(*selected)
                .map(|v| rhai_str(v))
                .ok_or_else(|| err(path, "choose a variant")),
            Control::Option { on, inner } => {
                if *on {
                    inner.source(path, pad)
                } else {
                    Ok("()".to_string())
                }
            }
            Control::Array { items, .. } => {
                let inner = format!("{pad}    ");
                let parts = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| item.source(&format!("{path}[{i}]"), &inner))
                    .collect::<Res<Vec<_>>>()?;
                Ok(show::join_container("[", "]", &parts, pad))
            }
            Control::Group(fields) => {
                let inner = format!("{pad}    ");
                let parts = fields
                    .iter()
                    .map(|f| {
                        let at = if path.is_empty() {
                            f.name.clone()
                        } else {
                            format!("{path}.{}", f.name)
                        };
                        f.control
                            .source(&at, &inner)
                            .map(|text| format!("{}: {text}", f.name))
                    })
                    .collect::<Res<Vec<_>>>()?;
                Ok(show::join_container("#{", "}", &parts, pad))
            }
            Control::Disabled { reason } => Err(err(path, reason.clone())),
        }
    }

    /// The control holding `value`, as the form would show a value the
    /// service sent or a snippet carried. A value of the wrong shape gives
    /// the type's default.
    pub fn from_value(interface: &'static Interface, ty: Ty, value: &Dynamic) -> Control {
        Self::fill(interface, ty, value, 0)
    }

    fn fill(interface: &'static Interface, ty: Ty, value: &Dynamic, depth: u8) -> Control {
        let default = Self::build(interface, ty, depth);
        match (ty, default) {
            (Ty::Bool, Control::Bool(_)) => Control::Bool(value.as_bool().unwrap_or(false)),
            (_, Control::Int { min, max, .. }) => Control::Int {
                text: value.as_int().unwrap_or(0).to_string(),
                min,
                max,
            },
            (Ty::F64, Control::Float(_)) => Control::Float(
                value
                    .as_float()
                    .or_else(|_| value.as_int().map(|n| n as f64))
                    .map_or_else(
                        |_| "0".to_string(),
                        |f| show::to_rhai(&Dynamic::from_float(f)),
                    ),
            ),
            (Ty::String, Control::Text(_)) => Control::Text(
                value
                    .read_lock::<ImmutableString>()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            ),
            (Ty::Bytes, Control::Bytes { .. }) => Control::Bytes {
                text: value
                    .read_lock::<Blob>()
                    .map(|b| format_hex(&b))
                    .unwrap_or_default(),
                mode: BytesMode::Hex,
            },
            (Ty::Enum(_), Control::Enum { name, variants, .. }) => {
                let selected = value
                    .read_lock::<ImmutableString>()
                    .and_then(|s| variants.iter().position(|v| *v == s.as_str()))
                    .or_else(|| {
                        value
                            .as_int()
                            .ok()
                            .and_then(|n| usize::try_from(n).ok())
                            .filter(|n| *n < variants.len())
                    })
                    .unwrap_or(0);
                Control::Enum {
                    name,
                    variants,
                    selected,
                }
            }
            (Ty::Option(inner), Control::Option { inner: slot, .. }) => {
                if value.is_unit() {
                    Control::Option {
                        on: false,
                        inner: slot,
                    }
                } else {
                    Control::Option {
                        on: true,
                        inner: Box::new(Self::fill(interface, *inner, value, depth + 1)),
                    }
                }
            }
            (Ty::Array(elem), Control::Array { .. }) => Control::Array {
                interface: Iface(interface),
                elem: *elem,
                items: value
                    .read_lock::<Array>()
                    .map(|items| {
                        items
                            .iter()
                            .map(|v| Self::fill(interface, *elem, v, depth + 1))
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            (Ty::Struct(name), Control::Group(fields)) => {
                let map = value.read_lock::<Map>();
                let def = interface.find_struct(name);
                let filled = fields
                    .into_iter()
                    .zip(def.map_or(&[][..], |d| d.fields))
                    .map(|(slot, field)| FormField {
                        control: match map.as_ref().and_then(|m| m.get(field.name)) {
                            Some(v) => Self::fill(interface, field.ty, v, depth + 1),
                            None => slot.control,
                        },
                        ..slot
                    })
                    .collect();
                Control::Group(filled)
            }
            (_, other) => other,
        }
    }
}

impl FormField {
    fn build(interface: &'static Interface, field: &Field, depth: u8) -> FormField {
        FormField {
            name: field.name.to_string(),
            ty: field.ty,
            control: Control::build(interface, field.ty, depth),
        }
    }
}

/// A form over a list of parameters (a method's request, a topic payload).
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub interface: Iface,
    pub fields: Vec<FormField>,
    /// A payload that is one bare value, not a struct's fields (an enum
    /// topic, or raw bytes): `argument_texts` has one entry.
    pub bare: bool,
}

impl Form {
    /// The form of a method's request.
    pub fn for_method(interface: &'static Interface, method: &Method) -> Form {
        Form {
            interface: Iface(interface),
            fields: method
                .params
                .iter()
                .map(|f| FormField::build(interface, f, 0))
                .collect(),
            bare: false,
        }
    }

    /// The form of a declared topic's payload: a struct's fields, or one
    /// combo for an enum.
    pub fn for_topic(interface: &'static Interface, topic: &Topic) -> Form {
        if let Some(def) = interface.find_struct(topic.payload) {
            return Form {
                interface: Iface(interface),
                fields: def
                    .fields
                    .iter()
                    .map(|f| FormField::build(interface, f, 1))
                    .collect(),
                bare: false,
            };
        }
        let ty = interface
            .find_enum(topic.payload)
            .map_or(Ty::String, |e| Ty::Enum(e.name));
        Form {
            interface: Iface(interface),
            fields: vec![FormField {
                name: "payload".to_string(),
                ty,
                control: Control::build(interface, ty, 0),
            }],
            bare: true,
        }
    }

    /// The form for a topic nothing declares: its payload is text or bytes.
    pub fn raw(interface: &'static Interface) -> Form {
        Form {
            interface: Iface(interface),
            fields: vec![FormField {
                name: "payload".to_string(),
                ty: Ty::String,
                control: Control::Text(String::new()),
            }],
            bare: true,
        }
    }

    /// The reason the form cannot be sent, if a switched-on control is
    /// disabled.
    pub fn blocked(&self) -> Option<String> {
        self.fields.iter().find_map(|f| {
            f.control
                .blocked()
                .map(|reason| format!("{}: {reason}", f.name))
        })
    }

    /// The Rhai text of each field's value, in order.
    pub fn argument_texts(&self) -> Res<Vec<String>> {
        self.fields
            .iter()
            .map(|f| f.control.source(&f.name, ""))
            .collect()
    }

    /// The payload as one Rhai value: the map of the fields, or the bare
    /// value.
    pub fn payload_text(&self) -> Res<String> {
        if self.bare {
            return self.fields[0].control.source("payload", "");
        }
        Control::Group(self.fields.clone()).to_rhai()
    }

    /// Set the fields from a Rhai map (named arguments or a payload).
    pub fn fill_from(&mut self, values: &Map) {
        let interface = self.interface.0;
        for field in &mut self.fields {
            if let Some(value) = values.get(field.name.as_str()) {
                field.control = Control::from_value(interface, field.ty, value);
            }
        }
    }
}
