//! A synchronous SMB 2.1 session over a [`Transport`].
//!
//! One request is outstanding at a time. The client owns the message ids, the
//! credits the server grants, the session and tree ids, and signing: when the
//! session signs, every request after the logon is signed and every response
//! must carry a valid signature (an interim `STATUS_PENDING` excepted); a
//! response that is signed when it need not be is still verified, so a forged
//! signature is never ignored.
//!
//! The logon refuses what F2 does not do rather than continuing weaker: a
//! server that logs the user on as a guest (Samba's `map to guest`, which
//! would make a wrong password look like success), a session or share that
//! requires encryption (SMB3, F6), and a server that requires signing when
//! the caller said [`Signing::Never`].

mod ops;

pub use ops::{Open, Share, MAX_LISTING};

use alloc::string::String;
use alloc::vec::Vec;

use crate::crypto::{equal, smb2_signature};
use crate::frame::{self, FrameReader};
use crate::header::{command as cmd, Header, FLAG_SIGNED, SIGNATURE_AT, SIZE, UNSOLICITED};
use crate::msg::{self, DIALECT_202, DIALECT_21, MAX_IO};
use crate::ntlm::{self, Challenge, Credentials};
use crate::spnego::{self, Hint, REJECT};
use crate::status::{MORE_PROCESSING_REQUIRED, PENDING, SUCCESS};
use crate::Error;

/// Bytes in and out. The native client passes a TCP stream; tests pass a
/// script.
pub trait Transport {
    /// Send all of `bytes`.
    fn send(&mut self, bytes: &[u8]) -> Result<(), Error>;
    /// Some received bytes; empty at the end of the stream. A transport with
    /// a deadline reports it as an error.
    fn recv(&mut self) -> Result<Vec<u8>, Error>;
}

/// When to sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signing {
    /// Sign when the server requires it.
    Auto,
    /// Always sign (`smb --sign`).
    Always,
    /// Always sign, and tell the server signing is required
    /// (`--sign-required`).
    Required,
    /// Never sign: a server that requires signing is refused (`--no-sign`).
    Never,
}

/// What the logon needs. The random values and the time come from the
/// caller: the library has neither a clock nor a random source.
pub struct Config<'a> {
    pub user: &'a str,
    pub password: &'a str,
    /// The NTLM domain; `None` takes the one the server names in its
    /// challenge (`MsvAvNbDomainName`).
    pub domain: Option<&'a str>,
    pub workstation: &'a str,
    pub signing: Signing,
    pub client_guid: [u8; 16],
    pub client_challenge: [u8; 8],
    /// The client's clock, FILETIME (used when the server sends no time).
    pub time: u64,
}

/// What the logon established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Logon {
    pub dialect: u16,
    /// The NTLM domain the user was authenticated in.
    pub domain: String,
    pub signing: bool,
    /// Whether the exchange used SPNEGO.
    pub spnego: bool,
    /// The server's clock from NEGOTIATE, FILETIME (0 if it sent none).
    pub server_time: u64,
}

/// Credits the client asks for with every request.
const CREDIT_REQUEST: u16 = 32;
/// Frames read while waiting for one response (oplock breaks, interim
/// responses) before the server is judged to be misbehaving.
const MAX_STRAY: usize = 64;

pub struct Client<T: Transport> {
    transport: T,
    reader: FrameReader,
    next_id: u64,
    credits: u32,
    dialect: u16,
    session_id: u64,
    tree_id: u32,
    key: Option<[u8; 16]>,
    sign: bool,
    max_read: u32,
    max_write: u32,
    max_transact: u32,
}

impl<T: Transport> Client<T> {
    /// Negotiate and log on.
    pub fn connect(transport: T, cfg: &Config) -> Result<(Client<T>, Logon), Error> {
        let mut client = Client {
            transport,
            reader: FrameReader::new(),
            next_id: 0,
            credits: 1,
            dialect: 0,
            session_id: 0,
            tree_id: 0,
            key: None,
            sign: false,
            max_read: MAX_IO,
            max_write: MAX_IO,
            max_transact: MAX_IO,
        };
        let (hint, server_time) = client.negotiate(cfg)?;
        let (domain, wrapped) = client.logon(cfg, hint)?;
        let logon = Logon {
            dialect: client.dialect,
            domain,
            signing: client.sign,
            spnego: wrapped,
            server_time,
        };
        Ok((client, logon))
    }

