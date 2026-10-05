//! Every program the image build places, by its full path in
//! [`SYSTEM_BIN`](crate::SYSTEM_BIN) (docs/filesystem-plan.md F3).
//!
//! Names are lowercase and looked up byte for byte: the OS volume is
//! case-sensitive ext2, so `/system/bin/TOP` is a different (missing) file.
//! Spawners (`init`'s manifest, the kernel's boot spawns and its `execve` of
//! native programs) use these constants; the task name of a spawned program is
//! the basename (`/system/bin/keyd` runs as `keyd`).

/// Declares one `pub const` per program, `"/system/bin/<file>"`, and [`ALL`].
/// The prefix is spelled here because `concat!` takes only literals; the
/// crate's tests check it equals [`SYSTEM_BIN`](crate::SYSTEM_BIN).
macro_rules! programs {
    ($($(#[$doc:meta])* $name:ident = $file:literal;)*) => {
        $(
            $(#[$doc])*
            pub const $name: &str = concat!("/system/bin/", $file);
        )*
        /// Every program path above, for checks that walk the table.
        pub const ALL: &[&str] = &[$($name),*];
    };
}

programs! {
    /// The services-profile boot program, the supervisor. Written by the image build.
    INIT = "init";
    /// The ABI bench's injected Linux fixture (`LAZYOS_INIT`), which owns the
    /// boot when present. Only bench images carry it. Written by the image build.
    ABI_INIT = "abi-init";
    /// `messengerd`, the Messenger registry daemon.
    MESSENGERD = "messengerd";
    /// `keyd`, the key service.
    KEYD = "keyd";
    /// `confd`, the configuration registry.
    CONFD = "confd";
    /// `timed`, the time service.
    TIMED = "timed";
    /// `inputd`, the input service.
    INPUTD = "inputd";
    /// `accountsd`, the accounts service.
    ACCOUNTSD = "accountsd";
    /// `logind`, the login service.
    LOGIND = "logind";
    /// `logd`, the log service.
    LOGD = "logd";
    /// `healthd`, the health monitor.
    HEALTHD = "healthd";
    /// `clipboardd`, the clipboard service.
    CLIPBOARDD = "clipboardd";
    /// `mimed`, the MIME database service.
    MIMED = "mimed";
    /// `pkgd`, the package manager.
    PKGD = "pkgd";
    /// The crash-test service (evidence images only).
    FLAKY = "flaky";
    /// `sndd`, the virtio-sound driver.
    SNDD = "sndd";
    /// `audiod`, the system mixer (docs/audio-plan.md).
    AUDIOD = "audiod";
    /// `netdrv`, the NIC driver.
    NETDRV = "netdrv";
    /// `netd`, the network stack service.
    NETD = "netd";
    /// `sysmond`, the system monitor service.
    SYSMOND = "sysmond";
    /// `printd`, the print spooler (an xui-app program, docs/printing-plan.md P6).
    PRINTD = "printd";
    /// `usbd`, the USB HID driver (`LAZYOS_USB=1` images).
    USBD = "usbd";
    /// `xuid`, the display compositor.
    XUID = "xuid";
    /// The `xuid` demo client.
    XDEMO = "xdemo";
    /// The single xui app image (`LAZYOS_XUI_APP`).
    XAPP = "xapp";
    /// The drag and drop demo.
    DRAGDEMO = "dragdemo";
    /// The shell-protocol evidence client.
    SHELLPROBE = "shellprobe";
    /// The hello demo window.
    HELLO = "hello";
    /// The Terminal xui app.
    TERMINAL = "terminal";
    /// The Devices xui app (issue #481).
    DEVICES = "devices";
    /// The Package Installer xui app.
    INSTALLER = "installer";
    /// LazyShell, the desktop shell xui app (issue #157).
    LAZYSHELL = "lazyshell";
    /// The image viewer (not shipped yet).
    VIEWER = "viewer";
    /// The program runner (not shipped yet).
    RUNNER = "runner";
    /// `top`, the text system monitor.
    TOP = "top";
    /// `messengerctl`, the fabric console (the shell also knows it as `msgctl`).
    MESSENGERCTL = "messengerctl";
    /// `confctl`, the configuration command line.
    CONFCTL = "confctl";
    /// The fault-injection probe.
    FAULTPROBE = "faultprobe";
    /// `beep`, the audio client.
    BEEP = "beep";
    /// `modplay`, the tracker-module player.
    MODPLAY = "modplay";
    /// `mixer`, the mixer's volume control command.
    MIXER = "mixer";
    /// `pkgctl`, the package manager command line.
    PKGCTL = "pkgctl";
    /// `nicctl`, the NIC control tool.
    NICCTL = "nicctl";
    /// `netctl`, the network stack tool.
    NETCTL = "netctl";
    /// `devctl`, the device inventory and class-rule viewer.
    DEVCTL = "devctl";
    /// `timectl`, the time service client.
    TIMECTL = "timectl";
    /// `powerctl`, the orderly shutdown/reboot command (docs/shutdown.md): the
    /// shell's `shutdown`, `poweroff`, `halt` and `reboot` run it.
    POWERCTL = "powerctl";
    /// `ping`.
    PING = "ping";
    /// `memfuse`, the in-memory user-space filesystem (docs/smb-plan.md F1).
    MEMFUSE = "memfuse";
    /// `nc`.
    NC = "nc";
    /// `nslookup`.
    NSLOOKUP = "nslookup";
    /// `ftp`, the FTP client.
    FTP = "ftp";
    /// `ftpfuse`, an FTP server mounted under `/mnt` (docs/smb-plan.md).
    FTPFUSE = "ftpfuse";
    /// `fetch`, the HTTP/HTTPS client (`LAZYOS_TLS=1`, Linux ABI;
    /// docs/tls-plan.md §7).
    FETCH = "fetch";
    /// `curl`, the same binary as [`FETCH`] with curl's option names.
    CURL = "curl";
    /// `wget`, the same binary as [`FETCH`] with wget's option names. It
    /// replaces BusyBox's `wget` applet, which does not verify certificates.
    WGET = "wget";
    /// The `std::net` Linux fixture `netd demo=1` runs (stage N5), when the
    /// harness built one.
    NETFIX = "netfix";
    /// `netbulk`, bulk TCP throughput over the native socket service
    /// (docs/performance-plan.md P4).
    NETBULK = "netbulk";
    /// The same over the Linux `AF_INET` shim: the fixture
    /// `tools/abi/fixtures/src/netbulk.rs`, when the harness built one.
    NETBULK_LINUX = "netbulk-linux";
    /// `msgbench`, the cross-process Messenger round-trip and throughput
    /// benchmark (docs/performance-plan.md P6).
    MSGBENCH = "msgbench";
    /// The clipboard copy demo.
    CLIPCP = "clipcp";
    /// The clipboard paste demo.
    CLIPPASTE = "clippaste";
    /// `rhai`, the scripting command (Linux ABI).
    RHAI = "rhai";
    /// `dash`, the Debian Almquist shell (`LAZYOS_LINUXAPPS=1`, Linux ABI).
    DASH = "dash";
    /// `lua`, the Lua 5.4 interpreter (`LAZYOS_LINUXAPPS=1`, Linux ABI).
    LUA = "lua";
    /// `sqlite3`, the SQLite shell (`LAZYOS_LINUXAPPS=1`, Linux ABI).
    SQLITE3 = "sqlite3";
    /// `jq`, the JSON processor (`LAZYOS_LINUXAPPS=1`, Linux ABI).
    JQ = "jq";
    /// `rg`, ripgrep (`LAZYOS_LINUXAPPS=1`, Linux ABI).
    RG = "rg";
    /// BusyBox, the console shell and applets (Linux ABI). The kernel resolves
    /// applet names onto it (`process/linux/path.rs`).
    BUSYBOX = "busybox";
}

/// The basename of a program path (`/system/bin/keyd` -> `keyd`), which is
/// also the task name it runs under.
pub const fn name(path: &str) -> &str {
    let bytes = path.as_bytes();
    let mut start = bytes.len();
    while start > 0 && bytes[start - 1] != b'/' {
        start -= 1;
    }
    let (_, tail) = bytes.split_at(start);
    match core::str::from_utf8(tail) {
        Ok(name) => name,
        Err(_) => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SYSTEM_BIN;

    #[test]
    fn every_program_lies_directly_under_system_bin() {
        for path in ALL {
            let rest = path
                .strip_prefix(SYSTEM_BIN)
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or_else(|| panic!("{path} is not under {SYSTEM_BIN}"));
            assert!(!rest.is_empty() && !rest.contains('/'), "{path}");
            assert_eq!(rest, name(path));
            assert!(
                rest.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{path} is not a lowercase name"
            );
        }
    }

    #[test]
    fn program_paths_are_unique() {
        for (index, path) in ALL.iter().enumerate() {
            assert!(!ALL[index + 1..].contains(path), "{path} is listed twice");
        }
    }

    #[test]
    fn the_real_init_owns_init() {
        assert_eq!(INIT, "/system/bin/init");
        assert_ne!(ABI_INIT, INIT);
    }

    #[test]
    fn name_is_the_basename() {
        assert_eq!(name(KEYD), "keyd");
        assert_eq!(name("plain"), "plain");
        assert_eq!(name("/a/"), "");
    }
}
