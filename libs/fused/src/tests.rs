//! Host tests: the records, the payload codecs against hostile bytes, the
//! serve loop against a scripted provider, and the in-memory tree.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;
use std::collections::VecDeque;

use crate::daemon::{serve_one, Buffers, FuseFs, Provider};
use crate::memfs::{MemFs, MAGIC};
use crate::payload::{self, decode_dirents, encode_dirent, set, SetAttrRecord, StatFsRecord};
use crate::wire::*;

mod memfs_rules;

fn clock() -> i64 {
    1_700_000_000
}

fn tree() -> MemFs {
    MemFs::new(1 << 20, 64, 1000, 1000, clock)
}

/// A provider that hands out scripted requests and records the replies.
#[derive(Default)]
struct Script {
    queue: VecDeque<(Request, Vec<u8>)>,
    replies: Vec<(Reply, Vec<u8>)>,
}

impl Provider for Script {
    fn next(&mut self, buf: &mut [u8]) -> Result<Option<Request>, i64> {
        let Some((request, payload)) = self.queue.pop_front() else {
            return Ok(None);
        };
        buf[..payload.len()].copy_from_slice(&payload);
        Ok(Some(request))
    }

    fn reply(&mut self, reply: &Reply, data: &[u8]) -> Result<(), i64> {
        assert_eq!(reply.data_len as usize, data.len());
        self.replies.push((*reply, data.to_vec()));
        Ok(())
    }
}

/// Serve one request against `fs`; the reply and its payload.
fn ask(fs: &mut dyn FuseFs, request: Request, payload: &[u8]) -> (Reply, Vec<u8>) {
    let mut script = Script::default();
    script.queue.push_back((request, payload.to_vec()));
    let served = serve_one(fs, &mut script, &mut Buffers::new()).expect("provider");
    assert!(served);
    let (reply, data) = script.replies.pop().expect("a reply");
    assert_eq!(reply.tag, request.tag);
    (reply, data)
}

fn path_request(tag: u64, op: Op, path: &str) -> Request {
    Request {
        tag,
        op: op as u64,
        path_len: path.len() as u64,
        ..Request::default()
    }
}

fn data_request(tag: u64, op: Op, path: &str, data: &[u8], offset: u64) -> (Request, Vec<u8>) {
    let mut request = path_request(tag, op, path);
    request.len = data.len() as u64;
    request.offset = offset;
    let mut payload = path.as_bytes().to_vec();
    payload.extend_from_slice(data);
    (request, payload)
}

#[test]
fn records_round_trip() {
    let request = Request {
        tag: 7,
        op: Op::Read as u64 | FLAG_NODE,
        ino: 9,
        generation: 3,
        offset: u64::MAX,
        len: 5,
        path_len: 0,
        mode: 0o644,
        uid: 1,
        gid: 2,
    };
    assert_eq!(Request::from_words(&request.to_words()), request);
    assert_eq!(request.operation(), Some(Op::Read));
    assert!(request.by_node());
    let reply = Reply {
        tag: 1,
        status: 2,
        count: 3,
        data_len: 4,
        attr: Attr {
            ino: 5,
            generation: 6,
            mode: S_IFDIR | 0o700,
            uid: 7,
            gid: 8,
            size: 9,
            atime: -1,
            mtime: 10,
            ctime: i64::MIN,
        },
    };
    assert_eq!(Reply::from_words(&reply.to_words()), reply);
    assert!(reply.attr.is_dir() && !reply.attr.is_file());
}

#[test]
fn payload_lengths_are_bounded() {
    let mut request = path_request(1, Op::Write, "a");
    request.len = MAX_DATA as u64;
    assert_eq!(request.payload_len(), Some(1 + MAX_DATA));
    request.len = MAX_DATA as u64 + 1;
    assert_eq!(request.payload_len(), None);
    request.len = 0;
    request.path_len = MAX_PATH as u64 + 1;
    assert_eq!(request.payload_len(), None);
    // A read's `len` is room, not payload.
    let mut read = path_request(1, Op::Read, "a");
    read.len = u64::MAX;
    assert_eq!(read.payload_len(), Some(1));
    read.op = 99;
    assert_eq!(read.payload_len(), None);
}

