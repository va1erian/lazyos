//! DOS-style commands for `SH.ELF` (issue #6): `dir`, `cd`, `type`, `copy`,
//! `del`, `ren`, `mkdir`, `exec`, `mem`, `help`, `reboot`, `shutdown`.
//!
//! Single-tasking like the DOS it imitates: `exec` runs the program to
//! completion before the prompt returns. Every operation is a native syscall,
//! so the kernel decides what is allowed: the FAT boot volume is read-only
//! (`del`/`copy` into it report "read-only file system") while `/tmp` is a
//! writable scratch volume. Failures are printed, never panics.

use crate::files::{self, Kind};
use crate::{sys, sysinfo};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const HELP: &str = "Commands:\n\
    dir [path]          list a directory       cd [path]      change directory\n\
    type <file>         print a file           copy <a> <b>   copy a file\n\
    del <file>          delete file or empty dir\n\
    ren <a> <b>         rename or move         mkdir <dir>    make a directory\n\
    exec <file> [args]  run a program          mem            memory statistics\n\
    reboot | shutdown   restart or power off (needs admin)\n\
    Anything else is evaluated by the interpreter. The boot volume is\n\
    read-only; write under /tmp.\n";

/// Shell state: the current directory (always absolute and normalized).
pub struct Shell {
    cwd: String,
}

impl Default for Shell {
    fn default() -> Self {
        Shell::new()
    }
}

impl Shell {
    pub fn new() -> Shell {
        Shell { cwd: "/".into() }
    }

    /// The prompt text, e.g. `/tmp> `.
    pub fn prompt(&self) -> String {
        format!("{}> ", self.cwd)
    }

    /// Run `line` if its first word is a shell command. Returns `false` when
    /// it is not, so the caller can hand it to the interpreter.
    pub fn run(&mut self, line: &str) -> bool {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((&command, args)) = words.split_first() else {
            return false;
        };
        match command {
            "help" => sys::write_str(HELP),
            "dir" | "ls" => self.dir(args),
            "cd" => self.cd(args),
            "pwd" => self.say(&self.cwd.clone()),
            "type" | "cat" => self.each(command, args, 1, Self::type_file),
            "copy" | "cp" => self.each(command, args, 2, Self::copy),
            "del" | "rm" => self.each(command, args, 1, Self::del),
            "ren" | "mv" => self.each(command, args, 2, Self::ren),
            "mkdir" | "md" => self.each(command, args, 1, Self::mkdir),
            "exec" | "run" => self.exec(args),
            "mem" => self.mem(),
            "reboot" => self.power(files::REBOOT),
            "shutdown" => self.power(files::SHUTDOWN),
            _ => return false,
        }
        true
    }

    fn say(&self, text: &str) {
        sys::write_str(text);
        sys::write_str("\n");
    }

    fn fail(&self, what: &str, errno: i64) {
        self.say(&format!("{what}: {}", files::describe(errno)));
    }

    /// Run `action` when exactly `arity` arguments were given, else print usage.
    fn each(&self, name: &str, args: &[&str], arity: usize, action: fn(&Shell, &[String])) {
        if args.len() != arity {
            let operands = if arity == 1 { "<path>" } else { "<from> <to>" };
            self.say(&format!("usage: {name} {operands}"));
            return;
        }
        let paths: Vec<String> = args.iter().map(|arg| resolve(&self.cwd, arg)).collect();
        action(self, &paths);
    }

    fn dir(&self, args: &[&str]) {
        let path = args
            .first()
            .map_or(self.cwd.clone(), |a| resolve(&self.cwd, a));
        match files::list(&path) {
            Ok(entries) => {
                self.say(&format!(" Directory of {path}"));
                for entry in &entries {
                    match entry.kind {
                        Kind::Dir => self.say(&format!("{:>10}  <DIR>  {}", "", entry.name)),
                        Kind::File => {
                            self.say(&format!("{:>10}         {}", entry.size, entry.name))
                        }
                    }
                }
                self.say(&format!("{} entries", entries.len()));
            }
            Err(errno) => self.fail(&path, errno),
        }
    }

