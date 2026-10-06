//! Writes `docs/icons.md` from the name table:
//! `cargo run -p lazyicons --example catalog` (in `xui-app/`).
//! `--stdout` prints it instead, `--check` only reports whether the file is
//! current (exit status 1 when it is not).

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let text = lazyicons::catalog_markdown();
    let path = doc_path();
    match std::env::args().nth(1).as_deref() {
        Some("--stdout") => print!("{text}"),
        Some("--check") => {
            let current = std::fs::read_to_string(&path).unwrap_or_default();
            // A Windows checkout may hold CRLF; the content is what counts.
            if current.replace("\r\n", "\n") != text {
                eprintln!("{} is stale: run the catalog example", path.display());
                return ExitCode::FAILURE;
            }
        }
        None => {
            if let Err(err) = std::fs::write(&path, &text) {
                eprintln!("cannot write {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
            println!(
                "wrote {} ({} names)",
                path.display(),
                lazyicons::all().count()
            );
        }
        Some(other) => {
            eprintln!("unknown argument {other:?} (expected --stdout or --check)");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// `docs/icons.md` at the repository root, three levels above this crate.
fn doc_path() -> PathBuf {
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = crate_dir
        .ancestors()
        .nth(3)
        .expect("the crate sits in xui-app/crates/");
    repo.join("docs").join("icons.md")
}
