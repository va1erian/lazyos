//! The library crate of the cargo sample: a greeting and a sum computed on
//! several threads, so a build on LazyOS exercises `std::thread` too.

/// The greeting for `name`. (This example is a doctest: `cargo test` builds
/// and runs it with rustdoc.)
///
/// ```
/// assert_eq!(greet::greeting("doctest"), "Hello, doctest");
/// ```
pub fn greeting(name: &str) -> String {
    format!("Hello, {name}")
}

/// The sum of `1..=n`, split across `threads` scoped threads.
pub fn parallel_sum(n: u64, threads: u64) -> u64 {
    let threads = threads.max(1);
    let chunk = n.div_ceil(threads);
    std::thread::scope(|scope| {
        let parts: Vec<_> = (0..threads)
            .map(|index| {
                let start = index * chunk + 1;
                let end = ((index + 1) * chunk).min(n);
                scope.spawn(move || (start..=end).sum::<u64>())
            })
            .collect();
        parts.into_iter().map(|part| part.join().unwrap()).sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greets_by_name() {
        assert_eq!(greeting("LazyOS"), "Hello, LazyOS");
    }

    #[test]
    fn parallel_sum_matches_the_formula() {
        for (n, threads) in [(100, 4), (7, 3), (1, 8), (0, 2)] {
            assert_eq!(
                parallel_sum(n, threads),
                n * (n + 1) / 2,
                "n={n} threads={threads}"
            );
        }
    }
}
