//! `fetch`, `curl`, `wget`: one binary, the personality chosen by the name
//! it was run as (docs/tls-plan.md §7). See `lib.rs` for the layout.

fn main() {
    let mut argv = Vec::new();
    for arg in std::env::args_os() {
        match arg.into_string() {
            Ok(text) => argv.push(text),
            Err(raw) => {
                // A URL, header or path that is not UTF-8 cannot be sent or
                // shown faithfully; refuse it instead of guessing.
                eprintln!("fetch: argument {raw:?} is not valid UTF-8");
                std::process::exit(2);
            }
        }
    }
    let argv0 = argv.first().cloned().unwrap_or_default();
    let rest = argv.get(1..).unwrap_or(&[]);
    std::process::exit(nettls::app::main(&argv0, rest));
}
