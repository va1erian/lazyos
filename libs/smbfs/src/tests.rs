//! [`SmbFs`] against `smbwire`'s in-memory server: the tree operations, the
//! handle pool, coherence with changes made on the server, reconnects, the
//! errors, and a seeded soak checked against `fused`'s in-memory tree.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::string::String;
use std::vec::Vec;

use fused::daemon::{FuseFs, Target};
use fused::memfs::MemFs;
use fused::payload::SetAttrRecord;
use fused::wire::errno;
use smbwire::client::{Client, Config, Signing, Transport};
use smbwire::header::command as cmd;
use smbwire::testserver::{Behaviour, Server, PASSWORD, USER};
use smbwire::Error;

use crate::{Connect, Options, SmbFs, FRESH_TICKS, IDLE_TICKS, MAX_HANDLES};

thread_local! {
    static NOW: Cell<u64> = const { Cell::new(1000) };
}

fn clock() -> u64 {
    NOW.with(Cell::get)
}

fn advance(ticks: u64) {
    NOW.with(|now| now.set(now.get() + ticks));
}

/// The server behind a switch that can cut the connection.
struct Link {
    server: Server,
    broken: bool,
}

impl Transport for Link {
    fn send(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.broken {
            return Err(Error::Transport(String::from("cut")));
        }
        self.server.send(bytes)
    }

    fn recv(&mut self) -> Result<Vec<u8>, Error> {
        if self.broken {
            return Err(Error::Transport(String::from("cut")));
        }
        self.server.recv()
    }
}

/// Hands out the server a test prepared for the next logon (none: refused).
#[derive(Clone, Default)]
struct Next(Rc<RefCell<Option<Server>>>);

impl Connect<Link> for Next {
    fn connect(&mut self) -> Result<Client<Link>, Error> {
        let server = self
            .0
            .borrow_mut()
            .take()
            .ok_or(Error::Transport(String::from("refused")))?;
        logon(server)
    }
}

fn logon(server: Server) -> Result<Client<Link>, Error> {
    let cfg = Config {
        user: USER,
        password: PASSWORD,
        domain: None,
        workstation: "LAZYOS",
        signing: Signing::Auto,
        client_guid: [0x11; 16],
        client_challenge: [0x22; 8],
        time: 0x01DA_0000_0000_0000,
    };
    let link = Link {
        server,
        broken: false,
    };
    let (mut client, _) = Client::connect(link, &cfg)?;
    client.tree_connect("10.0.2.2", "share")?;
    Ok(client)
}

fn mount_with(how: Behaviour) -> (SmbFs<Link, Next>, Next) {
    let next = Next::default();
    let client = logon(Server::new(how)).unwrap();
    let opts = Options {
        uid: 1000,
        gid: 1000,
        started: 1_800_000_000,
        clock,
    };
    (SmbFs::new(client, next.clone(), opts), next)
}

fn mount() -> SmbFs<Link, Next> {
    mount_with(Behaviour::default()).0
}

fn server(fs: &mut SmbFs<Link, Next>) -> &mut Server {
    &mut fs.client_mut().unwrap().transport().server
}

fn read_all(fs: &mut SmbFs<Link, Next>, path: &str) -> Result<Vec<u8>, u64> {
    let mut data = Vec::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = fs.read(Target::Path(path), data.len() as u64, &mut buf)?;
        if n == 0 {
            return Ok(data);
        }
        data.extend_from_slice(&buf[..n]);
    }
}

fn write_all(fs: &mut SmbFs<Link, Next>, path: &str, data: &[u8]) {
    for (i, chunk) in data.chunks(65536).enumerate() {
        let wrote = fs.write(Target::Path(path), (i * 65536) as u64, chunk);
        assert_eq!(wrote, Ok(chunk.len()));
    }
}

fn names(fs: &mut SmbFs<Link, Next>, path: &str) -> Vec<String> {
    let mut names: Vec<String> = fs
        .readdir(path)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    names
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ (i >> 8) as u8 ^ seed)
        .collect()
}