    fn negotiate(&mut self, cfg: &Config) -> Result<(Hint, u64), Error> {
        let mode = msg::SIGNING_ENABLED
            | if cfg.signing == Signing::Required {
                msg::SIGNING_REQUIRED
            } else {
                0
            };
        let body = msg::negotiate_request(mode, &cfg.client_guid, &[DIALECT_21, DIALECT_202]);
        let message = self.call(cmd::NEGOTIATE, body, &[])?;
        let r = msg::parse_negotiate(&message)?;
        if !matches!(r.dialect, DIALECT_21 | DIALECT_202) {
            return Err(Error::Dialect(r.dialect));
        }
        self.dialect = r.dialect;
        let server_requires = r.security_mode & msg::SIGNING_REQUIRED != 0;
        self.sign = match cfg.signing {
            Signing::Auto => server_requires,
            Signing::Always | Signing::Required => true,
            Signing::Never if server_requires => {
                return Err(Error::Refused("the server requires signing"))
            }
            Signing::Never => false,
        };
        // Sizes the server will accept, never above what the client asks.
        self.max_read = r.max_read.min(MAX_IO);
        self.max_write = r.max_write.min(MAX_IO);
        self.max_transact = r.max_transact.min(MAX_IO);
        if self.max_read < 4096 || self.max_write < 4096 || self.max_transact < 4096 {
            return Err(Error::Malformed("NEGOTIATE sizes below 4 KiB"));
        }
        Ok((spnego::parse_hint(&r.security_buffer)?, r.system_time))
    }

    fn logon(&mut self, cfg: &Config, hint: Hint) -> Result<(String, bool), Error> {
        let mode = (msg::SIGNING_ENABLED
            | if cfg.signing == Signing::Required {
                msg::SIGNING_REQUIRED
            } else {
                0
            }) as u8;
        let first = ntlm::negotiate(ntlm::CLIENT_FLAGS);
        let token = match hint {
            Hint::Spnego => spnego::wrap_init(&first),
            Hint::None => first,
        };
        let body = msg::session_setup_request(mode, &token)?;
        let (header, message) = self.exchange(cmd::SESSION_SETUP, body)?;
        if header.status != MORE_PROCESSING_REQUIRED {
            return Err(status_error(cmd::SESSION_SETUP, header.status));
        }
        if header.session_id == 0 {
            return Err(Error::Malformed("SESSION_SETUP without a session id"));
        }
        self.session_id = header.session_id;
        let r = msg::parse_session_setup(&message)?;
        let reply = spnego::parse_reply(&r.token)?;
        if reply.state == Some(REJECT) {
            return Err(Error::Refused("the server rejected NTLM authentication"));
        }
        let challenge =
            Challenge::parse(reply.token.ok_or(Error::Malformed("no NTLM challenge"))?)?;
        let domain = match cfg.domain {
            Some(domain) => String::from(domain),
            None => challenge.nb_domain.clone().unwrap_or_default(),
        };
        let who = Credentials {
            user: cfg.user,
            domain: &domain,
            password: cfg.password,
            workstation: cfg.workstation,
        };
        let auth = ntlm::authenticate(&challenge, &who, &cfg.client_challenge, cfg.time);
        let token = match reply.wrapped {
            true => spnego::wrap_resp(&auth.message),
            false => auth.message,
        };
        let body = msg::session_setup_request(mode, &token)?;
        let (header, message) = self.exchange(cmd::SESSION_SETUP, body)?;
        if header.status != SUCCESS {
            return Err(status_error(cmd::SESSION_SETUP, header.status));
        }
        // The key exists from here; the final response is verified if signed.
        self.key = Some(auth.session_key);
        if header.is_signed() {
            self.verify(&message)?;
        }
        let r = msg::parse_session_setup(&message)?;
        if r.flags & (msg::SESSION_FLAG_IS_GUEST | msg::SESSION_FLAG_IS_NULL) != 0 {
            return Err(Error::Refused("the server logged the user on as a guest"));
        }
        if r.flags & msg::SESSION_FLAG_ENCRYPT_DATA != 0 {
            return Err(Error::Refused("the server requires encryption (SMB3)"));
        }
        if reply.wrapped && !r.token.is_empty() {
            let last = spnego::parse_reply(&r.token)?;
            if last.state == Some(REJECT) {
                return Err(Error::Refused("the server rejected NTLM authentication"));
            }
        }
        Ok((domain, reply.wrapped))
    }

