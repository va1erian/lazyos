//! `struct termios` (the kernel's 36-byte x86_64 layout that `TCGETS` and
//! `TCSETS` move) and the flag bits the line discipline interprets.

/// `c_iflag` bits.
pub const ICRNL: u32 = 0o400;
pub const INLCR: u32 = 0o100;
pub const IGNCR: u32 = 0o200;
pub const IXON: u32 = 0o2000;
/// `c_oflag` bits.
pub const OPOST: u32 = 0o1;
pub const ONLCR: u32 = 0o4;
/// `c_lflag` bits.
pub const ISIG: u32 = 0o1;
pub const ICANON: u32 = 0o2;
pub const ECHO: u32 = 0o10;
pub const ECHOE: u32 = 0o20;
pub const ECHOK: u32 = 0o40;
pub const ECHONL: u32 = 0o100;
pub const ECHOCTL: u32 = 0o1000;
pub const ECHOKE: u32 = 0o4000;
pub const IEXTEN: u32 = 0o100000;

/// `c_cc` indices.
pub const VINTR: usize = 0;
pub const VQUIT: usize = 1;
pub const VERASE: usize = 2;
pub const VKILL: usize = 3;
pub const VEOF: usize = 4;
pub const VTIME: usize = 5;
pub const VMIN: usize = 6;
pub const VSUSP: usize = 10;
pub const VEOL: usize = 11;
pub const VWERASE: usize = 14;
pub const VLNEXT: usize = 15;
pub const VEOL2: usize = 16;
/// `NCCS` of the kernel's `struct termios`.
pub const NCCS: usize = 19;
/// Size of the kernel's `struct termios` on x86_64.
pub const SIZE: usize = 36;

/// The terminal settings, as Linux's `struct termios` holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; NCCS],
}

impl Default for Termios {
    /// What a freshly opened Linux terminal starts with (`stty sane`): cooked
    /// input with echo and signals, `\r` read as `\n`, `\n` written as `\r\n`.
    fn default() -> Termios {
        let mut cc = [0u8; NCCS];
        cc[VINTR] = 0x03; // ^C
        cc[VQUIT] = 0x1c; // ^\
        cc[VERASE] = 0x7f; // DEL
        cc[VKILL] = 0x15; // ^U
        cc[VEOF] = 0x04; // ^D
        cc[VTIME] = 0;
        cc[VMIN] = 1;
        cc[8] = 0x11; // VSTART ^Q
        cc[9] = 0x13; // VSTOP ^S
        cc[VSUSP] = 0x1a; // ^Z
        cc[12] = 0x12; // VREPRINT ^R
        cc[13] = 0x0f; // VDISCARD ^O
        cc[VWERASE] = 0x17; // ^W
        cc[VLNEXT] = 0x16; // ^V
        Termios {
            iflag: ICRNL | IXON,
            oflag: OPOST | ONLCR,
            // B38400 | CS8 | CREAD | HUPCL
            cflag: 0o17 | 0o60 | 0o200 | 0o2000,
            lflag: ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
            line: 0,
            cc,
        }
    }
}

impl Termios {
    /// The 36-byte wire form.
    pub fn to_bytes(self) -> [u8; SIZE] {
        let mut out = [0u8; SIZE];
        out[0..4].copy_from_slice(&self.iflag.to_le_bytes());
        out[4..8].copy_from_slice(&self.oflag.to_le_bytes());
        out[8..12].copy_from_slice(&self.cflag.to_le_bytes());
        out[12..16].copy_from_slice(&self.lflag.to_le_bytes());
        out[16] = self.line;
        out[17..17 + NCCS].copy_from_slice(&self.cc);
        out
    }

    /// Parse the wire form.
    pub fn from_bytes(bytes: &[u8; SIZE]) -> Termios {
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let mut cc = [0u8; NCCS];
        cc.copy_from_slice(&bytes[17..17 + NCCS]);
        Termios {
            iflag: word(0),
            oflag: word(4),
            cflag: word(8),
            lflag: word(12),
            line: bytes[16],
            cc,
        }
    }

    pub fn canonical(&self) -> bool {
        self.lflag & ICANON != 0
    }

    pub fn echo(&self) -> bool {
        self.lflag & ECHO != 0
    }

    pub fn signals(&self) -> bool {
        self.lflag & ISIG != 0
    }

    /// Whether `byte` is the (enabled) control character at `index`.
    /// `_POSIX_VDISABLE` (0) disables a slot.
    pub fn is_cc(&self, index: usize, byte: u8) -> bool {
        self.cc[index] != 0 && self.cc[index] == byte
    }
}

/// `struct winsize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WinSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for WinSize {
    fn default() -> WinSize {
        WinSize { rows: 24, cols: 80 }
    }
}
