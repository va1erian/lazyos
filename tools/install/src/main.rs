//! `lazyos-install`: see `lazyos_install` (src/lib.rs) and
//! docs/grub-install-plan.md.

fn main() {
    let options = match lazyos_install::args::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("lazyos-install: {error}\n\n{}", lazyos_install::args::USAGE);
            std::process::exit(2);
        }
    };
    if let Err(error) = lazyos_install::run::run(&options) {
        eprintln!("lazyos-install: {error}");
        std::process::exit(1);
    }
}
