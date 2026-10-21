//! Browser nodes as Rhai text (`docs/messenger-playground-plan.md` D3: every
//! action is a script). A call with typed placeholders, a serve skeleton, the
//! topic helpers, a connect to a live service.
//!
//! Placeholders are the type's zero value with a comment naming the field, so
//! the text always compiles and runs; the user edits the values. A property
//! test compiles and runs the snippet of every method in the table.

use rhai_lazy::msg::schema::{Field, Interface, Method, Topic, Ty};

use crate::rhai_names::{alias, concrete_topic, has_sugar, scriptable, sys_function, topic_helper};
use crate::util::{param_name, rhai_str, ty_label};

/// Generated text and the remarks to show beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    pub notes: Vec<String>,
}

/// Why no call text exists for a method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetError {
    /// The request carries kernel objects a script cannot create.
    Objects(Vec<String>),
    /// Only the kernel gate serves it.
    GateOnly,
    /// `Style::Sys` was asked for an interface with no `sys` module (or a
    /// method it leaves out).
    NoModule,
}

impl std::fmt::Display for SnippetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnippetError::Objects(names) => write!(
                f,
                "the request carries kernel objects ({}) a script cannot send",
                names.join(", ")
            ),
            SnippetError::GateOnly => f.write_str("only the kernel gate serves this method"),
            SnippetError::NoModule => f.write_str("this method has no sys:: function"),
        }
    }
}

/// How a call is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// `sys::` when the method has a function there, else `connect`.
    Auto,
    Sys,
    Connect,
}

/// What the generated text does with the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    None,
    /// `print(reply);`
    Print,
    /// `pg_show(reply);` (needs the runner prelude).
    Show,
}

#[derive(Debug, Clone, Copy)]
pub struct CallOptions<'a> {
    pub style: Style,
    pub output: Output,
    /// The registered name to connect to, when it is not the default.
    pub service: Option<&'a str>,
    /// The variable the reply is bound to.
    pub bind: &'a str,
}

impl Default for CallOptions<'_> {
    fn default() -> Self {
        CallOptions {
            style: Style::Auto,
            output: Output::Print,
            service: None,
            bind: "reply",
        }
    }
}

/// The placeholder for a value of `ty`: its zero value as Rhai source.
pub fn placeholder(interface: &Interface, ty: Ty) -> String {
    placeholder_at(interface, ty, 0)
}

fn placeholder_at(interface: &Interface, ty: Ty, depth: u8) -> String {
    match ty {
        Ty::Bool => "false".to_string(),
        Ty::I32 | Ty::I64 | Ty::U32 | Ty::U64 | Ty::Channel => "0".to_string(),
        Ty::F64 => "0.0".to_string(),
        Ty::String => "\"\"".to_string(),
        Ty::Bytes => "blob()".to_string(),
        Ty::Array(_) => "[]".to_string(),
        Ty::Option(_) | Ty::Buffer => "()".to_string(),
        Ty::Enum(name) => interface
            .find_enum(name)
            .and_then(|e| e.variants.first())
            .map_or_else(|| "0".to_string(), |v| rhai_str(v)),
        Ty::Struct(name) => match interface.find_struct(name) {
            Some(def) if depth < 2 && !def.fields.is_empty() => {
                let fields = def
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, placeholder_at(interface, f.ty, depth + 1)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("#{{ {fields} }}")
            }
            _ => "#{}".to_string(),
        },
    }
}

fn arg_note(field: &Field) -> String {
    format!("{}: {}", field.name, ty_label(field.ty))
}

/// The argument list: inline with `/* name: Type */` comments for up to two
/// parameters, one per line with a trailing `// name: Type` beyond that.
/// `texts` are the Rhai values, one per parameter.
fn arguments(params: &[Field], texts: &[String], open: &str, close: &str) -> String {
    if params.len() <= 2 {
        let args = params
            .iter()
            .zip(texts)
            .map(|(p, text)| format!("/* {} */ {text}", arg_note(p)))
            .collect::<Vec<_>>()
            .join(", ");
        return format!("{open}{args}{close}");
    }
    let mut out = format!(
        "{open}
"
    );
    for (i, (p, text)) in params.iter().zip(texts).enumerate() {
        let comma = if i + 1 < params.len() { "," } else { "" };
        for (n, line) in text.lines().enumerate() {
            let last = n + 1 == text.lines().count();
            if last {
                out.push_str(&format!(
                    "    {line}{comma}  // {}
",
                    arg_note(p)
                ));
            } else {
                out.push_str(&format!(
                    "    {line}
"
                ));
            }
        }
    }
    out.push_str(close);
    out
}

/// The zero-value placeholder of each parameter.
fn placeholders(interface: &Interface, params: &[Field]) -> Vec<String> {
    params
        .iter()
        .map(|p| placeholder(interface, p.ty))
        .collect()
}

