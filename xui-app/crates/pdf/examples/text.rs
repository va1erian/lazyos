//! Prints the text layer of a page: `cargo run -p lazypdf --example text -- <file.pdf> <page>`.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let doc = lazypdf::Document::open(std::fs::read(&a[0]).unwrap(), "").unwrap();
    let r = lazypdf::Renderer::new(&doc);
    let page: usize = a.get(1).map_or(1, |p| p.parse().unwrap());
    println!("{}", r.page_text(page - 1).unwrap().plain());
}
