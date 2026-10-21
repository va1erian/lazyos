//! The Gallery: the examples of `docs/messenger-playground-plan.md` section 6
//! as files with a header, embedded in the app.
//!
//! Each file starts with comment lines the manifest reads:
//!
//! ```text
//! // title: Who is on the fabric?
//! // tags: explore, registry        (the first tag is the section)
//! // needs: none                    (or: sound card, second run, admin login)
//! // after: 11_service_watchdog     (optional: the run this one follows)
//! // expect: lines of output, `*` matches any text, in order
//! ```
//!
//! The whole file is the script a Gallery entry loads (the header is comment
//! text), so line numbers in an error are the file's. Loading it never runs
//! it. Example 14 of the plan (a toy interface) is not shipped: it needs a new
//! `.midl`.

/// What an example needs beyond a plain desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    SoundCard,
    /// A second run (or window) to pair with.
    SecondRun,
    AdminLogin,
}

impl Need {
    fn parse(text: &str) -> Option<Need> {
        match text.trim() {
            "sound card" => Some(Need::SoundCard),
            "second run" => Some(Need::SecondRun),
            "admin login" => Some(Need::AdminLogin),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Need::SoundCard => "sound card",
            Need::SecondRun => "second run",
            Need::AdminLogin => "admin login",
        }
    }
}

/// The Gallery's groups, in the plan's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Explore,
    Listen,
    Do,
    Build,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Explore => "Explore",
            Section::Listen => "Listen",
            Section::Do => "Do things",
            Section::Build => "Build things",
        }
    }

    fn parse(tag: &str) -> Option<Section> {
        match tag {
            "explore" => Some(Section::Explore),
            "listen" => Some(Section::Listen),
            "do" => Some(Section::Do),
            "build" => Some(Section::Build),
            _ => None,
        }
    }
}

/// One example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Example {
    /// The file name without `.rhai`: `01_whos_on_the_fabric`.
    pub id: &'static str,
    pub title: String,
    pub section: Section,
    pub tags: Vec<String>,
    pub needs: Vec<Need>,
    /// The example it follows (a second run), if any.
    pub after: Option<String>,
    /// Output lines to expect, in order; `*` matches any run of text.
    pub expect: Vec<String>,
    /// The whole file, header included.
    pub source: &'static str,
}

impl Example {
    /// A script that keeps running until Stop (it ends in `msg::run();`).
    pub fn runs_until_stopped(&self) -> bool {
        self.source.contains("msg::run();")
    }

    /// The number the Gallery lists it under: `01`, `11b`.
    pub fn number(&self) -> &str {
        self.id.split('_').next().unwrap_or(self.id)
    }
}

/// Why a file's header could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderError {
    pub id: String,
    pub message: String,
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.id, self.message)
    }
}

/// Read the header of `source` into an example.
pub fn parse(id: &'static str, source: &'static str) -> Result<Example, HeaderError> {
    let fail = |message: &str| HeaderError {
        id: id.to_string(),
        message: message.to_string(),
    };
    let mut title = None;
    let mut tags = Vec::new();
    let mut needs = Vec::new();
    let mut after = None;
    let mut expect = Vec::new();
    let mut saw_needs = false;
    for line in source.lines() {
        let Some(comment) = line.strip_prefix("//") else {
            break;
        };
        let Some((key, value)) = comment.trim_start().split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "title" => title = Some(value.to_string()),
            "tags" => {
                tags = value
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            }
            "needs" => {
                saw_needs = true;
                if value != "none" {
                    for part in value.split(',') {
                        needs.push(
                            Need::parse(part)
                                .ok_or_else(|| fail(&format!("unknown need `{}`", part.trim())))?,
                        );
                    }
                }
            }
            "after" => after = Some(value.to_string()),
            "expect" => expect.push(value.to_string()),
            _ => {}
        }
    }
    let title = title
        .filter(|t| !t.is_empty())
        .ok_or_else(|| fail("no title"))?;
    let section = tags
        .first()
        .and_then(|t| Section::parse(t))
        .ok_or_else(|| fail("the first tag must be a section: explore, listen, do or build"))?;
    if !saw_needs {
        return Err(fail("no needs line (write `needs: none`)"));
    }
    if expect.is_empty() {
        return Err(fail("no expect line"));
    }
    Ok(Example {
        id,
        title,
        section,
        tags,
        needs,
        after,
        expect,
        source,
    })
}

