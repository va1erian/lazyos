//! Writes every package's `icons/app-{16,32,128}.png` (see the library), or
//! only the packages named on the command line (their directories, as in
//! `PACKAGES`), as `tools/xui/new_app.py` does for the app it adds.
//!
//! ```text
//! cargo run --manifest-path xui-app/Cargo.toml -p app-icons [-- xui-app/packages/<short>]
//! ```

use std::path::Path;

use app_icons::{render, PACKAGES, SIZES};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .ok_or("the crate sits three levels below the repository root")?;
    let only: Vec<String> = std::env::args().skip(1).collect();
    for &(dir, art) in PACKAGES {
        if !only.is_empty() && !only.iter().any(|name| name == dir) {
            continue;
        }
        let icons = root.join(dir).join("icons");
        std::fs::create_dir_all(&icons)?;
        for size in SIZES {
            let path = icons.join(format!("app-{size}.png"));
            render(art, size).save_png(&path)?;
            println!("{dir}/icons/app-{size}.png");
        }
    }
    Ok(())
}
