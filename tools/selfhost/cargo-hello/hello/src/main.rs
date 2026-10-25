//! The program of the cargo sample: uses the `greet` crate of the same
//! workspace and the standard library's collections.

use std::collections::HashMap;

const TEXT: &str = "the quick brown fox jumps over the lazy dog the end";

fn main() {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for word in TEXT.split_whitespace() {
        *counts.entry(word).or_default() += 1;
    }
    let sum = greet::parallel_sum(100, 4);
    println!(
        "{} from cargo: sum {sum} on 4 threads, {} distinct words",
        greet::greeting("LazyOS"),
        counts.len()
    );
}