#[test]
fn the_tree_round_trips() {
    let mut fs = mount();
    let root = fs.lookup(Target::Path("")).unwrap();
    assert!(root.is_dir() && root.uid == 1000);
    assert_eq!(names(&mut fs, ""), ["docs", "hello.txt"]);
    assert_eq!(
        read_all(&mut fs, "hello.txt").unwrap(),
        b"hello from the server\n"
    );
    let hello = fs.lookup(Target::Path("hello.txt")).unwrap();
    assert!(hello.is_file() && hello.size == 22 && hello.mode & 0o777 == 0o644);
    // Read again by node, as an open file is.
    let mut buf = [0u8; 5];
    let by_node = Target::Node {
        ino: hello.ino,
        generation: 1,
    };
    assert_eq!(fs.read(by_node, 6, &mut buf), Ok(5));
    assert_eq!(&buf, b"from ");

    // A 200 KB file, written in 64 KiB requests, lands byte for byte.
    let body = pattern(200_000, 3);
    fs.create("big.bin", 0o644, 1000, 1000).unwrap();
    write_all(&mut fs, "big.bin", &body);
    assert_eq!(server(&mut fs).files["big.bin"], body);
    assert_eq!(fs.lookup(Target::Path("big.bin")).unwrap().size, 200_000);
    assert_eq!(read_all(&mut fs, "big.bin").unwrap(), body);
    // An in-place patch keeps the rest.
    assert_eq!(fs.write(Target::Path("big.bin"), 10, b"PATCH"), Ok(5));
    let mut patched = body.clone();
    patched[10..15].copy_from_slice(b"PATCH");
    assert_eq!(server(&mut fs).files["big.bin"], patched);
    fs.truncate(Target::Path("big.bin"), 1000).unwrap();
    assert_eq!(server(&mut fs).files["big.bin"].len(), 1000);
    assert_eq!(fs.lookup(Target::Path("big.bin")).unwrap().size, 1000);

    fs.mkdir("new", 0o755, 1000, 1000).unwrap();
    fs.rename("big.bin", "new/big.bin").unwrap();
    assert_eq!(names(&mut fs, "new"), ["big.bin"]);
    fs.rename("new", "moved").unwrap();
    assert_eq!(read_all(&mut fs, "moved/big.bin").unwrap().len(), 1000);
    assert_eq!(fs.rmdir("moved"), Err(errno::ENOTEMPTY));
    fs.unlink("moved/big.bin").unwrap();
    fs.rmdir("moved").unwrap();
    assert_eq!(names(&mut fs, ""), ["docs", "hello.txt"]);
    assert!(!server(&mut fs).dirs.contains("moved"));
    // A file over a file replaces it.
    fs.create("a.txt", 0o644, 1000, 1000).unwrap();
    write_all(&mut fs, "a.txt", b"new");
    fs.rename("a.txt", "hello.txt").unwrap();
    assert_eq!(read_all(&mut fs, "hello.txt").unwrap(), b"new");
    assert_eq!(names(&mut fs, ""), ["docs", "hello.txt"]);

    let figures = fs.statfs().unwrap();
    assert_eq!(figures.magic, crate::MAGIC);
    assert_eq!(figures.block_size * figures.blocks, 1000 * 8 * 512);
    fs.flush().unwrap();
    let change = SetAttrRecord::default();
    assert_eq!(fs.setattr("hello.txt", &change).unwrap().size, 3);
}

#[test]
fn errors_are_the_posix_ones() {
    let mut fs = mount();
    assert_eq!(fs.lookup(Target::Path("nope")), Err(errno::ENOENT));
    assert_eq!(fs.unlink("docs"), Err(errno::EISDIR));
    assert_eq!(fs.rmdir("hello.txt"), Err(errno::ENOTDIR));
    assert_eq!(fs.rmdir(""), Err(errno::EPERM));
    assert_eq!(
        fs.create("hello.txt", 0o644, 0, 0).map(drop),
        Err(errno::EEXIST)
    );
    assert_eq!(fs.mkdir("docs", 0o755, 0, 0).map(drop), Err(errno::EEXIST));
    assert_eq!(fs.rename("docs", "docs/inner"), Err(errno::EINVAL));
    assert_eq!(fs.rename("hello.txt", "docs"), Err(errno::EISDIR));
    assert_eq!(fs.rename("docs", "hello.txt"), Err(errno::ENOTDIR));
    assert_eq!(fs.readdir("hello.txt").map(drop), Err(errno::ENOTDIR));
    let mut buf = [0u8; 4];
    assert_eq!(
        fs.read(Target::Path("docs"), 0, &mut buf),
        Err(errno::EISDIR)
    );
    let stale = Target::Node {
        ino: 999,
        generation: 1,
    };
    assert_eq!(fs.lookup(stale), Err(errno::ESTALE));
    // A removed file's node stays stale.
    let ino = fs.lookup(Target::Path("hello.txt")).unwrap().ino;
    fs.unlink("hello.txt").unwrap();
    let gone = Target::Node { ino, generation: 1 };
    assert_eq!(fs.read(gone, 0, &mut buf), Err(errno::ESTALE));
    fs.create("hello.txt", 0o644, 0, 0).unwrap();
    assert_ne!(fs.lookup(Target::Path("hello.txt")).unwrap().ino, ino);
}

