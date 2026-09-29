//! The interactive command loop: line editing, dispatch and error output.

use user::messenger;
use user::sys;
use user::task_snapshot;

use super::clients::{open_path, print_clipboard, print_keys, print_mime};
use super::names::{print_registry, resolve};
use super::render::{print_report, print_report_json, print_tasks_json};
use super::supervisor::{launch_app, print_apps, print_services};
use super::system::{print_health, print_log, print_sessions, verify_log};
use super::topics::{print_topics, tail};

/// The interactive command set, printed at startup and by `help`.
pub(crate) const HELP: &str = "commands: list | resolve <name> | services | health | sessions | \
                    apps | launch <app> [args] | \
                    log [tail [n]] | log verify | topics | tail <filter> [count] | \
                    mime <path> | open <path> [verb] | keys | clipboard | stats | stats-json | \
                    tasks-json | help | quit\n";

/// The registry command loop; `list` and `resolve <name>` print the name
/// table from the kernel, exactly as the spec's registry interface promises.
pub(crate) fn commands() -> ! {
    sys::write_str(HELP);
    let mut line = [0u8; 256];
    loop {
        sys::write_str("> ");
        let len = read_line(&mut line);
        let text = core::str::from_utf8(&line[..len]).unwrap_or("").trim();
        match text {
            "" => continue,
            "quit" | "exit" => sys::exit(0),
            "help" => sys::write_str(HELP),
            "list" => print_registry(),
            "services" => print_services(),
            "health" => print_health(),
            "sessions" => print_sessions(),
            "apps" => print_apps(),
            "log" => print_log(10),
            "log verify" => verify_log(),
            "stats" => match messenger::fabric_stats() {
                Ok(stats) => print_report(&stats),
                Err(error) => report(error.message()),
            },
            "stats-json" => match messenger::fabric_stats() {
                Ok(stats) => print_report_json(&stats),
                Err(error) => report(error.message()),
            },
            "tasks-json" => match task_snapshot::task_snapshot() {
                Ok(snapshot) => print_tasks_json(&snapshot),
                Err(error) => report(error.message()),
            },
            "topics" => print_topics(),
            "keys" => print_keys(),
            "clipboard" => print_clipboard(),
            _ if text.starts_with("resolve ") => resolve(text[8..].trim()),
            _ if text.starts_with("launch ") => launch_app(text[7..].trim()),
            _ if text.starts_with("log tail") => {
                let count = text[8..].trim().parse().unwrap_or(10);
                print_log(count)
            }
            _ if text.starts_with("tail ") => tail(text[5..].trim()),
            _ if text.starts_with("mime ") => print_mime(text[5..].trim()),
            _ if text.starts_with("open ") => open_path(text[5..].trim()),
            _ => report(
                "unknown command; try list, resolve <name>, services, health, sessions, \
                 apps, launch <app>, log, topics, tail, mime, open, keys, clipboard, \
                 stats, tasks-json, help, quit",
            ),
        }
    }
}

/// Read a line with basic backspace editing. Returns the byte length.
fn read_line(buffer: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let ch = sys::read_char();
        if ch == b'\n' as u64 {
            sys::write_str("\n");
            return len;
        }
        if ch == 8 {
            if len > 0 {
                len -= 1;
                sys::write_str("\u{8} \u{8}");
            }
            continue;
        }
        if (32..127).contains(&ch) && len + 1 < buffer.len() {
            buffer[len] = ch as u8;
            len += 1;
            sys::write(&[ch as u8]);
        }
    }
}

/// Print a friendly error line.
pub(crate) fn report(message: &str) {
    sys::write_str("error: ");
    sys::write_str(message);
    sys::write_str("\n");
}
