//! The usbredir protocol (the host side of QEMU's `usb-redir` device) and the
//! session that serves the fake dongle over it.
//!
//! usbredir carries one USB device over a byte stream: every packet is a
//! 12-byte header (type, body length, id; little endian) and a body. QEMU
//! forwards the guest's transfers as request packets and waits for the reply
//! with the same id; interrupt-IN data is pushed once QEMU started receiving
//! on the endpoint. Only the parts this device needs are implemented; the
//! capabilities QEMU's xHCI insists on (64-bit ids, 32-bit bulk lengths) are
//! spoken once both sides announce them (`wire.rs`).
//!
//! The session is single threaded: it reads with a short timeout, answers
//! what arrived, asks the controller for what it wants to send, and
//! delivers it.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::controller::{Controller, HciIn};
use crate::device::{self, Control, Setup};

mod wire;

use wire::*;

const TICK: Duration = Duration::from_millis(2);

/// What the harness asked of this session.
pub struct Options {
    /// Show the device to the guest this long after the connection (0: at
    /// once, so it is there at boot; later: a hot-plug).
    pub connect_after_ms: u64,
    /// Remove the device this long after the first HCI command (the guest
    /// has brought the controller up by then).
    pub unplug_after_ms: Option<u64>,
}

pub struct Session<C: Controller> {
    stream: TcpStream,
    input: Vec<u8>,
    /// The capabilities both sides announced.
    caps: u32,
    got_hello: bool,
    controller: C,
    options: Options,
    start: Instant,
    connected: bool,
    unplugged: bool,
    /// The device left by itself (a firmware reset) and returns at this time.
    returns_at: Option<u64>,
    first_command_at: Option<u64>,
    configuration: u8,
    alternate: [u8; 2],
    receiving: bool,
    /// Interrupt-IN chunks (each at most one packet of the endpoint).
    events: VecDeque<Vec<u8>>,
    /// Whole ACL packets the controller sent.
    acl: VecDeque<Vec<u8>>,
    /// Bulk-IN requests QEMU is waiting on: `(id, bytes wanted)`.
    bulk_requests: VecDeque<(u64, u32)>,
}

