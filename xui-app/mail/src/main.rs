//! `xui-mail`: esMail (va1erian/esmail) on LazyOS.
//!
//! The mail core is esMail's own (IMAP and SMTP over TLS, the SQLite cache,
//! HTML sanitising), built with its `rustls` backend. This file supplies the
//! LazyOS side before the window opens: the GPLv2-compatible crypto provider
//! the TLS connections use (`nettls-crypto`, docs/tls-plan.md §3.2), the
//! session-only password store (`secrets.rs`), where esMail keeps its files,
//! and the fonts. `app/` is the window.
//!
//! `xui-mail mailto:...` opens a new message to the link's address (`mimed`
//! starts Mail that way for `x-scheme-handler/mailto`).
//!
//! Serial evidence: `MAIL:UP:PASS` after the first frame, `MAIL:BIND:FAIL:<code>`
//! when the display cannot be bound and `MAIL:RUN:FAIL:<err>` when the loop
//! fails; `app/mod.rs` lists the rest.

mod app;
mod secrets;

use std::path::Path;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::themed::run_themed;
use xui_core::backend::PlatformSpec;
use xui_core::units::Dip;

/// The window's size: three panes need the width.
const WINDOW: (i32, i32) = (1000, 620);

fn main() -> std::process::ExitCode {
    // Before anything else starts a thread: `set_var` is only sound while the
    // process has one.
    place_files(&xui_app::platform::dirs::default_dir());
    if rustls::crypto::CryptoProvider::install_default(nettls_crypto::provider()).is_err() {
        println!("MAIL:RUN:FAIL:a TLS crypto provider was already installed");
        return std::process::ExitCode::FAILURE;
    }
    secrets::install();
    xui_app::font::register_docs();
    webfonts::register();

    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("MAIL:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("MAIL:UP:PASS"));
    let config = esmail::config::Config::load();
    let spec = PlatformSpec::new("Mail").size(Dip(width as f32), Dip(height as f32));
    // `init` starts Mail with a `mailto:` link when another app opens one.
    let link = app::mailto::from_args(std::env::args());
    let outcome = run_themed(&backend, spec, move |ui| {
        let mut mail = app::Mail::build(ui, config).expect("the Mail window was created");
        if let Some(link) = &link {
            mail.compose_mailto(ui, link);
        }
        mail
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("MAIL:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Points esMail's configuration and cache at `home` (the user's home, or
/// `/transient` without one), unless the environment already chose: desktop
/// apps may start without `$HOME`, where esMail would find no directory.
fn place_files(home: &Path) {
    for (var, dir) in [
        ("ESMAIL_CONFIG_DIR", home.join(".config/esmail")),
        ("ESMAIL_DATA_DIR", home.join(".local/share/esmail")),
    ] {
        if std::env::var_os(var).is_none() {
            // SAFETY: called first thing in `main`, before any thread exists,
            // so nothing can read the environment concurrently.
            unsafe { std::env::set_var(var, dir) };
        }
    }
}
