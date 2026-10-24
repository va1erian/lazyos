//! Prints the built-in shapes as a `.lcur` theme: `cargo run -p cursorgfx --example themetext`.
fn main() {
    print!(
        "{}",
        cursorgfx::format::serialize(&cursorgfx::Theme::builtin("builtin"))
    );
}
