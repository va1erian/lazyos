//! `t_file` — `std::fs` reading a file from the FAT volume.

mod common;

fn main() {
    match std::fs::read_to_string("HELLO.TXT") {
        Ok(text) => common::report("file", text.contains("Hello from LazyOS"), "unexpected contents"),
        Err(err) => common::fail("file", &format!("read error: {err}")),
    }
}
