//! `t_alloc` — allocator growth (`mmap`/`brk`) behind `Vec`/`String`/`Box`.

mod common;

fn main() {
    let values: Vec<u32> = (0..100_000).collect();
    let sum: u64 = values.iter().map(|v| *v as u64).sum();
    let text: String = format!("sum={sum}");
    let boxed = Box::new(sum);

    let ok = values.len() == 100_000 && sum == 4_999_950_000 && text.contains("4999950000") && *boxed == sum;
    common::report("alloc", ok, "allocation or contents wrong");
}
