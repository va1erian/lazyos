//! The commands of `smb`, run against a connected share.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use ftpwire::{crc32_update, Pattern};
use smbwire::client::{Client, Open};
use user::sys;

use super::link::{explain, Tcp};

/// Largest file `put` reads, and `put -g` generates.
const MAX_FILE: usize = 16 * 1024 * 1024;

pub struct Session {
    client: Client<Tcp>,
    /// The current directory, from the share root, without slashes at
    /// either end.
    cwd: String,
    quiet: bool,
    verbose: bool,
}

/// What `put` sends: a file read whole, or a generated stream.
enum Source {
    File(Vec<u8>),
    Generated(usize),
}

impl Session {
    pub fn new(client: Client<Tcp>, quiet: bool, verbose: bool) -> Session {
        Session {
            client,
            cwd: String::new(),
            quiet,
            verbose,
        }
    }

    /// `path` against the current directory, with `.` and `..` resolved
    /// (never above the share root).
    fn resolve(&self, path: &str) -> Result<String, String> {
        let mut parts: Vec<&str> = Vec::new();
        if !path.starts_with('/') {
            parts.extend(self.cwd.split('/').filter(|p| !p.is_empty()));
        }
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts
                        .pop()
                        .ok_or_else(|| format!("{path}: above the share root"))?;
                }
                name => parts.push(name),
            }
        }
        Ok(parts.join("/"))
    }

    fn cmd_ls(&mut self, args: &[String]) -> Result<(), String> {
        let path = self.resolve(args.first().map_or("", String::as_str))?;
        let entries = self.client.list(&path).map_err(|e| explain(&e))?;
        for entry in &entries {
            let kind = if entry.info.is_dir() { 'd' } else { '-' };
            sys::write_str(&format!(
                "{kind} {:>10} {}\n",
                entry.info.end_of_file, entry.name
            ));
        }
        sys::write_str(&format!("SMB:LIST n={}\n", entries.len()));
        Ok(())
    }

    fn cmd_cd(&mut self, args: &[String]) -> Result<(), String> {
        let path = self.resolve(args.first().map_or("/", String::as_str))?;
        let info = self.client.stat(&path).map_err(|e| explain(&e))?;
        if !info.is_dir() {
            return Err(format!("{path}: not a directory"));
        }
        self.cwd = path;
        Ok(())
    }

    fn cmd_get(&mut self, args: &[String]) -> Result<(), String> {
        let name = args.first().ok_or("get: which file?")?;
        let to_stdout = args.get(1).map(String::as_str) == Some("-");
        let path = self.resolve(name)?;
        let opened = self
            .client
            .open(&path, Open::Read)
            .map_err(|e| explain(&e))?;
        let (mut crc, mut total) = (0u32, 0u64);
        let result = loop {
            match self
                .client
                .read(&opened.file_id, total, self.client.max_read())
            {
                Ok(chunk) if chunk.is_empty() => break Ok(()),
                Ok(chunk) => {
                    crc = crc32_update(crc, &chunk);
                    total += chunk.len() as u64;
                    if to_stdout {
                        sys::write(&chunk);
                    }
                }
                Err(e) => break Err(explain(&e)),
            }
        };
        let closed = self.client.close(&opened.file_id).map_err(|e| explain(&e));
        result.and(closed)?;
        if !(to_stdout && self.quiet) {
            sys::write_str(&format!("SMB:GET {name} bytes={total} crc={crc:08x}\n"));
        }
        Ok(())
    }

    fn cmd_put(&mut self, args: &[String]) -> Result<(), String> {
        let (source, remote) = match args {
            [flag, n, remote] if flag == "-g" => {
                let n: usize = n.parse().map_err(|_| "put -g: bytes?")?;
                (Source::Generated(n.min(MAX_FILE)), remote.as_str())
            }
            [local, rest @ ..] => {
                let remote = rest.first().map_or(local.as_str(), String::as_str);
                let mut path = Vec::from(local.as_bytes());
                path.push(0);
                let mut buffer = alloc::vec![0u8; MAX_FILE + 1];
                let n = sys::read_file(&path, &mut buffer)
                    .ok_or_else(|| format!("put: no file {local}"))?;
                if n > MAX_FILE {
                    return Err(format!("put: {local} is over {MAX_FILE} bytes"));
                }
                buffer.truncate(n);
                (Source::File(buffer), remote)
            }
            [] => return Err(String::from("put: which file?")),
        };
        let total = match &source {
            Source::File(bytes) => bytes.len(),
            Source::Generated(n) => *n,
        };
        let path = self.resolve(remote)?;
        let opened = self
            .client
            .open(&path, Open::Replace)
            .map_err(|e| explain(&e))?;
        let mut pattern = Pattern::new();
        let mut chunk = alloc::vec![0u8; self.client.max_write() as usize];
        let (mut crc, mut sent) = (0u32, 0usize);
        let result = (|| {
            while sent < total {
                let n = (total - sent).min(chunk.len());
                match &source {
                    Source::File(bytes) => chunk[..n].copy_from_slice(&bytes[sent..sent + n]),
                    Source::Generated(_) => pattern.fill(&mut chunk[..n]),
                }
                // A short write leaves the rest of this chunk to send again.
                let mut at = 0;
                while at < n {
                    let wrote = self
                        .client
                        .write(&opened.file_id, (sent + at) as u64, &chunk[at..n])
                        .map_err(|e| explain(&e))? as usize;
                    if wrote == 0 {
                        return Err(String::from("the server wrote nothing"));
                    }
                    at += wrote;
                }
                crc = crc32_update(crc, &chunk[..n]);
                sent += n;
            }
            self.client.flush(&opened.file_id).map_err(|e| explain(&e))
        })();
        let closed = self.client.close(&opened.file_id).map_err(|e| explain(&e));
        result.and(closed)?;
        sys::write_str(&format!("SMB:PUT {remote} bytes={sent} crc={crc:08x}\n"));
        Ok(())
    }

    /// `rm` refuses a directory and `rmdir` a file, as their names promise.
    fn cmd_remove(&mut self, args: &[String], directory: bool) -> Result<(), String> {
        let path = self.resolve(args.first().ok_or("which?")?)?;
        if path.is_empty() {
            return Err(String::from("not the share root"));
        }
        let info = self.client.stat(&path).map_err(|e| explain(&e))?;
        match (directory, info.is_dir()) {
            (true, false) => Err(format!("{path}: not a directory")),
            (false, true) => Err(format!("{path}: is a directory")),
            _ => self.client.delete(&path).map_err(|e| explain(&e)),
        }
    }

    /// Run one command. `Ok(false)` means `quit`.
    fn run(&mut self, words: &[String]) -> Result<bool, String> {
        let Some((verb, args)) = words.split_first() else {
            return Ok(true);
        };
        if self.verbose {
            sys::write_str(&format!("smb: {}\n", words.join(" ")));
        }
        match verb.as_str() {
            "quit" | "bye" | "exit" => return Ok(false),
            "pwd" => sys::write_str(&format!("/{}\n", self.cwd)),
            "cd" => self.cmd_cd(args)?,
            "ls" | "dir" => self.cmd_ls(args)?,
            "get" => self.cmd_get(args)?,
            "put" => self.cmd_put(args)?,
            "mkdir" => {
                let path = self.resolve(args.first().ok_or("mkdir: which?")?)?;
                self.client.mkdir(&path).map_err(|e| explain(&e))?;
            }
            "rm" => self.cmd_remove(args, false)?,
            "rmdir" => self.cmd_remove(args, true)?,
            "mv" => {
                let [from, to] = args else {
                    return Err(String::from("mv: from to?"));
                };
                let (from, to) = (self.resolve(from)?, self.resolve(to)?);
                self.client
                    .rename(&from, &to, false)
                    .map_err(|e| explain(&e))?;
            }
            "df" => {
                let fs = self.client.statfs().map_err(|e| explain(&e))?;
                sys::write_str(&format!("total={} available={}\n", fs.total, fs.available));
            }
            other => return Err(format!("unknown command {other:?}")),
        }
        Ok(true)
    }

    /// The script, or the prompt when there is none; the commands run.
    pub fn run_all(&mut self, script: &[Vec<String>]) -> Result<usize, String> {
        let mut done = 0;
        if script.is_empty() {
            loop {
                sys::write_str("smb> ");
                let line = super::read_line(true);
                if line.is_empty() {
                    break;
                }
                let words: Vec<String> = line.split_whitespace().map(String::from).collect();
                match self.run(&words) {
                    Ok(true) => done += 1,
                    Ok(false) => break,
                    // An interactive session survives a failed command.
                    Err(message) => sys::write_str(&format!("? {message}\n")),
                }
            }
        } else {
            for words in script {
                if !self.run(words)? {
                    break;
                }
                done += 1;
            }
        }
        Ok(done)
    }

    pub fn close(mut self) {
        self.client.logoff();
    }
}