impl<C: Controller> Session<C> {
    pub fn new(stream: TcpStream, controller: C, options: Options) -> io::Result<Session<C>> {
        stream.set_read_timeout(Some(TICK))?;
        stream.set_nodelay(true)?;
        Ok(Session {
            stream,
            input: Vec::new(),
            caps: 0,
            got_hello: false,
            controller,
            options,
            start: Instant::now(),
            connected: false,
            unplugged: false,
            returns_at: None,
            first_command_at: None,
            configuration: 0,
            alternate: [0; 2],
            receiving: false,
            events: VecDeque::new(),
            acl: VecDeque::new(),
            bulk_requests: VecDeque::new(),
        })
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Whether the capability `bit` was announced by both sides.
    fn has(&self, bit: u32) -> bool {
        self.caps & (1 << bit) != 0
    }

    fn send(&mut self, kind: u32, id: u64, body: &[u8]) -> io::Result<()> {
        let mut packet = Vec::with_capacity(HEADER_WIDE + body.len());
        packet.extend_from_slice(&kind.to_le_bytes());
        packet.extend_from_slice(&(body.len() as u32).to_le_bytes());
        if self.has(CAP_64BITS_IDS) {
            packet.extend_from_slice(&id.to_le_bytes());
        } else {
            packet.extend_from_slice(&(id as u32).to_le_bytes());
        }
        packet.extend_from_slice(body);
        self.stream.write_all(&packet)
    }

    fn hello(&mut self) -> io::Result<()> {
        let mut body = vec![0u8; 64];
        let version = b"fakebt 0.1";
        body[..version.len()].copy_from_slice(version);
        body.extend_from_slice(&OUR_CAPS.to_le_bytes());
        self.send(kind::HELLO, 0, &body)
    }

    /// Serve until the peer closes the connection.
    pub fn run(&mut self) -> io::Result<()> {
        self.hello()?;
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    self.input.extend_from_slice(&chunk[..n]);
                    self.parse()?;
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => return Err(e),
            }
            self.tick()?;
        }
    }

    /// Everything that happens without the peer asking: the device appears,
    /// the controller speaks, the device is pulled out.
    fn tick(&mut self) -> io::Result<()> {
        let now = self.now_ms();
        // Not before the peer's hello: its capabilities shape `ep_info`.
        if !self.connected
            && self.returns_at.is_none()
            && self.got_hello
            && now >= self.options.connect_after_ms
        {
            self.connect()?;
        }
        if self.connected && !self.unplugged {
            self.pump(now)?;
            self.leave_if_reset(now)?;
        }
        if let Some(back) = self.returns_at.filter(|&back| now >= back) {
            self.returns_at = None;
            println!("fakebt: device returns at {now} ms (due {back})");
            self.connect()?;
        }
        if let (Some(first), Some(after)) = (self.first_command_at, self.options.unplug_after_ms) {
            if !self.unplugged && now >= first + after {
                self.unplugged = true;
                println!("fakebt: unplug at {now} ms");
                self.send(kind::DEVICE_DISCONNECT, 0, &[])?;
            }
        }
        Ok(())
    }

    /// The controller dropped off the bus (an Intel firmware reset): the
    /// device disconnects, and a new one with the same ids appears later.
    fn leave_if_reset(&mut self, now: u64) -> io::Result<()> {
        let Some(gone_for) = self.controller.take_detach(now) else {
            return Ok(());
        };
        println!("fakebt: re-enumerate at {now} ms, back in {gone_for} ms");
        self.send(kind::DEVICE_DISCONNECT, 0, &[])?;
        self.connected = false;
        self.returns_at = Some(now + gone_for);
        self.configuration = 0;
        self.alternate = [0; 2];
        self.receiving = false;
        self.events.clear();
        self.acl.clear();
        self.bulk_requests.clear();
        Ok(())
    }

    fn connect(&mut self) -> io::Result<()> {
        let mut interfaces = vec![0u8; 4 + 4 * 32];
        interfaces[..4].copy_from_slice(&2u32.to_le_bytes());
        for number in 0..2usize {
            interfaces[4 + number] = number as u8;
            interfaces[4 + 32 + number] = 0xE0;
            interfaces[4 + 64 + number] = 0x01;
            interfaces[4 + 96 + number] = 0x01;
        }
        self.send(kind::INTERFACE_INFO, 0, &interfaces)?;
        let ep_info = self.ep_info();
        self.send(kind::EP_INFO, 0, &ep_info)?;
        let mut body = vec![SPEED_HIGH, 0xE0, 0x01, 0x01];
        body.extend_from_slice(&device::vendor().to_le_bytes());
        body.extend_from_slice(&device::product().to_le_bytes());
        if self.has(CAP_CONNECT_DEVICE_VERSION) {
            body.extend_from_slice(&device::VERSION_BCD.to_le_bytes());
        }
        self.send(kind::DEVICE_CONNECT, 0, &body)?;
        self.connected = true;
        println!("fakebt: device connected at {} ms", self.now_ms());
        Ok(())
    }

    /// The endpoint index the protocol uses: the number, plus 16 for IN.
    fn index(address: u8) -> usize {
        usize::from((address & 0x80) >> 3 | (address & 0x0F))
    }

    fn ep_info(&self) -> Vec<u8> {
        let mut types = [TYPE_INVALID; 32];
        let mut intervals = [0u8; 32];
        let mut interfaces = [0u8; 32];
        let mut packets = [0u16; 32];
        // Endpoint 0 in both directions.
        for index in [0, 16] {
            types[index] = 0;
            packets[index] = 64;
        }
        for (transfer, interval, interface, packet, address) in device::endpoint_table() {
            let index = Self::index(address);
            types[index] = transfer;
            intervals[index] = interval;
            interfaces[index] = interface;
            packets[index] = packet;
        }
        let mut body = Vec::new();
        body.extend_from_slice(&types);
        body.extend_from_slice(&intervals);
        body.extend_from_slice(&interfaces);
        if self.has(CAP_EP_INFO_MAX_PACKET_SIZE) {
            for packet in packets {
                body.extend_from_slice(&packet.to_le_bytes());
            }
        }
        body
    }

    /// Take whole packets out of the input and answer each.
    fn parse(&mut self) -> io::Result<()> {
        loop {
            // The header grows once both sides announced 64-bit ids, which a
            // hello (parsed above) may have just done.
            let header = if self.has(CAP_64BITS_IDS) {
                HEADER_WIDE
            } else {
                HEADER_NARROW
            };
            if self.input.len() < header {
                break;
            }
            let word = |at: usize| {
                u32::from_le_bytes([
                    self.input[at],
                    self.input[at + 1],
                    self.input[at + 2],
                    self.input[at + 3],
                ])
            };
            let (kind, length) = (word(0), word(4) as usize);
            let id = if header == HEADER_WIDE {
                u64::from(word(8)) | u64::from(word(12)) << 32
            } else {
                u64::from(word(8))
            };
            if length > MAX_BODY {
                return Err(io::Error::new(ErrorKind::InvalidData, "packet too long"));
            }
            if self.input.len() < header + length {
                break;
            }
            let body: Vec<u8> = self.input[header..header + length].to_vec();
            self.input.drain(..header + length);
            self.handle(kind, id, &body)?;
        }
        Ok(())
    }

    fn handle(&mut self, packet: u32, id: u64, body: &[u8]) -> io::Result<()> {
        match packet {
            kind::HELLO => {
                self.got_hello = true;
                let peer = body
                    .get(64..68)
                    .map_or(0, |w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]));
                self.caps = peer & OUR_CAPS;
                Ok(())
            }
            kind::RESET => {
                self.configuration = 0;
                self.alternate = [0; 2];
                self.receiving = false;
                self.events.clear();
                self.acl.clear();
                self.bulk_requests.clear();
                Ok(())
            }
            kind::SET_CONFIGURATION => {
                self.configuration = body.first().copied().unwrap_or(0);
                self.alternate = [0; 2];
                self.send(
                    kind::CONFIGURATION_STATUS,
                    id,
                    &[STATUS_SUCCESS, self.configuration],
                )
            }
            kind::GET_CONFIGURATION => self.send(
                kind::CONFIGURATION_STATUS,
                id,
                &[STATUS_SUCCESS, self.configuration],
            ),
            kind::SET_ALT_SETTING => {
                let (interface, alternate) = (body.first().copied(), body.get(1).copied());
                match (interface, alternate) {
                    (Some(i @ 0..=1), Some(a)) => {
                        self.alternate[usize::from(i)] = a;
                        self.send(kind::ALT_SETTING_STATUS, id, &[STATUS_SUCCESS, i, a])
                    }
                    _ => self.send(kind::ALT_SETTING_STATUS, id, &[STATUS_INVAL, 0, 0]),
                }
            }
            kind::GET_ALT_SETTING => {
                let interface = body.first().copied().unwrap_or(0).min(1);
                let alternate = self.alternate[usize::from(interface)];
                self.send(
                    kind::ALT_SETTING_STATUS,
                    id,
                    &[STATUS_SUCCESS, interface, alternate],
                )
            }
            kind::START_INTERRUPT | kind::STOP_INTERRUPT => {
                self.receiving = packet == kind::START_INTERRUPT;
                let endpoint = body.first().copied().unwrap_or(device::EP_EVENTS);
                self.send(kind::INTERRUPT_STATUS, id, &[STATUS_SUCCESS, endpoint])
            }
            kind::CONTROL_PACKET => self.control(id, body),
            kind::BULK_PACKET => self.bulk(id, body),
            kind::CANCEL_DATA_PACKET => {
                if let Some(at) = self.bulk_requests.iter().position(|&(i, _)| i == id) {
                    self.bulk_requests.remove(at);
                    let reply = self.bulk_header_bytes(device::EP_ACL_IN, STATUS_CANCELLED, 0);
                    self.send(kind::BULK_PACKET, id, &reply)?;
                }
                Ok(())
            }
            other => {
                println!("fakebt: ignoring packet type {other}");
                Ok(())
            }
        }
    }

    /// A control transfer: endpoint, request, request type, status, value,
    /// index, length (10 bytes), then the data of an OUT request.
    fn control(&mut self, id: u64, body: &[u8]) -> io::Result<()> {
        let Some(head) = body.get(..10) else {
            return Ok(());
        };
        let word = |at: usize| u16::from_le_bytes([head[at], head[at + 1]]);
        let setup = Setup {
            request_type: head[2],
            request: head[1],
            value: word(4),
            index: word(6),
            length: word(8),
        };
        let now = self.now_ms();
        let (status, data) = match device::control(setup, &body[10..]) {
            Control::Done(data) => (STATUS_SUCCESS, data),
            Control::Command(packet) => {
                self.first_command_at.get_or_insert(now);
                self.controller.command(now, &packet);
                (STATUS_SUCCESS, Vec::new())
            }
            Control::Stall => (STATUS_STALL, Vec::new()),
        };
        let length = if setup.request_type & 0x80 != 0 {
            data.len() as u16
        } else {
            setup.length
        };
        let mut reply = vec![head[0], head[1], head[2], status];
        reply.extend_from_slice(&setup.value.to_le_bytes());
        reply.extend_from_slice(&setup.index.to_le_bytes());
        reply.extend_from_slice(&length.to_le_bytes());
        reply.extend_from_slice(&data);
        self.send(kind::CONTROL_PACKET, id, &reply)
    }

    /// Bytes of a bulk packet's header: endpoint, status, length (u16), stream
    /// id (u32) and, with 32-bit bulk lengths, the high half of the length.
    fn bulk_header(&self) -> usize {
        if self.has(CAP_32BITS_BULK_LENGTH) {
            10
        } else {
            8
        }
    }

    fn bulk_length(&self, head: &[u8]) -> u32 {
        let low = u32::from(u16::from_le_bytes([head[2], head[3]]));
        if self.has(CAP_32BITS_BULK_LENGTH) {
            low | u32::from(u16::from_le_bytes([head[8], head[9]])) << 16
        } else {
            low
        }
    }

    fn bulk_header_bytes(&self, endpoint: u8, status: u8, length: usize) -> Vec<u8> {
        let mut head = vec![endpoint, status];
        head.extend_from_slice(&(length as u16).to_le_bytes());
        head.extend_from_slice(&[0; 4]);
        if self.has(CAP_32BITS_BULK_LENGTH) {
            head.extend_from_slice(&((length >> 16) as u16).to_le_bytes());
        }
        head
    }

    /// A bulk transfer: endpoint, status, length (u16), stream id (u32), then
    /// the data of an OUT transfer.
    fn bulk(&mut self, id: u64, body: &[u8]) -> io::Result<()> {
        let size = self.bulk_header();
        let Some(head) = body.get(..size) else {
            return Ok(());
        };
        let (endpoint, length) = (head[0], self.bulk_length(head));
        match endpoint {
            device::EP_ACL_OUT => {
                let now = self.now_ms();
                self.controller.acl_out(now, &body[size..]);
                let reply = self.bulk_header_bytes(endpoint, STATUS_SUCCESS, body.len() - size);
                self.send(kind::BULK_PACKET, id, &reply)
            }
            device::EP_ACL_IN => {
                self.bulk_requests.push_back((id, length));
                Ok(())
            }
            _ => {
                let reply = self.bulk_header_bytes(endpoint, STATUS_STALL, 0);
                self.send(kind::BULK_PACKET, id, &reply)
            }
        }
    }

    /// Ask the controller for what it has to say and deliver it.
    fn pump(&mut self, now: u64) -> io::Result<()> {
        while let Some(packet) = self.controller.poll_in(now) {
            match packet {
                HciIn::Event(bytes) => {
                    let size = usize::from(device::EVENTS_PACKET);
                    for chunk in bytes.chunks(size) {
                        self.events.push_back(chunk.to_vec());
                    }
                }
                HciIn::Acl(bytes) => self.acl.push_back(bytes),
            }
        }
        while self.receiving {
            let Some(chunk) = self.events.pop_front() else {
                break;
            };
            let mut body = vec![device::EP_EVENTS, STATUS_SUCCESS];
            body.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
            body.extend_from_slice(&chunk);
            self.send(kind::INTERRUPT_PACKET, 0, &body)?;
        }
        while let (Some(&(id, wanted)), Some(front)) =
            (self.bulk_requests.front(), self.acl.front_mut())
        {
            let take = front.len().min(wanted as usize);
            let data: Vec<u8> = front.drain(..take).collect();
            if front.is_empty() {
                self.acl.pop_front();
            }
            self.bulk_requests.pop_front();
            let mut body = self.bulk_header_bytes(device::EP_ACL_IN, STATUS_SUCCESS, data.len());
            body.extend_from_slice(&data);
            self.send(kind::BULK_PACKET, id, &body)?;
        }
        Ok(())
    }
}