#[test]
fn paths_and_names() {
    for good in ["", "a", "a/b", "dir/file.txt", "x y/z"] {
        assert_eq!(payload::parse_path(good.as_bytes()), Some(good), "{good:?}");
    }
    for bad in ["/a", "a/", "a//b", ".", "..", "a/../b", "a/./b", "a\0b"] {
        assert_eq!(payload::parse_path(bad.as_bytes()), None, "{bad:?}");
    }
    assert_eq!(payload::parse_path(&[0xff, 0xfe]), None);
    let long = "a".repeat(MAX_PATH + 1);
    assert_eq!(payload::parse_path(long.as_bytes()), None);
    let name = "n".repeat(MAX_NAME + 1);
    assert!(!payload::valid_name(&name));
    assert!(valid_mount_name(b"mem") && valid_mount_name(b"smb.chaton-1_x"));
    for bad in [&b""[..], b".", b"..", b"a/b", b"a b", &[b'a'; 65][..]] {
        assert!(!valid_mount_name(bad), "{bad:?}");
    }
}

#[test]
fn dirents_round_trip_and_refuse_garbage() {
    let mut buf = [0u8; 64];
    let at = encode_dirent(&mut buf, 0, 42, true, "dir").unwrap();
    let end = encode_dirent(&mut buf, at, 43, false, "f").unwrap();
    let entries = decode_dirents(&buf[..end], 2).unwrap();
    assert_eq!(entries[0].name, "dir");
    assert!(entries[0].dir && !entries[1].dir);
    assert_eq!(entries[1].ino, 43);
    // Too many, too few, leftover bytes, a bad kind, a bad name.
    assert!(decode_dirents(&buf[..end], 3).is_none());
    assert!(decode_dirents(&buf[..end], 1).is_none());
    assert!(decode_dirents(&buf[..end - 1], 2).is_none());
    assert!(decode_dirents(&buf[..end], usize::MAX).is_none());
    let mut bad = buf;
    bad[8] = 2;
    assert!(decode_dirents(&bad[..end], 2).is_none());
    let mut dots = [0u8; 16];
    let n = encode_dirent(&mut dots, 0, 1, false, "ab").unwrap();
    dots[10] = b'.';
    dots[11] = b'.';
    assert!(decode_dirents(&dots[..n], 1).is_none());
    // No room, an invalid name.
    assert!(encode_dirent(&mut buf, 60, 1, false, "long").is_none());
    assert!(encode_dirent(&mut buf, 0, 1, false, "a/b").is_none());
    assert!(encode_dirent(&mut buf, usize::MAX, 1, false, "a").is_none());
}

#[test]
fn statfs_and_setattr_records() {
    let figures = StatFsRecord {
        magic: 1,
        block_size: 2,
        blocks: 3,
        blocks_free: 4,
        files: 5,
        files_free: 6,
        name_max: 7,
    };
    let mut buf = [0u8; 64];
    let len = figures.encode(&mut buf).unwrap();
    assert_eq!(StatFsRecord::decode(&buf[..len]), Some(figures));
    assert!(StatFsRecord::decode(&buf[..len - 1]).is_none());
    let change = SetAttrRecord {
        mask: set::MODE | set::MTIME,
        mode: 0o600,
        mtime: -5,
        ..SetAttrRecord::default()
    };
    let len = change.encode(&mut buf).unwrap();
    assert_eq!(SetAttrRecord::decode(&buf[..len]), Some(change));
    buf[0] = 0x80;
    assert!(
        SetAttrRecord::decode(&buf[..len]).is_none(),
        "unknown mask bits"
    );
}

