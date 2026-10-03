//! `procstress` — `std::process`: spawn with pipes/exit status, `process::id`,
//! `current_exe` and environment passing.

mod common;

use std::io::Read;
use std::process::{Command, Stdio};

/// The fixture's own name (`/system/bin/abi-init`), so a child `execve`
/// finds it again through the `$PATH` search the kernel maps onto `/system/bin`.
const PROGRAM: &str = "abi-init";
const ENV_KEY: &str = "LAZYOS_ABI_CHILD";

fn note(first: &mut Option<String>, reason: String) {
    if first.is_none() {
        *first = Some(reason);
    }
}

fn main() {
    // Child mode: report the environment check through the exit code so the
    // parent can verify it across `fork`/`execve`.
    if std::env::args().nth(1).as_deref() == Some("child") {
        let code = if std::env::var(ENV_KEY).as_deref() == Ok("42") {
            42
        } else {
            7
        };
        std::process::exit(code);
    }

    let mut first: Option<String> = None;

    if std::process::id() == 0 {
        note(&mut first, "process::id() is zero".to_string());
    }
    match std::env::current_exe() {
        Ok(path) => {
            if path.as_os_str().is_empty() {
                note(&mut first, "current_exe() is empty".to_string());
            }
        }
        Err(err) => note(&mut first, format!("current_exe: {err}")),
    }

    // Spawn with piped stdio, inherited environment plus an override, and
    // verify the exit status the child chose.
    match Command::new(PROGRAM)
        .arg("child")
        .env(ENV_KEY, "42")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let status = child.wait();
            match status {
                Ok(status) => {
                    if status.code() != Some(42) {
                        note(&mut first, format!("piped child exit {status:?}"));
                    }
                }
                Err(err) => note(&mut first, format!("wait: {err}")),
            }
            let mut out = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_string(&mut out);
            }
        }
        Err(err) => note(&mut first, format!("spawn with pipes: {err}")),
    }

    // The same spawn without pipes: a separate code path (no pipe setup).
    match Command::new(PROGRAM)
        .arg("child")
        .env(ENV_KEY, "42")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => {
            if status.code() != Some(42) {
                note(&mut first, format!("null-stdio child exit {status:?}"));
            }
        }
        Err(err) => note(&mut first, format!("spawn with null stdio: {err}")),
    }

    // A child without the override must see the mismatch and exit 7.
    match Command::new(PROGRAM)
        .arg("child")
        .env(ENV_KEY, "0")
        .status()
    {
        Ok(status) => {
            if status.code() != Some(7) {
                note(&mut first, format!("env-mismatch child exit {status:?}"));
            }
        }
        Err(err) => note(&mut first, format!("spawn env-mismatch child: {err}")),
    }

    // A child with the environment cleared must also see the mismatch.
    match Command::new(PROGRAM)
        .arg("child")
        .env_clear()
        .stdout(Stdio::null())
        .status()
    {
        Ok(status) => {
            if status.code() != Some(7) {
                note(&mut first, format!("env-clear child exit {status:?}"));
            }
        }
        Err(err) => note(&mut first, format!("spawn env-clear child: {err}")),
    }

    // The parent itself started with an empty environment.
    if std::env::var(ENV_KEY).is_ok() {
        note(
            &mut first,
            "parent environment leaked the child key".to_string(),
        );
    }

    match first {
        Some(reason) => common::fail("procstress", &reason),
        None => common::pass("procstress"),
    }
}
