//! `cargo run -p terminfo --example compile -- SRC OUT`: write the compiled
//! entry the image build would install (for inspecting it with `infocmp`).

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(src), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: compile SRC OUT");
        std::process::exit(2);
    };
    let source = std::fs::read_to_string(&src).expect("read the source");
    match terminfo::compile_source(&source) {
        Ok(bytes) => std::fs::write(&out, bytes).expect("write the entry"),
        Err(error) => {
            eprintln!("{src}: {error}");
            std::process::exit(1);
        }
    }
}
