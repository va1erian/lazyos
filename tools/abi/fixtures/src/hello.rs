//! `t_hello` — the minimal contract: `write(1)` + `exit`.

mod common;

fn main() {
    println!("ABI fixture: hello");
    common::pass("hello");
}