fn object_names(method: &Method) -> Vec<String> {
    method.objects.iter().map(|o| o.name.to_string()).collect()
}

fn check(interface: &Interface, method: &Method) -> Result<(), SnippetError> {
    if !method.objects.is_empty() {
        return Err(SnippetError::Objects(object_names(method)));
    }
    if !scriptable(interface, method) {
        return Err(SnippetError::GateOnly);
    }
    Ok(())
}

/// A call to `method` with typed placeholders.
pub fn call(
    interface: &Interface,
    method: &Method,
    options: &CallOptions<'_>,
) -> Result<Snippet, SnippetError> {
    call_with(
        interface,
        method,
        options,
        &placeholders(interface, method.params),
    )
}

/// A call to `method` with the Rhai text of each argument, as a form
/// produced them ([`crate::form::Form::argument_texts`]).
pub fn call_with(
    interface: &Interface,
    method: &Method,
    options: &CallOptions<'_>,
    texts: &[String],
) -> Result<Snippet, SnippetError> {
    check(interface, method)?;
    assert_eq!(texts.len(), method.params.len(), "one text per parameter");
    let sys = sys_function(interface, method);
    let use_sys = match options.style {
        Style::Auto => sys.is_some(),
        Style::Sys => {
            if sys.is_none() {
                return Err(SnippetError::NoModule);
            }
            true
        }
        Style::Connect => false,
    };
    let mut text = String::new();
    let mut notes = Vec::new();
    let expr = if let (true, Some(function)) = (use_sys, &sys) {
        format!("{function}{}", arguments(method.params, texts, "(", ")"))
    } else {
        text.push_str(&format!("{};\n", connect_line(interface, options.service)));
        if has_sugar(method) && !method.oneway {
            format!(
                "svc.{}{}",
                rhai_lazy::msg::schema::snake_case(method.name),
                arguments(method.params, texts, "(", ")")
            )
        } else {
            let verb = if method.oneway {
                "invoke_oneway"
            } else {
                "invoke"
            };
            format!(
                "svc.{verb}({}, {})",
                rhai_str(method.name),
                arguments(method.params, texts, "[", "]")
            )
        }
    };
    let replies = !method.returns.is_empty();
    if replies {
        text.push_str(&format!("let {} = {expr};\n", options.bind));
        match options.output {
            Output::None => {}
            Output::Print => text.push_str(&format!("print({});\n", options.bind)),
            Output::Show => text.push_str(&format!("pg_show({});\n", options.bind)),
        }
    } else {
        text.push_str(&format!("{expr};\n"));
    }
    if method.oneway {
        notes.push("One-way: the call returns () once the message is queued.".to_string());
    }
    if method.params.iter().any(|p| matches!(p.ty, Ty::Option(_))) {
        notes.push("An Option parameter is () for none, otherwise the value.".to_string());
    }
    Ok(Snippet { text, notes })
}

/// `let svc = msg::connect(..)`, with the live name when it is not the
/// default one.
pub fn connect_line(interface: &Interface, service: Option<&str>) -> String {
    match service {
        Some(name) if name != interface.default_service() && name != interface.name => format!(
            "let svc = msg::connect({}, {})",
            rhai_str(interface.name),
            rhai_str(name)
        ),
        _ => format!("let svc = msg::connect({})", rhai_str(interface.name)),
    }
}

/// Connect to a live service by the name it is registered under.
pub fn connect(interface: &Interface, service: &str) -> Snippet {
    Snippet {
        text: format!(
            "{};\nprint(svc.methods);\n",
            connect_line(interface, Some(service))
        ),
        notes: Vec::new(),
    }
}

fn zero_reply(interface: &Interface, returns: &[Field]) -> String {
    match returns {
        [] => "()".to_string(),
        [only] => placeholder(interface, only.ty),
        many => {
            let fields = many
                .iter()
                .map(|f| format!("{}: {}", f.name, placeholder(interface, f.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("#{{ {fields} }}")
        }
    }
}

/// A `msg::serve` with one handler per method, each returning the zero value
/// of its reply, then `msg::run()`.
pub fn serve(interface: &Interface) -> Snippet {
    let service = format!("demo.{}", alias(interface.name));
    let mut text = format!(
        "// Serve {} under a name of your own. Names under os.lazy. are reserved.\n",
        interface.name
    );
    text.push_str(&format!(
        "msg::serve({}, {}, #{{\n",
        rhai_str(&service),
        rhai_str(interface.name)
    ));
    let mut notes = Vec::new();
    for method in interface.methods {
        if !method.objects.is_empty() {
            text.push_str(&format!(
                "    // {}: carries kernel objects ({}); a script cannot serve it\n",
                method.name,
                object_names(method).join(", ")
            ));
            notes.push(format!(
                "{} is left out: it carries kernel objects.",
                method.name
            ));
            continue;
        }
        let params = method
            .params
            .iter()
            .map(|p| param_name(p.name))
            .collect::<Vec<_>>()
            .join(", ");
        text.push_str(&format!("    {}: |{params}| {{\n", method.name));
        if !method.params.is_empty() {
            let list = method
                .params
                .iter()
                .map(arg_note)
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("        // arguments: {list}\n"));
        }
        if !method.returns.is_empty() {
            let list = method
                .returns
                .iter()
                .map(arg_note)
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("        // reply: {list}\n"));
        }
        text.push_str(&format!(
            "        {}\n    }},\n",
            zero_reply(interface, method.returns)
        ));
    }
    text.push_str("});\nprint(\"serving ");
    text.push_str(&service);
    text.push_str("\");\nmsg::run();\n");
    Snippet { text, notes }
}

/// What to do with a topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicAction {
    /// The generated `sys::<alias>::on_<helper>` (else `msg::on`), then `msg::run()`.
    On,
    /// `msg::on` and `msg::run()`.
    OnMsg,
    /// `msg::subscribe` and a pull with `next`.
    Subscribe,
    /// Subscribe, take the replayed retained event, close.
    Peek,
    /// `msg::publish` with a payload of the declared type.
    Publish,
}