    /// Send one request and return its final response, header checked and
    /// signature verified.
    pub(crate) fn exchange(
        &mut self,
        command: u16,
        body: Vec<u8>,
    ) -> Result<(Header, Vec<u8>), Error> {
        if self.credits == 0 {
            return Err(Error::NoCredits);
        }
        let id = self.next_id;
        let tree = match command {
            cmd::NEGOTIATE | cmd::SESSION_SETUP | cmd::LOGOFF => 0,
            _ => self.tree_id,
        };
        let mut header = Header::request(command, id, tree, self.session_id);
        header.credit_charge = u16::from(self.dialect == DIALECT_21);
        header.credits = CREDIT_REQUEST;
        let signing = self.sign && command != cmd::SESSION_SETUP && self.key.is_some();
        if signing {
            header.flags |= FLAG_SIGNED;
        }
        let mut message = Vec::with_capacity(SIZE + body.len());
        header.encode(&mut message);
        message.extend_from_slice(&body);
        if let (true, Some(key)) = (signing, self.key) {
            let signature = smb2_signature(&key, &message);
            message[SIGNATURE_AT..SIZE].copy_from_slice(&signature);
        }
        self.transport.send(&frame::encode(&message)?)?;
        self.next_id += 1;
        self.credits -= 1;
        for _ in 0..MAX_STRAY {
            let reply = self.receive()?;
            let h = Header::parse(&reply)?;
            if !h.is_response() || h.next_command != 0 {
                return Err(Error::Malformed("not a single response"));
            }
            if h.message_id == UNSOLICITED {
                continue; // an oplock break the client never asked for
            }
            if h.message_id != id || h.command != command {
                return Err(Error::Malformed("a response to another request"));
            }
            self.credits = self.credits.saturating_add(u32::from(h.credits));
            if h.is_async() && h.status == PENDING {
                continue; // interim: the final response follows
            }
            if self.session_id != 0 && h.session_id != self.session_id {
                return Err(Error::Malformed("a response for another session"));
            }
            if command != cmd::SESSION_SETUP {
                if h.is_signed() {
                    self.verify(&reply)?;
                } else if signing {
                    return Err(Error::Signature);
                }
            }
            return Ok((h, reply));
        }
        Err(Error::Malformed("too many stray messages"))
    }

    /// [`exchange`](Self::exchange), with any status but success (or one of
    /// `also`) turned into an error.
    pub(crate) fn call(
        &mut self,
        command: u16,
        body: Vec<u8>,
        also: &[u32],
    ) -> Result<Vec<u8>, Error> {
        let (header, message) = self.exchange(command, body)?;
        if header.status == SUCCESS || also.contains(&header.status) {
            Ok(message)
        } else {
            Err(status_error(command, header.status))
        }
    }

    fn verify(&self, message: &[u8]) -> Result<(), Error> {
        let key = self.key.ok_or(Error::Signature)?;
        let mut copy = message.to_vec();
        let got: [u8; 16] = copy[SIGNATURE_AT..SIZE]
            .try_into()
            .map_err(|_| Error::Signature)?;
        copy[SIGNATURE_AT..SIZE].fill(0);
        if equal(&smb2_signature(&key, &copy), &got) {
            Ok(())
        } else {
            Err(Error::Signature)
        }
    }

    fn receive(&mut self) -> Result<Vec<u8>, Error> {
        loop {
            if let Some(message) = self.reader.next_frame() {
                return Ok(message);
            }
            let bytes = self.transport.recv()?;
            if bytes.is_empty() {
                return Err(Error::Closed);
            }
            self.reader.feed(&bytes)?;
        }
    }

    pub fn dialect(&self) -> u16 {
        self.dialect
    }

    pub fn signing(&self) -> bool {
        self.sign
    }

    /// Largest READ the client sends.
    pub fn max_read(&self) -> u32 {
        self.max_read
    }

    /// Largest WRITE the client sends.
    pub fn max_write(&self) -> u32 {
        self.max_write
    }

    /// The transport, for a caller that needs it back.
    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }
}

fn status_error(command: u16, status: u32) -> Error {
    Error::Status { command, status }
}
