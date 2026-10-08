//! `smbcat`: `libs/smbwire` on the host over a std TCP stream, to check the
//! client against a real server (Samba in Docker) and the harness server
//! without booting LazyOS.
//!
//! ```text
//! LAZYOS_SMB_PASSWORD=... cargo run -p smbwire --example smbcat -- \
//!     [--sign|--sign-required|--no-sign] [-W DOMAIN] HOST:PORT SHARE USER [CMD ARGS]...
//! ```
//!
//! Commands: `ls [dir]` (default), `get FILE` (to stdout), `put FILE LOCAL`,
//! `mkdir DIR`, `rm PATH`, `mv FROM TO`, `df`, `selftest` (a round trip of
//! every operation under `smbcat-selftest/`).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

use smbwire::client::{Client, Config, Open, Signing, Transport};
use smbwire::Error;

struct Tcp(TcpStream);

impl Transport for Tcp {
    fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0
            .write_all(bytes)
            .map_err(|e| Error::Transport(e.to_string()))
    }

    fn recv(&mut self) -> Result<Vec<u8>, Error> {
        let mut buffer = vec![0u8; 65536];
        let n = self
            .0
            .read(&mut buffer)
            .map_err(|e| Error::Transport(e.to_string()))?;
        buffer.truncate(n);
        Ok(buffer)
    }
}

fn read_all(c: &mut Client<Tcp>, path: &str) -> Result<Vec<u8>, Error> {
    let opened = c.open(path, Open::Read)?;
    let mut data = Vec::new();
    loop {
        let chunk = c.read(&opened.file_id, data.len() as u64, c.max_read())?;
        if chunk.is_empty() {
            break;
        }
        data.extend_from_slice(&chunk);
    }
    c.close(&opened.file_id)?;
    Ok(data)
}

fn write_all(c: &mut Client<Tcp>, path: &str, data: &[u8]) -> Result<(), Error> {
    let opened = c.open(path, Open::Replace)?;
    let mut at = 0;
    while at < data.len() {
        let end = (at + c.max_write() as usize).min(data.len());
        at += c.write(&opened.file_id, at as u64, &data[at..end])? as usize;
    }
    c.flush(&opened.file_id)?;
    c.close(&opened.file_id)
}

fn selftest(c: &mut Client<Tcp>) -> Result<(), Error> {
    let dir = "smbcat-selftest";
    let _ = c.delete(&format!("{dir}/b.bin"));
    let _ = c.delete(&format!("{dir}/a.bin"));
    let _ = c.delete(dir);
    c.mkdir(dir)?;
    let data: Vec<u8> = (0..300_000u32)
        .map(|i| (i * 131 + (i >> 9)) as u8)
        .collect();
    write_all(c, &format!("{dir}/a.bin"), &data)?;
    assert_eq!(read_all(c, &format!("{dir}/a.bin"))?, data, "read back");
    assert_eq!(
        c.stat(&format!("{dir}/a.bin"))?.end_of_file,
        data.len() as u64
    );
    c.rename(&format!("{dir}/a.bin"), &format!("{dir}/b.bin"), false)?;
    let names: Vec<String> = c.list(dir)?.into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["b.bin"], "listing after rename");
    let opened = c.open(&format!("{dir}/b.bin"), Open::Replace)?;
    c.write(&opened.file_id, 0, b"0123456789")?;
    c.set_size(&opened.file_id, 4)?;
    c.close(&opened.file_id)?;
    assert_eq!(read_all(c, &format!("{dir}/b.bin"))?, b"0123");
    let not_empty = c.delete(dir).map_err(|e| e.status());
    assert!(not_empty.is_err(), "a non-empty directory is not deleted");
    c.delete(&format!("{dir}/b.bin"))?;
    c.delete(dir)?;
    println!("selftest: ok ({} bytes round trip)", data.len());
    Ok(())
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut signing = Signing::Auto;
    let mut domain = None;
    while let Some(flag) = args.first().filter(|a| a.starts_with('-')).cloned() {
        args.remove(0);
        match flag.as_str() {
            "--sign" => signing = Signing::Always,
            "--sign-required" => signing = Signing::Required,
            "--no-sign" => signing = Signing::Never,
            "-W" => domain = Some(args.remove(0)),
            other => panic!("unknown flag {other}"),
        }
    }
    let [address, share, user, rest @ ..] = args.as_slice() else {
        eprintln!("usage: smbcat [flags] HOST:PORT SHARE USER [CMD ARGS]...");
        std::process::exit(2);
    };
    let password = std::env::var("LAZYOS_SMB_PASSWORD").expect("LAZYOS_SMB_PASSWORD");
    let stream = TcpStream::connect(address).expect("connect");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cfg = Config {
        user,
        password: &password,
        domain: domain.as_deref(),
        workstation: "SMBCAT",
        signing,
        client_guid: (now as u128 * 0x9E37_79B9_7F4A_7C15).to_le_bytes(),
        client_challenge: (now ^ 0x5DEE_CE66).to_le_bytes(),
        time: (now + 11_644_473_600) * 10_000_000,
    };
    let host = address
        .rsplit_once(':')
        .map_or(address.as_str(), |(h, _)| h);
    let result = (|| -> Result<(), Error> {
        let (mut c, logon) = Client::connect(Tcp(stream), &cfg)?;
        eprintln!("logon: {logon:?}");
        c.tree_connect(host, share)?;
        match rest
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [] | ["ls"] => c
                .list("")?
                .iter()
                .for_each(|e| println!("{:>10} {}", e.info.end_of_file, e.name)),
            ["ls", dir] => c
                .list(dir)?
                .iter()
                .for_each(|e| println!("{:>10} {}", e.info.end_of_file, e.name)),
            ["get", file] => std::io::stdout()
                .write_all(&read_all(&mut c, file)?)
                .unwrap(),
            ["put", file, local] => {
                write_all(&mut c, file, &std::fs::read(local).expect("local file"))?
            }
            ["mkdir", dir] => c.mkdir(dir)?,
            ["rm", path] => c.delete(path)?,
            ["mv", from, to] => c.rename(from, to, false)?,
            ["df"] => println!("{:?}", c.statfs()?),
            ["selftest"] => selftest(&mut c)?,
            other => panic!("unknown command {other:?}"),
        }
        c.logoff();
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("smbcat: {error:?}");
        std::process::exit(1);
    }
}
