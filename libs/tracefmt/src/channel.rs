//! The channel catalogue: one variant per program or subsystem, named after
//! the prefix its lines already carry.
//!
//! A channel is the first segment of a marker (`INIT` of `INIT:AUTOSTART:PASS`),
//! so the catalogue is the list of what the OS can say and a stray prefix is
//! a compile error, not a string nobody greps. `tools/trace/catalog.py`
//! generates `docs/trace/channels.md` from the table below; keep one
//! `Variant => "PREFIX",` row per line.
//!
//! The mask has [`MASK_BITS`] bits, one per discriminant.

use core::fmt;

/// Bits in a [`crate::Mask`]: the catalogue may grow to this many channels.
pub const MASK_BITS: usize = 128;

macro_rules! channels {
    ($($variant:ident => $prefix:literal,)*) => {
        /// One program or subsystem a line can come from.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum Channel {
            $($variant,)*
        }

        impl Channel {
            /// Every channel, in discriminant order.
            pub const ALL: &'static [Channel] = &[$(Channel::$variant,)*];

            /// The wire prefix (`INIT`), upper case.
            pub const fn prefix(self) -> &'static str {
                match self {
                    $(Channel::$variant => $prefix,)*
                }
            }
        }
    };
}

channels! {
    Abi => "ABI",
    About => "ABOUT",
    Accounts => "ACCOUNTS",
    App => "APP",
    Archiver => "ARCHIVER",
    Async => "ASYNC",
    Audio => "AUDIO",
    Audiod => "AUDIOD",
    Beep => "BEEP",
    Boot => "BOOT",
    Calc => "CALC",
    Clip => "CLIP",
    Clipboard => "CLIPBOARD",
    Confd => "CONFD",
    Confded => "CONFDED",
    Dev => "DEV",
    Devd => "DEVD",
    Devices => "DEVICES",
    Dnd => "DND",
    Docs => "DOCS",
    Doom => "DOOM",
    Editor => "EDITOR",
    Elevd => "ELEVD",
    Emusic => "EMUSIC",
    Fabmon => "FABMON",
    Fatfuse => "FATFUSE",
    Fault => "FAULT",
    Files => "FILES",
    Fs => "FS",
    Ftp => "FTP",
    Ftpfuse => "FTPFUSE",
    Golf => "GOLF",
    Greeter => "GREETER",
    Health => "HEALTH",
    Init => "INIT",
    Inputd => "INPUTD",
    Installer => "INSTALLER",
    Keyd => "KEYD",
    Label => "LABEL",
    Lazyweb => "LAZYWEB",
    Logd => "LOGD",
    Login => "LOGIN",
    Lride => "LRIDE",
    Lrplay => "LRPLAY",
    Mail => "MAIL",
    Mcp => "MCP",
    Memfuse => "MEMFUSE",
    Mime => "MIME",
    Mixer => "MIXER",
    Modplay => "MODPLAY",
    Mountd => "MOUNTD",
    Msg => "MSG",
    Msgbench => "MSGBENCH",
    Msgctl => "MSGCTL",
    Msgrd => "MSGRD",
    Nc => "NC",
    Net => "NET",
    Netapp => "NETAPP",
    Netbulk => "NETBULK",
    Netctl => "NETCTL",
    Netd => "NETD",
    Netdrives => "NETDRIVES",
    Netdrv => "NETDRV",
    Netstatus => "NETSTATUS",
    Nettools => "NETTOOLS",
    Nicctl => "NICCTL",
    Nslookup => "NSLOOKUP",
    Paint => "PAINT",
    Pdf => "PDF",
    Perf => "PERF",
    Ping => "PING",
    Pkgctl => "PKGCTL",
    Pkgd => "PKGD",
    Printd => "PRINTD",
    Quake => "QUAKE",
    Rhai => "RHAI",
    Sched => "SCHED",
    Settings => "SETTINGS",
    Shell => "SHELL",
    Shellprobe => "SHELLPROBE",
    Sky => "SKY",
    Smb => "SMB",
    Smbfuse => "SMBFUSE",
    Snd => "SND",
    Sndd => "SNDD",
    Srb2kart => "SRB2KART",
    Sys => "SYS",
    Sysmon => "SYSMON",
    Sysmond => "SYSMOND",
    Term => "TERM",
    Test => "TEST",
    Theme => "THEME",
    Timectl => "TIMECTL",
    Timed => "TIMED",
    Tls => "TLS",
    Tray => "TRAY",
    Traydemo => "TRAYDEMO",
    Ui => "UI",
    Usbd => "USBD",
    Video => "VIDEO",
    Volume => "VOLUME",
    Web => "WEB",
    Webkit => "WEBKIT",
    Webplus => "WEBPLUS",
    Widget => "WIDGET",
    Wifid => "WIFID",
    Wifi => "WIFI",
    Wifictl => "WIFICTL",
    Wifiprobe => "WIFIPROBE",
    Wifisim => "WIFISIM",
    Wlanmd => "WLANMD",
    Writer => "WRITER",
    Xdemo => "XDEMO",
    Xuiapp => "XUIAPP",
    Xuid => "XUID",
}

const _: () = assert!(
    Channel::ALL.len() <= MASK_BITS,
    "the mask is full: widen it"
);

/// A budget: at most `lines` per `window` of the caller's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    pub lines: u32,
    pub window: u64,
}

impl Channel {
    /// The mask bit's index.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The name a mask spells: the prefix in lower case (`init`).
    pub fn name(self) -> impl fmt::Display {
        struct Lower(&'static str);
        impl fmt::Display for Lower {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0
                    .chars()
                    .try_for_each(|c| fmt::Write::write_char(f, c.to_ascii_lowercase()))
            }
        }
        Lower(self.prefix())
    }

    /// The channel a mask name (`init`, any case) or a wire prefix names.
    pub fn from_name(name: &str) -> Option<Channel> {
        Channel::ALL
            .iter()
            .copied()
            .find(|channel| channel.prefix().eq_ignore_ascii_case(name))
    }

    /// Whether the default mask lets this channel through. A channel that
    /// prints for every event of a busy path (a refusal, a poll) is opt-in:
    /// its old build switch adds it to the mask.
    pub const fn default_on(self) -> bool {
        !matches!(self, Channel::Label)
    }

    /// The rate limit of a channel that can fire from a wedged peer or a
    /// tight loop, in ticks of 10 ms (a 10 s window): past it the channel
    /// prints one `SUPPRESSED` line per window instead.
    ///
    /// A limit is per channel, so only a channel every line of which is the
    /// same noisy event has one. `Ui` carries the probe's `UI:RECT` and
    /// `UI:WIDGET` evidence (a menu opens fifty rows at once, and a session
    /// clicks them by name) beside `UI:STALL`: the stall alone is limited,
    /// with a `Budget` of its own (`xui_app::stall`).
    pub const fn rate(self) -> Option<Rate> {
        match self {
            Channel::Label => Some(Rate {
                lines: 40,
                window: 1000,
            }),
            _ => None,
        }
    }
}
