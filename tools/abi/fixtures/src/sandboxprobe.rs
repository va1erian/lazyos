//! `sandboxprobe` — the stand-in WebProcess and NetworkProcess of the browser's
//! sandbox attack harness (`python tools/webkit/attack_run.py`, issue #832,
//! `docs/architecture/process-sandbox.md`). **Test only**: an image ships it
//! only when built with `LAZYOS_WEBKIT_ATTACK=1`, as `/system/bin/webprocess`
//! and `/system/bin/networkprocess`, started by the real browser's UI process
//! exactly as it starts the real ones, so the label the kernel stamps on it is
//! the label of the real process.
//!
//! It plays the part of an exploited engine process: from inside the sandbox
//! it tries what a page's native code would try, and prints one line per
//! attempt:
//!
//! ```text
//! ATTACK:<role>:LABEL:<label name>
//! ATTACK:<role>:<probe>:BLOCKED:<errno>      refused by the policy (EPERM, EACCES) or absent (ENOSYS)
//! ATTACK:<role>:<probe>:ALLOWED:ok|<errno>   it worked, or failed for another reason
//! ATTACK:<role>:DONE
//! ```
//!
//! The judge (`tools/webkit/attackjudge.py`) holds the table of what each role
//! must be refused and what it must still be allowed, and matches every
//! `BLOCKED` of a kernel gate with a `LABEL:DENY` line of the serial log. The
//! role is the program's own name (`webprocess` or `networkprocess`); the
//! probes are the same for both, which is the point: the label, not the
//! program, decides.
//!
//! Started as `/system/bin/abi-init` (`LAZYOS_INIT`, the ABI bench's way of
//! running a fixture as the first program) it is the **driver** of
//! `python tools/webkit/attack_run.py --kernel`: it loads the rules the
//! browser's two process tables compile to as the labels `app:os.lazy.sandbox-web`
//! and `app:os.lazy.sandbox-net` (what `pkgd` does for an installed package),
//! starts itself as the two roles under those labels as the user, and waits.
//! That exercises the kernel's gates with real programs without the package
//! manager's child-label support or a WebKit build.

use std::fs::{self, File};
use std::io;
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::process::Command;

use lazyos_sys::cred;
use lazyos_sys::msg::{self, parcel};
use lazyos_sys::spawn::{spawnv_stdio, Personality, SpawnCred};
use messenger_generated::os_lazy_messenger_policy_v1 as policy;

const EPERM: i32 = 1;
const EACCES: i32 = 13;
const ENOSYS: i32 = 38;

unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn bind(fd: i32, addr: *const u8, len: u32) -> i32;
    fn close(fd: i32) -> i32;
    fn fork() -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    fn getppid() -> i32;
    fn syscall(number: i64, ...) -> i64;
}

/// The errno of a failed libc call.
fn last_errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// What an attempt came to: `Ok` when it worked, `Err(errno)` otherwise.
type Attempt = Result<(), i32>;

fn io_attempt<T>(result: io::Result<T>) -> Attempt {
    result
        .map(|_| ())
        .map_err(|error| error.raw_os_error().unwrap_or(0))
}

/// One refusal of a label is one `LABEL:DENY` line of the kernel's trace, which allows forty
/// lines in a ten second window: the probes are paced so that none is dropped.
const PACE: std::time::Duration = std::time::Duration::from_millis(350);

/// Under the real browser both engine processes probe at once, so each paces itself twice as slowly to
/// keep the two together under the label trace's rate limit (the kernel-only harness runs one at a time).
fn pace() -> std::time::Duration {
    if std::env::var_os("LAZYOS_WEBKIT_PROFILE").is_some() {
        PACE * 2
    } else {
        PACE
    }
}

fn say(role: &str, probe: &str, attempt: Attempt) {
    std::thread::sleep(pace());
    match attempt {
        Ok(()) => println!("ATTACK:{role}:{probe}:ALLOWED:ok"),
        Err(code) if matches!(code, EPERM | EACCES | ENOSYS) => {
            println!("ATTACK:{role}:{probe}:BLOCKED:{code}")
        }
        Err(code) => println!("ATTACK:{role}:{probe}:ALLOWED:{code}"),
    }
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| String::from("/home/user"))
}

