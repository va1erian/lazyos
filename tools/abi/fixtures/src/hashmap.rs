//! `t_hashmap` — `HashMap` needs a random seed (`getrandom`).

mod common;

use std::collections::HashMap;

fn main() {
    let mut map = HashMap::new();
    for i in 0..1000u32 {
        map.insert(format!("k{i}"), i);
    }
    let ok = map.len() == 1000 && map.get("k500") == Some(&500);
    common::report("hashmap", ok, "hashmap insert/lookup wrong");
}