#[test]
fn handles_are_kept_bounded_and_closed_when_idle() {
    let mut fs = mount();
    let creates = |fs: &mut SmbFs<Link, Next>| {
        server(fs)
            .seen
            .iter()
            .filter(|c| **c == cmd::CREATE)
            .count()
    };
    let mut buf = [0u8; 8];
    fs.read(Target::Path("hello.txt"), 0, &mut buf).unwrap();
    let after_first = creates(&mut fs);
    for offset in 0..10 {
        fs.read(Target::Path("hello.txt"), offset, &mut buf)
            .unwrap();
    }
    assert_eq!(creates(&mut fs), after_first, "a kept handle was reopened");
    // A write upgrades the handle once.
    fs.write(Target::Path("hello.txt"), 0, b"H").unwrap();
    fs.write(Target::Path("hello.txt"), 1, b"E").unwrap();
    assert_eq!(creates(&mut fs), after_first + 1);
    assert_eq!(fs.open_handles(), 1);
    for n in 0..(MAX_HANDLES + 4) {
        let path = format!("f{n}");
        fs.create(&path, 0o644, 0, 0).unwrap();
        fs.write(Target::Path(&path), 0, b"x").unwrap();
    }
    assert_eq!(fs.open_handles(), MAX_HANDLES);
    advance(IDLE_TICKS - 1);
    fs.idle();
    assert_eq!(fs.open_handles(), MAX_HANDLES);
    advance(1);
    fs.idle();
    assert_eq!(fs.open_handles(), 0);
    // Every handle the server gave out was closed.
    let creates = creates(&mut fs);
    let closes = server(&mut fs)
        .seen
        .iter()
        .filter(|c| **c == cmd::CLOSE)
        .count();
    assert_eq!(creates, closes);
}

#[test]
fn changes_on_the_server_show_once_fresh_ticks_pass() {
    let mut fs = mount();
    assert_eq!(fs.lookup(Target::Path("hello.txt")).unwrap().size, 22);
    assert_eq!(names(&mut fs, "docs"), ["readme.md"]);
    server(&mut fs)
        .files
        .insert(String::from("hello.txt"), vec![b'x'; 50]);
    server(&mut fs)
        .files
        .insert(String::from("docs/other.md"), b"1".to_vec());
    // Believed for FRESH_TICKS: no request, the old answers.
    let asked = server(&mut fs).seen.len();
    assert_eq!(fs.lookup(Target::Path("hello.txt")).unwrap().size, 22);
    assert_eq!(fs.lookup(Target::Path("docs/other.md")), Err(errno::ENOENT));
    assert_eq!(names(&mut fs, "docs"), ["readme.md"]);
    assert_eq!(
        server(&mut fs).seen.len(),
        asked,
        "the caches were not used"
    );
    advance(FRESH_TICKS);
    assert_eq!(fs.lookup(Target::Path("hello.txt")).unwrap().size, 50);
    assert_eq!(names(&mut fs, "docs"), ["other.md", "readme.md"]);
    assert_eq!(fs.lookup(Target::Path("docs/other.md")).unwrap().size, 1);
    // Reads are never cached: a kept handle reads what is there now.
    server(&mut fs)
        .files
        .insert(String::from("hello.txt"), b"fresh".to_vec());
    let mut buf = [0u8; 16];
    assert_eq!(fs.read(Target::Path("hello.txt"), 0, &mut buf), Ok(5));
    // Removed on the server: gone after the lifetime.
    server(&mut fs).files.remove("docs/other.md");
    advance(FRESH_TICKS);
    assert_eq!(fs.lookup(Target::Path("docs/other.md")), Err(errno::ENOENT));
}