/// The label this process runs under, as the kernel names it.
fn label() -> String {
    let mut buf = [0u8; cred::MAX_LABEL_BYTES];
    let me = match cred::cred_get(None) {
        Ok(me) => me,
        Err(code) => return format!("(cred_get failed: {code})"),
    };
    match cred::label_name(me.label_id, &mut buf) {
        Ok(len) => String::from_utf8_lossy(&buf[..len]).into_owned(),
        Err(_) => String::from("(unlabelled)"),
    }
}

/// `socket(AF_INET, SOCK_STREAM, 0)` and close it: whether the kernel makes one.
fn inet_socket() -> Attempt {
    // SAFETY: plain libc calls with integer arguments; the descriptor is closed.
    let fd = unsafe { socket(2, 1, 0) };
    if fd < 0 {
        return Err(last_errno());
    }
    // SAFETY: `fd` was just returned by `socket`.
    unsafe { close(fd) };
    Ok(())
}

/// `socket(AF_INET)` and `bind` to port 8080: whether the process may be a server.
fn inet_serve() -> Attempt {
    // SAFETY: plain libc calls with integer arguments.
    let fd = unsafe { socket(2, 1, 0) };
    if fd < 0 {
        return Err(last_errno());
    }
    let mut addr = [0u8; 16];
    addr[..2].copy_from_slice(&2u16.to_le_bytes());
    addr[2..4].copy_from_slice(&8080u16.to_be_bytes());
    // SAFETY: `addr` is a live `struct sockaddr_in` of 16 bytes and `fd` a socket.
    let bound = unsafe { bind(fd, addr.as_ptr(), 16) };
    let error = last_errno();
    // SAFETY: `fd` was just returned by `socket`.
    unsafe { close(fd) };
    if bound < 0 {
        Err(error)
    } else {
        Ok(())
    }
}

fn resolve(name: &str) -> Attempt {
    match parcel::resolve(name) {
        Ok(handle) => {
            let _ = lazyos_sys::msg::release(handle);
            Ok(())
        }
        Err(code) => Err((-code) as i32),
    }
}

fn write_probe(dir: &str, name: &str) -> Attempt {
    let path = format!("{dir}/{name}");
    // `create_dir_all` of a directory that is there is `mkdir` -> `EEXIST` -> fine; the
    // directories above a granted one are looked at, not written.
    let result = io_attempt(fs::create_dir_all(dir)).and_then(|()| io_attempt(File::create(&path)));
    if result.is_ok() {
        let _ = fs::remove_file(&path);
    }
    result
}