const SHOW_EVENT: &str = "print(`${e.topic}: ${e.payload}`)";

/// Text for a declared topic.
pub fn topic(interface: &Interface, topic: &Topic, action: TopicAction) -> Snippet {
    let pattern = rhai_str(topic.pattern);
    let mut notes = Vec::new();
    let text = match action {
        TopicAction::On => match (
            crate::rhai_names::module(interface)
                .filter(|_| crate::rhai_names::module_usable(interface)),
            topic_helper(interface, topic),
        ) {
            (Some(module), Some(helper)) => format!(
                "sys::{}::on_{}(|e| {SHOW_EVENT});\nmsg::run();\n",
                module.alias, helper.helper
            ),
            _ => on_text(&pattern),
        },
        TopicAction::OnMsg => on_text(&pattern),
        TopicAction::Subscribe => subscribe_text(&pattern, 5000),
        TopicAction::Peek => {
            if topic.retained {
                notes.push("A retained topic replays its last value at once.".to_string());
            } else {
                notes
                    .push("This topic is not retained: Peek waits for the next event.".to_string());
            }
            subscribe_text(&pattern, 1000)
        }
        TopicAction::Publish => {
            let name = concrete_topic(topic.pattern);
            if name != topic.pattern {
                notes.push(format!(
                    "Replace the x in the topic name ({}).",
                    topic.pattern
                ));
            }
            let payload = payload_placeholder(interface, topic.payload);
            format!("msg::publish({}, {payload});\n", rhai_str(&name))
        }
    };
    Snippet { text, notes }
}

fn payload_placeholder(interface: &Interface, payload: &str) -> String {
    if interface.find_struct(payload).is_some() {
        placeholder(interface, Ty::Struct(static_name(interface, payload)))
    } else if interface.find_enum(payload).is_some() {
        placeholder(interface, Ty::Enum(static_name(interface, payload)))
    } else {
        "\"\"".to_string()
    }
}

fn static_name(interface: &Interface, name: &str) -> &'static str {
    interface
        .find_struct(name)
        .map(|s| s.name)
        .or_else(|| interface.find_enum(name).map(|e| e.name))
        .unwrap_or("")
}

fn on_text(pattern: &str) -> String {
    format!("msg::on({pattern}, |e| {SHOW_EVENT});\nmsg::run();\n")
}

fn subscribe_text(pattern: &str, wait_ms: u32) -> String {
    format!(
        "let sub = msg::subscribe({pattern});\nlet e = sub.next({wait_ms});   // () when nothing arrived in time\nprint(if e == () {{ \"no event\" }} else {{ e.payload }});\nsub.close();\n"
    )
}

/// Text for a topic nothing declares (a live topic): bytes in, bytes out.
pub fn raw_topic(topic_name: &str, action: TopicAction) -> Snippet {
    let name = rhai_str(topic_name);
    let text = match action {
        TopicAction::On | TopicAction::OnMsg => format!(
            "msg::on({name}, |e| print(`${{e.topic}}: ${{e.payload.as_string()}}`));\nmsg::run();\n"
        ),
        TopicAction::Subscribe | TopicAction::Peek => {
            let wait = if action == TopicAction::Peek {
                1000
            } else {
                5000
            };
            format!(
                "let sub = msg::subscribe({name});\nlet e = sub.next({wait});\nprint(if e == () {{ \"no event\" }} else {{ e.bytes }});\nsub.close();\n"
            )
        }
        TopicAction::Publish => format!("msg::publish({name}, \"hello\");\n"),
    };
    Snippet {
        text,
        notes: vec!["No interface declares this topic: its payload is bytes.".to_string()],
    }
}
