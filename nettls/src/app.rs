//! One run, start to exit code: parse, load roots, fetch, write.

use std::io::Write;

use ureq::http::Response;
use ureq::Body;
use url::Url;

use crate::body::{self, Encoding, BODY_CAP};
use crate::cli;
use crate::client::{self, Client, Observer};
use crate::opts::{Command, Options, Personality};
use crate::output;
use crate::redirect;
use crate::report::Failure;
use crate::roots;
use crate::sanitize::printable_str;
use crate::timefmt;
use crate::tls;
use crate::writeout::{self, Facts};

/// Run the program as `argv0` with `args`; returns the exit code.
pub fn main(argv0: &str, args: &[String]) -> i32 {
    let personality = Personality::from_argv0(argv0);
    let command = match cli::parse(personality, args) {
        Ok(command) => command,
        Err(message) => {
            let failure = Failure::Usage(message);
            eprintln!("{}", failure.render(personality));
            eprintln!("Try '{} --help' for more information.", personality.name());
            return failure.code(personality);
        }
    };
    match command {
        Command::Print(text) => {
            print!("{text}");
            0
        }
        Command::Run(opts) => match run(&opts) {
            Ok(()) => 0,
            Err(failure) => {
                if !opts.quiet || opts.show_error {
                    eprintln!("{}", failure.render(personality));
                }
                failure.code(personality)
            }
        },
    }
}

/// Fetch `opts.url` and write what was asked for.
pub fn run(opts: &Options) -> Result<(), Failure> {
    let url = redirect::parse_user_url(&opts.url)?;
    let choice = roots::choose(
        opts.cacert.as_deref(),
        std::env::var(roots::ENV_CERT_FILE).ok(),
    );
    let loaded = roots::load(&choice).map_err(Failure::Roots)?;
    if opts.verbose {
        eprintln!(
            "* CA bundle {} ({}): {} trust anchors, {} ignored",
            choice.path.display(),
            choice.source,
            loaded.store.len(),
            loaded.ignored
        );
    }
    let config = tls::client_config(loaded.store).map_err(Failure::Roots)?;
    let client = Client::new(opts, config);
    let mut report = Report {
        opts,
        header_blocks: Vec::new(),
    };
    let outcome = client.fetch(opts, url.clone(), &mut report)?;

    let status = outcome.response.status();
    let fails = opts.fail || opts.personality != Personality::Curl;
    if fails && status.as_u16() >= 400 {
        let reason = status.canonical_reason().unwrap_or("").to_string();
        return Err(Failure::HttpStatus(status.as_u16(), reason));
    }

    let mut sink = output::open(&opts.output, &url)?;
    let write_err = |e: std::io::Error| Failure::Write(e.to_string());
    if opts.head || opts.include {
        for block in &report.header_blocks {
            sink.writer()
                .write_all(block.as_bytes())
                .map_err(write_err)?;
        }
    }
    let content_type = header_text(&outcome.response, "content-type");
    let length = header_text(&outcome.response, "content-length");
    if opts.personality == Personality::Wget && !opts.quiet {
        let shown = sink
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "STDOUT".into());
        eprintln!(
            "Length: {} [{}]",
            or(&length, "unspecified"),
            or(&content_type, "unknown")
        );
        eprintln!("Saving to: '{shown}'\n");
    }
    let size = if opts.head {
        0
    } else {
        write_body(outcome.response, sink.writer())?
    };
    if let Some(template) = &opts.write_out {
        let facts = Facts {
            http_code: status.as_u16(),
            url_effective: outcome.url.as_str(),
            num_redirects: outcome.redirects,
            size_download: size,
            content_type: &content_type,
        };
        print!("{}", writeout::expand(template, &facts));
        let _ = std::io::stdout().flush();
    }
    if opts.personality == Personality::Wget && !opts.quiet {
        let shown = sink
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".into());
        eprintln!("{} - '{shown}' saved [{size}]", timefmt::now_utc());
    }
    Ok(())
}

/// Decode and write the body under the cap.
fn write_body(response: Response<Body>, out: &mut dyn Write) -> Result<u64, Failure> {
    let encoding = Encoding::from_header(
        response
            .headers()
            .get("content-encoding")
            .map(|v| v.as_bytes()),
    );
    let (_, body) = response.into_parts();
    body::copy_body(body.into_reader(), &encoding, out, BODY_CAP)
}

/// A response header as printable text, or empty.
fn header_text(response: &Response<Body>, name: &str) -> String {
    response
        .headers()
        .get(name)
        .map(|v| crate::sanitize::printable(v.as_bytes()))
        .unwrap_or_default()
}

fn or<'a>(text: &'a str, default: &'a str) -> &'a str {
    if text.is_empty() {
        default
    } else {
        text
    }
}

/// Per-hop reporting: wget's progress, `-S`, `-v`, and the header blocks
/// `-i`/`-I` write before the body.
struct Report<'a> {
    opts: &'a Options,
    header_blocks: Vec<String>,
}

impl Observer for Report<'_> {
    fn before(&mut self, url: &Url) {
        let shown = printable_str(url.as_str());
        if self.opts.personality == Personality::Wget && !self.opts.quiet {
            eprintln!("--{}--  {shown}", timefmt::now_utc());
        }
        if self.opts.verbose {
            let host = url.host_str().unwrap_or("");
            let port = url.port_or_known_default().unwrap_or(0);
            eprintln!("* Connecting to {} port {port}", printable_str(host));
            let method = if self.opts.head { "HEAD" } else { "GET" };
            let path = &url[url::Position::BeforePath..url::Position::AfterQuery];
            eprintln!("> {method} {} HTTP/1.1", printable_str(path));
            eprintln!("> Host: {}", printable_str(host));
            for (name, value) in &self.opts.headers {
                eprintln!("> {name}: {}", printable_str(value));
            }
        }
    }

    fn after(&mut self, _url: &Url, response: &Response<Body>) {
        let status = client::status_line(response);
        let headers = client::header_lines(response);
        if self.opts.personality == Personality::Wget && !self.opts.quiet {
            let code = response.status();
            eprintln!(
                "HTTP request sent, awaiting response... {} {}",
                code.as_u16(),
                code.canonical_reason().unwrap_or("")
            );
            if self.opts.server_response {
                eprintln!("  {status}");
                for line in &headers {
                    eprintln!("  {line}");
                }
            }
            if self.opts.follow && redirect::is_redirect(code.as_u16()) {
                let location = header_text(response, "location");
                eprintln!("Location: {location} [following]");
            }
        }
        if self.opts.verbose {
            eprintln!("< {status}");
            for line in &headers {
                eprintln!("< {line}");
            }
        }
        let mut block = format!("{status}\n");
        for line in &headers {
            block.push_str(line);
            block.push('\n');
        }
        block.push('\n');
        self.header_blocks.push(block);
    }
}