/// The embedded files, in Gallery order.
const FILES: &[(&str, &str)] = &[
    (
        "01_whos_on_the_fabric",
        include_str!("../../../packages/playground/examples/01_whos_on_the_fabric.rhai"),
    ),
    (
        "02_what_can_it_do",
        include_str!("../../../packages/playground/examples/02_what_can_it_do.rhai"),
    ),
    (
        "03_read_a_retained_value",
        include_str!("../../../packages/playground/examples/03_read_a_retained_value.rhai"),
    ),
    (
        "04_heartbeat",
        include_str!("../../../packages/playground/examples/04_heartbeat.rhai"),
    ),
    (
        "05_memory_bar_chart",
        include_str!("../../../packages/playground/examples/05_memory_bar_chart.rhai"),
    ),
    (
        "06_config_spy",
        include_str!("../../../packages/playground/examples/06_config_spy.rhai"),
    ),
    (
        "07_qos_laboratory",
        include_str!("../../../packages/playground/examples/07_qos_laboratory.rhai"),
    ),
    (
        "08_hello_toast",
        include_str!("../../../packages/playground/examples/08_hello_toast.rhai"),
    ),
    (
        "09_open_with_oracle",
        include_str!("../../../packages/playground/examples/09_open_with_oracle.rhai"),
    ),
    (
        "10_volume_wave",
        include_str!("../../../packages/playground/examples/10_volume_wave.rhai"),
    ),
    (
        "11_service_watchdog",
        include_str!("../../../packages/playground/examples/11_service_watchdog.rhai"),
    ),
    (
        "11b_service_watchdog_trigger",
        include_str!("../../../packages/playground/examples/11b_service_watchdog_trigger.rhai"),
    ),
    (
        "12_pirate_echo_server",
        include_str!("../../../packages/playground/examples/12_pirate_echo_server.rhai"),
    ),
    (
        "12b_pirate_echo_client",
        include_str!("../../../packages/playground/examples/12b_pirate_echo_client.rhai"),
    ),
    (
        "13_chat_room",
        include_str!("../../../packages/playground/examples/13_chat_room.rhai"),
    ),
];

/// The ids of the embedded files, in order (for checking the directory).
pub fn ids() -> Vec<&'static str> {
    FILES.iter().map(|(id, _)| *id).collect()
}

/// Every example. A file whose header cannot be read is a build-time bug a
/// test catches, so this returns only the ones that parse.
pub fn examples() -> Vec<Example> {
    FILES
        .iter()
        .filter_map(|(id, source)| parse(id, source).ok())
        .collect()
}

/// Every header problem (empty when the Gallery is sound).
pub fn problems() -> Vec<HeaderError> {
    FILES
        .iter()
        .filter_map(|(id, source)| parse(id, source).err())
        .collect()
}

/// The example called `id`.
pub fn find(id: &str) -> Option<Example> {
    examples().into_iter().find(|e| e.id == id)
}

/// Whether `text` matches `pattern`, where `*` matches any run of characters.
/// Ends are trimmed: an expected line is judged by its words.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.trim();
    let text = text.trim();
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let first = parts[0];
    let last = parts[parts.len() - 1];
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) {
        return false;
    }
    if text.len() < first.len() + last.len() {
        return false;
    }
    let mut at = first.len();
    let end = text.len() - last.len();
    for part in &parts[1..parts.len() - 1] {
        match text[at..end].find(part) {
            Some(found) => at += found + part.len(),
            None => return false,
        }
    }
    true
}

/// Check `output` against the expected lines: each expectation matches a
/// later line than the one before it. Returns the first that did not.
pub fn check_expectations(expect: &[String], output: &[String]) -> Result<(), String> {
    let mut next = 0;
    for want in expect {
        match output[next..].iter().position(|l| glob_match(want, l)) {
            Some(found) => next += found + 1,
            None => return Err(want.clone()),
        }
    }
    Ok(())
}