#[test]
fn a_lost_session_is_replaced_and_safe_operations_retried() {
    let (mut fs, next) = mount_with(Behaviour::default());
    let snapshot = |fs: &mut SmbFs<Link, Next>| {
        let old = server(fs);
        let mut new = Server::new(Behaviour::default());
        new.files = old.files.clone();
        new.dirs = old.dirs.clone();
        new
    };
    fs.create("kept.txt", 0o644, 0, 0).unwrap();
    write_all(&mut fs, "kept.txt", b"before");
    // Cut the connection: a read logs on again and is retried.
    let replacement = snapshot(&mut fs);
    *next.0.borrow_mut() = Some(replacement);
    fs.client_mut().unwrap().transport().broken = true;
    assert_eq!(read_all(&mut fs, "kept.txt").unwrap(), b"before");
    assert_eq!(fs.stats().reconnects, 1);
    // A create is not repeated: EIO, then the next call has a session.
    let replacement = snapshot(&mut fs);
    *next.0.borrow_mut() = Some(replacement);
    fs.client_mut().unwrap().transport().broken = true;
    assert_eq!(fs.create("new.txt", 0o644, 0, 0).map(drop), Err(errno::EIO));
    assert!(fs.client_mut().is_none());
    fs.create("new.txt", 0o644, 0, 0).unwrap();
    assert_eq!(fs.stats().reconnects, 2);
    // No server to log on to: EIO, counted, and no session.
    fs.client_mut().unwrap().transport().broken = true;
    assert_eq!(
        fs.lookup(Target::Path("kept.txt")).map(drop),
        Err(errno::EIO)
    );
    assert_eq!(fs.stats().reconnect_failures, 1);
    assert_eq!(fs.open_handles(), 0);
}

/// A seeded run of mixed operations, each answered the same as `fused`'s
/// in-memory tree answers it; then both trees are compared whole.
#[test]
fn a_seeded_soak_matches_the_in_memory_tree() {
    let mut fs = mount();
    // Start both trees empty.
    for path in ["hello.txt", "docs/readme.md"] {
        fs.unlink(path).unwrap();
    }
    fs.rmdir("docs").unwrap();
    let mut model = MemFs::new(64 << 20, 10_000, 0, 0, || 0);
    let mut seed: u64 = 0x5eed_f5b5;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let files = ["a", "b", "d/c", "d/e"];
    model.mkdir("d", 0o755, 0, 0).unwrap();
    fs.mkdir("d", 0o755, 0, 0).unwrap();
    for round in 0..3000 {
        let path = files[next(4) as usize];
        match next(6) {
            0 => {
                let (a, b) = (
                    model.create(path, 0o644, 0, 0),
                    fs.create(path, 0o644, 0, 0),
                );
                assert_eq!(a.map(drop), b.map(drop), "{round}: create {path}");
            }
            1 | 2 => {
                let offset = next(100_000);
                let data = pattern(next(70_000) as usize + 1, round as u8);
                let chunk = &data[..data.len().min(65536)];
                let a = model.write(Target::Path(path), offset, chunk);
                let b = fs.write(Target::Path(path), offset, chunk);
                assert_eq!(a, b, "{round}: write {path}");
            }
            3 => {
                let size = next(120_000);
                let a = model.truncate(Target::Path(path), size);
                let b = fs.truncate(Target::Path(path), size);
                assert_eq!(a, b, "{round}: truncate {path}");
            }
            4 => {
                let (mut x, mut y) = (vec![0u8; 4096], vec![0u8; 4096]);
                let offset = next(130_000);
                let a = model.read(Target::Path(path), offset, &mut x);
                let b = fs.read(Target::Path(path), offset, &mut y);
                assert_eq!(a, b, "{round}: read {path}");
                assert_eq!(x, y, "{round}: bytes of {path}");
            }
            _ => {
                let (a, b) = (model.unlink(path), fs.unlink(path));
                assert_eq!(a, b, "{round}: unlink {path}");
            }
        }
        if round % 100 == 0 {
            advance(FRESH_TICKS);
        }
    }
    for path in files {
        let a = model.lookup(Target::Path(path)).map(|a| a.size);
        let b = fs.lookup(Target::Path(path)).map(|a| a.size);
        assert_eq!(a, b, "size of {path}");
        if a.is_ok() {
            assert_eq!(
                read_all(&mut fs, path),
                Ok(server(&mut fs).files[path].clone())
            );
        }
    }
}
