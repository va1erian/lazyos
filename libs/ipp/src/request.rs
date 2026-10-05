//! The requests a print client sends, with the operation attributes RFC 8011
//! requires first and in its order: `attributes-charset`,
//! `attributes-natural-language`, the target, then the rest.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{op, tag, Attribute, Group, Message, Value};

/// What the client says about itself on every request.
#[derive(Clone, Copy, Debug)]
pub struct Client<'a> {
    /// The printer's URI, `ipp://host:631/ipp/print`.
    pub printer_uri: &'a str,
    /// `requesting-user-name`.
    pub user: &'a str,
}

/// The job's description and the choices of the print dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ticket {
    /// `job-name`: the document's title.
    pub name: String,
    /// `document-format`, e.g. `image/pwg-raster`.
    pub format: String,
    /// `copies` (left out when `None`).
    pub copies: Option<i32>,
    /// `media`, a PWG self-describing name such as `iso_a4_210x297mm`.
    pub media: Option<String>,
    /// `print-color-mode`: `color` or `monochrome`.
    pub color_mode: Option<String>,
    /// `print-quality`: 3 draft, 4 normal, 5 high.
    pub quality: Option<i32>,
}

/// `print-quality` values (RFC 8011 5.2.13).
pub mod quality {
    pub const DRAFT: i32 = 3;
    pub const NORMAL: i32 = 4;
    pub const HIGH: i32 = 5;
}

/// The printer attributes a client asks for to fill its dialog and status
/// line.
pub const STATUS_ATTRIBUTES: &[&str] = &[
    "printer-make-and-model",
    "printer-state",
    "printer-state-message",
    "printer-state-reasons",
    "document-format-supported",
    "media-supported",
    "media-default",
    "marker-names",
    "marker-levels",
    "marker-colors",
];

/// The job attributes a client polls for.
pub const JOB_ATTRIBUTES: &[&str] = &[
    "job-state",
    "job-state-reasons",
    "job-state-message",
    "job-impressions-completed",
];

fn operation(client: &Client, code: u16, request_id: u32) -> (Message, Group) {
    let mut group = Group::new(tag::OPERATION);
    group.attributes.push(Attribute::new(
        "attributes-charset",
        Value::string(tag::CHARSET, "utf-8"),
    ));
    group.attributes.push(Attribute::new(
        "attributes-natural-language",
        Value::string(tag::NATURAL_LANGUAGE, "en"),
    ));
    group.attributes.push(Attribute::new(
        "printer-uri",
        Value::uri(client.printer_uri),
    ));
    (Message::new(code, request_id), group)
}

fn user(group: &mut Group, client: &Client) {
    group.attributes.push(Attribute::new(
        "requesting-user-name",
        Value::name(client.user),
    ));
}

fn keywords(names: &[&str]) -> Vec<Value> {
    names.iter().map(|n| Value::keyword(n)).collect()
}

/// Get-Printer-Attributes for `requested` (every attribute when empty).
pub fn get_printer_attributes(client: &Client, request_id: u32, requested: &[&str]) -> Message {
    let (mut message, mut group) = operation(client, op::GET_PRINTER_ATTRIBUTES, request_id);
    user(&mut group, client);
    if !requested.is_empty() {
        group
            .attributes
            .push(Attribute::set("requested-attributes", keywords(requested)));
    }
    message.groups.push(group);
    message
}

/// Validate-Job: whether the printer would accept `ticket`, without a
/// document.
pub fn validate_job(client: &Client, request_id: u32, ticket: &Ticket) -> Message {
    job_request(client, op::VALIDATE_JOB, request_id, ticket)
}

/// Print-Job: the document follows the encoded message on the wire.
pub fn print_job(client: &Client, request_id: u32, ticket: &Ticket) -> Message {
    job_request(client, op::PRINT_JOB, request_id, ticket)
}

/// Create-Job: the job without its document, so the printer gives the job
/// its id before any page is sent and every later abort can name it in
/// Cancel-Job. The document follows in [`send_document`]. As RFC 8011 4.2.4
/// says, it carries no document attributes: `document-format` travels with
/// the document.
pub fn create_job(client: &Client, request_id: u32, ticket: &Ticket) -> Message {
    let (mut message, group) = job_operation_group(client, op::CREATE_JOB, request_id, ticket);
    message.groups.push(group);
    push_template(&mut message, ticket);
    message
}