fn main() {
    let program = std::env::args().next().unwrap_or_default();
    let role = program.rsplit('/').next().unwrap_or("probe").to_string();
    let role = role.as_str();
    // The ABI bench's way of running a fixture as the first program installs it as
    // `/system/bin/init` (`LAZYOS_INIT`); the two roles of the harness are started by name.
    if !matches!(role, "webprocess" | "networkprocess") {
        std::process::exit(driver());
    }
    let home = home();
    println!("ATTACK:{role}:LABEL:{}", label());

    // What the engine process is meant to be able to do (the table of the
    // judge says, per role, which of these it must still be allowed).
    say(
        role,
        "read_fonts",
        io_attempt(fs::read_dir("/system/share/fonts")),
    );
    say(
        role,
        "read_proc_self",
        io_attempt(File::open("/proc/self/status")),
    );
    say(
        role,
        "read_dev_urandom",
        io_attempt(File::open("/dev/urandom")),
    );
    say(
        role,
        "read_resolv_conf",
        io_attempt(File::open("/etc/resolv.conf")),
    );
    say(
        role,
        "read_ca_bundle",
        io_attempt(File::open("/etc/ssl/certs/ca-certificates.crt")),
    );
    // The profile folder the browser's UI process named for its children (`LAZYOS_WEBKIT_PROFILE`:
    // `.apps/os.lazy.webkit` for WebKit, `.apps/os.lazy.lazywebplus` for LazyWeb+); the kernel-only
    // harness has none and uses WebKit's.
    let profile = std::env::var("LAZYOS_WEBKIT_PROFILE")
        .unwrap_or_else(|_| format!("{home}/.apps/os.lazy.webkit"));
    say(role, "write_profile", write_probe(&profile, "attack-probe"));
    // LazyWeb+'s network process may save downloads here; nothing else may.
    say(
        role,
        "write_downloads",
        write_probe(&format!("{home}/Downloads"), "attack-probe"),
    );
    say(role, "inet_socket", inet_socket());
    say(role, "inet_serve", inet_serve());

    // What an exploit would reach for.
    say(role, "read_home_dir", io_attempt(fs::read_dir(&home)));
    say(
        role,
        "read_home_file",
        io_attempt(File::open(format!("{home}/.profile"))),
    );
    say(
        role,
        "read_ssh_key",
        io_attempt(File::open(format!("{home}/.ssh/id_rsa"))),
    );
    say(
        role,
        "read_system_binary",
        io_attempt(File::open("/system/bin/busybox")),
    );
    say(role, "read_confd", io_attempt(File::open("/conf/confd.db")));
    say(role, "read_passwd", io_attempt(File::open("/etc/passwd")));
    say(role, "read_tty", io_attempt(File::open("/dev/tty")));
    say(role, "write_home", write_probe(&home, "sandbox-breach.txt"));
    say(
        role,
        "write_system",
        io_attempt(File::create("/system/bin/sandbox-breach")),
    );
    say(
        role,
        "write_tmp",
        io_attempt(File::create("/tmp/sandbox-breach")),
    );
    say(
        role,
        "tcp_connect",
        io_attempt(TcpStream::connect_timeout(
            &"10.0.2.2:80".parse().unwrap(),
            std::time::Duration::from_secs(1),
        )),
    );
    say(
        role,
        "unix_connect",
        io_attempt(UnixStream::connect("/tmp/sandbox-probe.sock")),
    );
    say(role, "resolve_display", resolve("os.lazy.display.v1"));
    say(role, "resolve_input", resolve("os.lazy.input.v1"));
    say(role, "resolve_clipboard", resolve("os.lazy.clipboard.v1"));
    say(
        role,
        "exec_busybox",
        io_attempt(Command::new("/system/bin/busybox").arg("true").status()),
    );
    say(
        role,
        "exec_native",
        io_attempt(Command::new("/system/bin/top").status()),
    );
    // SAFETY: `fork` takes no arguments; a child that appears is told to leave at once.
    let child = unsafe { fork() };
    if child == 0 {
        std::process::exit(0);
    }
    say(
        role,
        "fork",
        if child < 0 { Err(last_errno()) } else { Ok(()) },
    );
    // SAFETY: signal 0 only checks that the parent may be signalled.
    let killed = unsafe { kill(getppid(), 0) };
    say(
        role,
        "signal_parent",
        if killed < 0 {
            Err(last_errno())
        } else {
            Ok(())
        },
    );
    // `ptrace(PTRACE_TRACEME)` and `process_vm_readv`: not implemented, so never reachable.
    // SAFETY: raw syscalls with zero arguments; neither touches memory.
    let traced = unsafe { syscall(101, 0i64, 0i64, 0i64, 0i64) };
    say(
        role,
        "ptrace",
        if traced < 0 {
            Err(last_errno())
        } else {
            Ok(())
        },
    );
    // SAFETY: as above.
    let vm = unsafe { syscall(310, 1i64, 0i64, 0i64, 0i64, 0i64, 0i64) };
    say(
        role,
        "process_vm_readv",
        if vm < 0 { Err(last_errno()) } else { Ok(()) },
    );

    println!("ATTACK:{role}:DONE");
    if std::env::var_os("SANDBOXPROBE_EXIT").is_some() {
        return;
    }
    // Under the real browser the UI process ends its children itself: the real ones answer IPC.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

/// The rules of the two process tables of `xui-app/packages/webkit/manifest.toml`, as the
/// kernel's file gate wants them (`pkgstore::filerules`). `test_attack_judge.py` checks that
/// these lists are the manifest's.
const WEB_FILES: &[&str] = &[
    "read:/system/share/fonts",
    "read:/system/etc/fonts",
    "read:/tmp/fontconfig",
];
const NETWORK_FILES: &[&str] = &[
    "read:/etc/resolv.conf",
    "read:/etc/hosts",
    "read:/etc/ssl/certs/ca-certificates.crt",
    "write:$HOME/.apps/os.lazy.webkit",
];

/// A role of the kernel-only harness: its program name, the label it is run under, its
/// file rules, whether its `network` is `outbound`.
const ROLES: &[(&str, &str, &[&str], bool)] = &[
    ("webprocess", "app:os.lazy.sandbox-web", WEB_FILES, false),
    (
        "networkprocess",
        "app:os.lazy.sandbox-net",
        NETWORK_FILES,
        true,
    ),
];

/// `LoadLabel`: replace every rule of `label` with allow rules for `ids` (what `pkgd` does).
fn load_label(label: &str, ids: &[u64]) -> Result<(), i64> {
    let rules: Vec<policy::LabelRule> = ids
        .iter()
        .map(|&interface_id| policy::LabelRule {
            interface_id,
            method: u32::MAX,
            allow: true,
        })
        .collect();
    let body = policy::encode_load_label_args(&policy::LoadLabelArgs {
        label: label.into(),
        rules,
        // A label with no program is the kernel-only harness's: the driver starts the probe itself.
        program: None,
    })
    .map_err(|_| -22i64)?;
    let request = parcel::request(policy::INTERFACE_ID, policy::METHOD_LOADLABEL, 0, body);
    let bytes = parcel::encode(&request)?;
    msg::registry(msg::op::ACL_LOAD, msg::REGISTRY_TARGET_SELF, &bytes).map(|_| ())
}

/// Run as the first program: give each role its label and rules, start it as the user, wait.
fn driver() -> i32 {
    let home = "/home/user";
    // The user's profile directory, which the browser's UI process makes and the
    // NetworkProcess may write in; the user owns it.
    let profile = format!("{home}/.apps/os.lazy.webkit");
    println!(
        "ATTACK:driver:NOTE:mkdir {profile}: {:?}",
        fs::create_dir_all(&profile)
    );
    for path in [format!("{home}/.apps"), profile.clone()] {
        println!(
            "ATTACK:driver:NOTE:chown {path}: {:?}",
            std::os::unix::fs::chown(&path, Some(1000), Some(1000))
        );
    }
    // What the roles are granted to read must exist, so that "allowed" means a file was read
    // and not that nothing was there: a font directory (a console image has none) and the
    // resolver configuration the NetworkProcess's `/etc/resolv.conf` is backed by (`netd`
    // writes it in a real image).
    println!(
        "ATTACK:driver:NOTE:fonts {:?}",
        fs::create_dir_all("/system/share/fonts/liberation")
    );
    println!(
        "ATTACK:driver:NOTE:resolv.conf {:?}",
        fs::create_dir_all("/transient/net")
            .and_then(|()| fs::write("/transient/net/resolv.conf", "nameserver 10.0.2.3\n"))
    );
    let network = messenger_generated::os_lazy_net_socket_v1::INTERFACE_ID;
    let user = cred::Cred::new(1000, 1000, 0, 0, 0);
    // Where `LAZYOS_INIT` put this very program.
    let Some(program) = ["/system/bin/abi-init", "/system/bin/init"]
        .into_iter()
        .find(|path| fs::metadata(path).is_ok())
    else {
        println!("ATTACK:driver:FAIL:no program to start the roles from");
        return 1;
    };
    for (name, label, files, outbound) in ROLES {
        let mut ids = match filepolicy::compile(files) {
            Ok(ids) => ids,
            Err((index, error)) => {
                println!(
                    "ATTACK:driver:FAIL:rule {index} of {name}: {}",
                    error.message()
                );
                return 1;
            }
        };
        if *outbound {
            ids.push(network);
        }
        if let Err(code) = load_label(label, &ids) {
            println!("ATTACK:driver:FAIL:load {label}: {code}");
            return 1;
        }
        println!("ATTACK:driver:START:{name}:{label}");
        let pid = match spawnv_stdio(
            program,
            &[name],
            &["HOME=/home/user", "SANDBOXPROBE_EXIT=1"],
            Personality::Linux,
            SpawnCred::AsLabelled(user, label),
            None,
        ) {
            Ok(pid) => pid as i32,
            Err(code) => {
                println!("ATTACK:driver:FAIL:spawn {name}: {code}");
                return 1;
            }
        };
        let mut status = 0;
        // SAFETY: `pid` is a child of this process and `status` a live integer.
        unsafe { waitpid(pid, &mut status, 0) };
        println!("ATTACK:driver:EXIT:{name}:{status}");
    }
    println!("ATTACK:driver:DONE");
    0
}