    fn cd(&mut self, args: &[&str]) {
        let Some(arg) = args.first() else {
            let cwd = self.cwd.clone();
            self.say(&cwd);
            return;
        };
        let path = resolve(&self.cwd, arg);
        match files::stat(&path) {
            Ok((_, Kind::Dir)) => self.cwd = path,
            Ok(_) => self.fail(&path, 20),
            Err(errno) => self.fail(&path, errno),
        }
    }

    fn type_file(&self, paths: &[String]) {
        match files::read_all(&paths[0]) {
            Ok(bytes) => sys::write(&bytes),
            Err(errno) => self.fail(&paths[0], errno),
        }
    }

    fn copy(&self, paths: &[String]) {
        let target = self.copy_target(&paths[0], &paths[1]);
        match files::read_all(&paths[0]) {
            Ok(bytes) => match files::write_file(&target, &bytes) {
                Ok(()) => self.say(&format!("copied {} bytes to {target}", bytes.len())),
                Err(errno) => self.fail(&target, errno),
            },
            Err(errno) => self.fail(&paths[0], errno),
        }
    }

    /// `copy a /tmp` (destination is a directory) copies to `/tmp/a`.
    fn copy_target(&self, from: &str, to: &str) -> String {
        match files::stat(to) {
            Ok((_, Kind::Dir)) => join(to, from.rsplit('/').next().unwrap_or(from)),
            _ => to.to_string(),
        }
    }

    fn del(&self, paths: &[String]) {
        if let Err(errno) = files::remove(&paths[0]) {
            self.fail(&paths[0], errno);
        }
    }

    fn ren(&self, paths: &[String]) {
        if let Err(errno) = files::rename(&paths[0], &paths[1]) {
            self.fail(&paths[0], errno);
        }
    }

    fn mkdir(&self, paths: &[String]) {
        if let Err(errno) = files::mkdir(&paths[0]) {
            self.fail(&paths[0], errno);
        }
    }

    fn exec(&self, args: &[&str]) {
        let Some((program, rest)) = args.split_first() else {
            self.say("usage: exec <file> [args]");
            return;
        };
        let mut cmdline = resolve(&self.cwd, program);
        for arg in rest {
            cmdline.push(' ');
            cmdline.push_str(arg);
        }
        cmdline.push('\0');
        let Some(pid) = sys::spawn(cmdline.as_bytes()) else {
            self.say(&format!("{program}: cannot execute"));
            return;
        };
        // Block until *this* child exits (other children may be reaped first).
        while let Some((child, status)) = sys::wait(0) {
            if child == pid {
                self.say(&describe_exit(status));
                return;
            }
        }
    }

    fn mem(&self) {
        match sysinfo::snapshot() {
            Ok(s) => {
                let kib = |frames: u64| frames * 4;
                self.say(&format!(
                    "frames: {} KiB total, {} KiB used, {} KiB free",
                    kib(s.frames_total),
                    kib(s.frames_live),
                    kib(s.frames_free)
                ));
                self.say(&format!(
                    "heap: {} KiB total, {} KiB used, {} KiB free; {} tasks",
                    s.heap_total / 1024,
                    s.heap_used / 1024,
                    s.heap_free / 1024,
                    s.tasks_live
                ));
            }
            Err(errno) => self.fail("mem", -errno),
        }
    }

    fn power(&self, op: u64) {
        // Returns only when the kernel refuses.
        if let Err(errno) = files::power(op) {
            self.fail("power", errno);
        }
    }
}

fn describe_exit(status: u64) -> String {
    match status {
        0 => "program exited".into(),
        129..=159 => format!(
            "program killed by signal {} (status {status})",
            status - 128
        ),
        _ => format!("program exited with status {status}"),
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Resolve `arg` against `cwd` into an absolute path with `.`/`..` folded and
/// no empty components. `..` never climbs above the root.
pub fn resolve(cwd: &str, arg: &str) -> String {
    let joined = if arg.starts_with('/') {
        arg.to_string()
    } else {
        format!("{cwd}/{arg}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    let mut path = String::from("/");
    path.push_str(&parts.join("/"));
    path
}