/// Send-Document for job `job_id` (from [`create_job`]): the document
/// follows the encoded message on the wire. `last` is `last-document`; a
/// printer that takes one document per job is always sent `true`.
pub fn send_document(
    client: &Client,
    request_id: u32,
    job_id: i32,
    format: &str,
    last: bool,
) -> Message {
    let (mut message, mut group) = operation(client, op::SEND_DOCUMENT, request_id);
    group
        .attributes
        .push(Attribute::new("job-id", Value::Integer(job_id)));
    user(&mut group, client);
    group
        .attributes
        .push(Attribute::new("document-format", Value::mime(format)));
    group
        .attributes
        .push(Attribute::new("last-document", Value::Boolean(last)));
    message.groups.push(group);
    message
}

/// The operation group of a job-creating request, up to `job-name`.
fn job_operation_group(
    client: &Client,
    code: u16,
    request_id: u32,
    ticket: &Ticket,
) -> (Message, Group) {
    let (message, mut group) = operation(client, code, request_id);
    user(&mut group, client);
    group
        .attributes
        .push(Attribute::new("job-name", Value::name(&ticket.name)));
    (message, group)
}

fn job_request(client: &Client, code: u16, request_id: u32, ticket: &Ticket) -> Message {
    let (mut message, mut group) = job_operation_group(client, code, request_id, ticket);
    group.attributes.push(Attribute::new(
        "document-format",
        Value::mime(&ticket.format),
    ));
    message.groups.push(group);
    push_template(&mut message, ticket);
    message
}

/// The ticket's job template attributes, as a job group when it has any.
fn push_template(message: &mut Message, ticket: &Ticket) {
    let mut job = Group::new(tag::JOB);
    if let Some(copies) = ticket.copies {
        job.attributes
            .push(Attribute::new("copies", Value::Integer(copies)));
    }
    if let Some(media) = &ticket.media {
        job.attributes
            .push(Attribute::new("media", Value::keyword(media)));
    }
    if let Some(mode) = &ticket.color_mode {
        job.attributes
            .push(Attribute::new("print-color-mode", Value::keyword(mode)));
    }
    if let Some(quality) = ticket.quality {
        job.attributes
            .push(Attribute::new("print-quality", Value::Enum(quality)));
    }
    if !job.attributes.is_empty() {
        message.groups.push(job);
    }
}

/// Get-Job-Attributes for job `job_id`.
pub fn get_job_attributes(client: &Client, request_id: u32, job_id: i32) -> Message {
    job_operation(
        client,
        op::GET_JOB_ATTRIBUTES,
        request_id,
        job_id,
        JOB_ATTRIBUTES,
    )
}

/// Cancel-Job for job `job_id`.
pub fn cancel_job(client: &Client, request_id: u32, job_id: i32) -> Message {
    job_operation(client, op::CANCEL_JOB, request_id, job_id, &[])
}

fn job_operation(
    client: &Client,
    code: u16,
    request_id: u32,
    job_id: i32,
    requested: &[&str],
) -> Message {
    let (mut message, mut group) = operation(client, code, request_id);
    group
        .attributes
        .push(Attribute::new("job-id", Value::Integer(job_id)));
    user(&mut group, client);
    if !requested.is_empty() {
        group
            .attributes
            .push(Attribute::set("requested-attributes", keywords(requested)));
    }
    message.groups.push(group);
    message
}

/// What a job reply says: its id, state and the reasons and message the
/// printer gave, as far as the reply has them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobStatus {
    pub id: Option<i32>,
    pub state: Option<i32>,
    pub reasons: Vec<String>,
    pub message: Option<String>,
}

impl JobStatus {
    /// Reads the job group of a Print-Job or Get-Job-Attributes reply.
    pub fn of(reply: &Message) -> JobStatus {
        let reasons = reply
            .get(tag::JOB, "job-state-reasons")
            .map(|a| {
                a.values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|r| *r != "none")
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        JobStatus {
            id: reply.int(tag::JOB, "job-id"),
            state: reply.int(tag::JOB, "job-state"),
            reasons,
            message: reply
                .text(tag::JOB, "job-state-message")
                .filter(|m| !m.is_empty())
                .map(String::from),
        }
    }
}

/// One ink or toner cartridge from `marker-names` and `marker-levels`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub name: String,
    /// Percent full, or `None` when the printer does not know (`-1`, `-2`,
    /// `-3` on the wire).
    pub level: Option<u8>,
}

/// The cartridges a Get-Printer-Attributes reply lists.
pub fn markers(reply: &Message) -> Vec<Marker> {
    let (Some(names), levels) = (
        reply.get(tag::PRINTER, "marker-names"),
        reply.get(tag::PRINTER, "marker-levels"),
    ) else {
        return Vec::new();
    };
    names
        .values
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let level = levels
                .and_then(|l| l.values.get(i))
                .and_then(Value::as_int)
                .and_then(|n| u8::try_from(n).ok())
                .filter(|n| *n <= 100);
            Some(Marker {
                name: String::from(name.as_str()?),
                level,
            })
        })
        .collect()
}
