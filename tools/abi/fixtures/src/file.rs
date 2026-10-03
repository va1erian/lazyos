//! `t_file` — `std::fs` reading a sample file the OS volume ships.

mod common;

fn main() {
    match std::fs::read_to_string("/system/share/samples/hello.txt") {
        Ok(text) => common::report("file", text.contains("Hello from LazyOS"), "unexpected contents"),
        Err(err) => common::fail("file", &format!("read error: {err}")),
    }
}