#[test]
fn serve_a_whole_session() {
    let mut fs = tree();
    let mut create = path_request(1, Op::Create, "notes.txt");
    create.mode = 0o100644;
    create.uid = 1000;
    create.gid = 1000;
    let (reply, _) = ask(&mut fs, create, b"notes.txt");
    assert_eq!(reply.status, 0);
    assert!(reply.attr.is_file() && reply.attr.mode & 0o7777 == 0o644);
    let file = reply.attr;

    let body: Vec<u8> = (0..70_000u32).map(|i| (i * 7) as u8).collect();
    let (write, payload) = data_request(2, Op::Write, "notes.txt", &body[..MAX_DATA], 0);
    let (reply, _) = ask(&mut fs, write, &payload);
    assert_eq!((reply.status, reply.count), (0, MAX_DATA as u64));
    // The tail by node.
    let mut tail = Request {
        tag: 3,
        op: Op::Write as u64 | FLAG_NODE,
        ino: file.ino,
        generation: file.generation,
        offset: MAX_DATA as u64,
        len: (body.len() - MAX_DATA) as u64,
        ..Request::default()
    };
    let (reply, _) = ask(&mut fs, tail, &body[MAX_DATA..]);
    assert_eq!(reply.count as usize, body.len() - MAX_DATA);

    let mut read = path_request(4, Op::Read, "notes.txt");
    read.offset = 1000;
    read.len = 5000;
    let (reply, data) = ask(&mut fs, read, b"notes.txt");
    assert_eq!(reply.count, 5000);
    assert_eq!(data, body[1000..6000]);
    // Past the end, and room larger than the wire allows.
    read.offset = body.len() as u64 - 10;
    read.len = u64::MAX;
    let (reply, data) = ask(&mut fs, read, b"notes.txt");
    assert_eq!((reply.count, data.len()), (10, 10));

    let mut mkdir = path_request(5, Op::Mkdir, "d");
    mkdir.mode = 0o755;
    assert_eq!(ask(&mut fs, mkdir, b"d").0.status, 0);
    let mut rename = path_request(6, Op::Rename, "notes.txt");
    rename.len = 11;
    let (reply, _) = ask(&mut fs, rename, b"notes.txtd/moved.txt");
    assert_eq!(reply.status, 0);
    let (reply, _) = ask(
        &mut fs,
        path_request(7, Op::Lookup, "notes.txt"),
        b"notes.txt",
    );
    assert_eq!(reply.status, errno::ENOENT);
    // The node handle followed the rename.
    tail.op = Op::Lookup as u64 | FLAG_NODE;
    tail.len = 0;
    let (reply, _) = ask(&mut fs, tail, b"");
    assert_eq!((reply.status, reply.attr.size), (0, body.len() as u64));

    let mut list = path_request(8, Op::ReadDir, "");
    list.len = MAX_DATA as u64;
    let (reply, data) = ask(&mut fs, list, b"");
    let entries = decode_dirents(&data, reply.count as usize).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].name.as_str(), entries[0].dir), ("d", true));
    list.offset = 1;
    let (reply, data) = ask(&mut fs, list, b"");
    assert_eq!((reply.count, data.len()), (0, 0));

    let (reply, data) = ask(&mut fs, path_request(9, Op::StatFs, ""), b"");
    let figures = StatFsRecord::decode(&data).unwrap();
    assert_eq!((reply.status, figures.magic), (0, MAGIC));
    assert_eq!(
        ask(&mut fs, path_request(10, Op::Rmdir, "d"), b"d")
            .0
            .status,
        errno::ENOTEMPTY
    );
    let unlink = path_request(11, Op::Unlink, "d/moved.txt");
    assert_eq!(ask(&mut fs, unlink, b"d/moved.txt").0.status, 0);
    assert_eq!(ask(&mut fs, tail, b"").0.status, errno::ESTALE);
    assert_eq!(
        ask(&mut fs, path_request(12, Op::Rmdir, "d"), b"d")
            .0
            .status,
        0
    );
    assert_eq!(fs.used(), 0);
    assert_eq!(fs.node_count(), 1);
}

#[test]
fn hostile_requests_never_reach_the_tree() {
    let mut fs = tree();
    let cases: Vec<(Request, Vec<u8>, u64)> = vec![
        (
            Request {
                op: 0,
                ..Request::default()
            },
            vec![],
            errno::ENOSYS,
        ),
        (
            Request {
                op: 0xFFFF,
                ..Request::default()
            },
            vec![],
            errno::ENOSYS,
        ),
        (
            path_request(1, Op::Lookup, "../etc"),
            b"../etc".to_vec(),
            errno::EINVAL,
        ),
        (
            path_request(1, Op::Lookup, "/abs"),
            b"/abs".to_vec(),
            errno::EINVAL,
        ),
        (
            Request {
                op: Op::Create as u64 | FLAG_NODE,
                ino: 1,
                ..Request::default()
            },
            vec![],
            errno::EINVAL,
        ),
        (
            Request {
                op: Op::Lookup as u64,
                path_len: MAX_PATH as u64 + 1,
                ..Request::default()
            },
            vec![],
            errno::EINVAL,
        ),
        (
            Request {
                op: Op::Write as u64,
                len: MAX_DATA as u64 + 1,
                ..Request::default()
            },
            vec![],
            errno::EINVAL,
        ),
        (
            {
                let mut r = path_request(1, Op::SetAttr, "");
                r.len = 3;
                r
            },
            b"abc".to_vec(),
            errno::EINVAL,
        ),
        (
            {
                let mut r = path_request(1, Op::Rename, "a");
                r.len = 2;
                r
            },
            b"a..".to_vec(),
            errno::EINVAL,
        ),
    ];
    for (request, payload, want) in cases {
        let (reply, data) = ask(&mut fs, request, &payload);
        assert_eq!(reply.status, want, "{request:?}");
        assert!(data.is_empty());
    }
    assert_eq!(fs.node_count(), 1);
}
