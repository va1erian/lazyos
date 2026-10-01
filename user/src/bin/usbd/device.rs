//! One USB device on a root port: reset, address, read its descriptors,
//! configure its HID interface and run the interrupt-IN pipe (xHCI 4.3,
//! USB 2.0 9.1.2).
//!
//! Everything the device returns is parsed from a copy with `libs/usbhid`;
//! a device that answers nonsense is reported and left unconfigured.

use alloc::format;

use usbhid::desc::{self, Config, DeviceDescriptor, HidInterface, Protocol};
use usbhid::report::{self, Pointer};
use user::sys;
use xhci::context::{EndpointContext, EndpointType, InputContext, SlotContext, INPUT_CONTEXTS};
use xhci::regs::{self, portsc, Speed};
use xhci::ring::{ProducerRing, RawMem};
use xhci::trb::{self, code, kind, request, SetupPacket, Trb};

use super::hc::{nap, Hc};
use super::mem::{Region, PAGE};
use super::Error;

/// TRBs per transfer ring.
const RING_TRBS: usize = 32;
/// Layout of a device's region: input context (page 0), output context and
/// both transfer rings (page 1), descriptor and report buffers (page 2).
const INPUT: usize = 0;
const OUTPUT: usize = PAGE;
const EP0_RING: usize = 2 * PAGE - 2 * RING_TRBS * 16;
const INT_RING: usize = 2 * PAGE - RING_TRBS * 16;
const DATA: usize = 2 * PAGE;
const DATA_BYTES: usize = 1024;
const REPORT: usize = DATA + DATA_BYTES;
const DEVICE_BYTES: usize = 3 * PAGE;
/// Largest report this driver reads (boot reports are 8 bytes).
pub(super) const MAX_REPORT: usize = 64;
/// Ticks a port reset may take.
const RESET_TICKS: u64 = 100;

/// A configured HID device with its interrupt pipe running.
pub(super) struct Device {
    pub(super) port: u8,
    pub(super) slot: u8,
    pub(super) descriptor: DeviceDescriptor,
    pub(super) hid: HidInterface,
    /// Report-protocol devices (a tablet): where X, Y, wheel and buttons are
    /// in a report. `None` for boot keyboards and mice.
    pub(super) layout: Option<Pointer>,
    /// The interrupt endpoint's Device Context Index.
    pub(super) dci: u8,
    mem: Region,
    ep0: ProducerRing<RawMem>,
    interrupt: ProducerRing<RawMem>,
    report_len: u16,
    /// The interrupt TRB in flight.
    in_flight: Option<u64>,
}

/// Reset `port` if it is a USB 2 port (USB 3 ports enable themselves) and
/// return its speed once enabled; `None` when nothing usable is attached.
pub(super) fn reset_port(hc: &mut Hc, port: u8) -> Option<Speed> {
    let status = hc.portsc(port);
    if status & portsc::CCS == 0 {
        return None;
    }
    let speed = Speed::of_port(status)?;
    if matches!(speed, Speed::Low | Speed::Full | Speed::High) {
        hc.set_portsc(port, portsc::set(status, portsc::PR));
        let deadline = sys::clock() + RESET_TICKS;
        while hc.portsc(port) & portsc::PRC == 0 {
            if sys::clock() > deadline {
                return None;
            }
            nap();
        }
    }
    let status = hc.portsc(port);
    hc.set_portsc(port, portsc::ack_changes(status));
    if status & portsc::PED == 0 {
        return None;
    }
    // The speed is only final after the reset.
    Speed::of_port(status)
}

impl Device {
    /// Address the device on `port`, read and check its descriptors, and
    /// configure its first boot HID interface. `Ok(None)` for a device that
    /// is not a boot keyboard or mouse (reported, then left alone). Every
    /// failure gives the slot and its memory back.
    pub(super) fn attach(hc: &mut Hc, port: u8, speed: Speed) -> Result<Option<Device>, Error> {
        let slot = hc.command(trb::enable_slot())?.slot();
        if slot == 0 {
            return Err(Error::Completion(kind::ENABLE_SLOT, 0));
        }
        let mem = match hc.take_region(slot, DEVICE_BYTES) {
            Ok(mem) => mem,
            Err(error) => {
                let _ = hc.command(trb::disable_slot(slot));
                return Err(error);
            }
        };
        let mut device = Device::new(port, slot, mem)?;
        match device.bring_up(hc, speed) {
            Ok(true) => Ok(Some(device)),
            Ok(false) => {
                device.release(hc);
                Ok(None)
            }
            Err(error) => {
                device.release(hc);
                Err(error)
            }
        }
    }

    fn new(port: u8, slot: u8, mem: Region) -> Result<Device, Error> {
        let ep0 = ProducerRing::new(mem.ring(EP0_RING, RING_TRBS)).map_err(Error::Xhci)?;
        let interrupt = ProducerRing::new(mem.ring(INT_RING, RING_TRBS)).map_err(Error::Xhci)?;
        Ok(Device {
            port,
            slot,
            descriptor: DeviceDescriptor {
                usb: 0,
                class: 0,
                subclass: 0,
                protocol: 0,
                max_packet0: 0,
                vendor: 0,
                product: 0,
                configurations: 0,
            },
            hid: HidInterface {
                number: 0,
                alternate: 0,
                protocol: Protocol::None,
                report_len: 0,
                endpoint: None,
            },
            layout: None,
            dci: 0,
            mem,
            ep0,
            interrupt,
            report_len: 0,
            in_flight: None,
        })
    }

    /// Address the device, read its descriptors and configure it; `false`
    /// when it has no boot interface.
    fn bring_up(&mut self, hc: &mut Hc, speed: Speed) -> Result<bool, Error> {
        hc.set_device_context(self.slot, self.mem.bus(OUTPUT));
        let max_packet0 = speed.default_max_packet0();
        let dequeue = self.ep0.dequeue_pointer();
        {
            let mut input = input_context(&mut self.mem, hc.info.context_64)?;
            input
                .slot(&SlotContext {
                    route: 0,
                    speed,
                    entries: 1,
                    root_port: self.port,
                })
                .map_err(Error::Xhci)?;
            input
                .endpoint(1, &ep0_context(max_packet0, dequeue))
                .map_err(Error::Xhci)?;
        }
        hc.command(trb::address_device(self.mem.bus(INPUT), self.slot, false))?;
        self.read_descriptors(hc, speed)?;
        let config = self.read_config(hc)?;
        // A boot keyboard or mouse, else any HID interface whose report
        // descriptor holds a pointer (a tablet has no boot protocol).
        let Some(hid) = config.first_boot().or_else(|| config.first_hid()) else {
            return Ok(false);
        };
        self.hid = hid;
        self.configure(hc, &config, speed)?;
        Ok(true)
    }

    /// Give the slot back: Disable Slot stops every endpoint and the
    /// controller lets go of the contexts and rings, after which the memory
    /// can serve the slot's next device. If the command fails the
    /// controller may still own the memory, so it is never reused.
    pub(super) fn release(self, hc: &mut Hc) {
        let slot = self.slot;
        match hc.command(trb::disable_slot(slot)) {
            Ok(_) => {
                hc.discard_slot(slot);
                hc.set_device_context(slot, 0);
                hc.give_region(slot, self.mem);
            }
            Err(error) => sys::write_str(&format!(
                "USBD:SLOT:LEAK slot={slot} disable failed: {error}\n"
            )),
        }
    }

    /// The device descriptor, fixing endpoint 0's packet size first.
    fn read_descriptors(&mut self, hc: &mut Hc, speed: Speed) -> Result<(), Error> {
        let mut head = [0u8; 8];
        self.control_in(
            hc,
            request::get_descriptor(desc::kind::DEVICE, 0, 8),
            &mut head,
        )?;
        let max_packet0 = match (speed, head[7]) {
            // USB 3 encodes the size as a power of two.
            (Speed::Super | Speed::SuperPlus, exponent) => 1u16 << exponent.min(9),
            (_, size @ (8 | 16 | 32 | 64)) => u16::from(size),
            _ => return Err(Error::Descriptor("bMaxPacketSize0")),
        };
        if max_packet0 != speed.default_max_packet0() {
            let mut input = input_context(&mut self.mem, hc.info.context_64)?;
            input.ep0_max_packet(max_packet0).map_err(Error::Xhci)?;
            hc.command(trb::evaluate_context(self.mem.bus(INPUT), self.slot))?;
        }
        let mut bytes = [0u8; desc::DEVICE_LEN];
        let setup = request::get_descriptor(desc::kind::DEVICE, 0, desc::DEVICE_LEN as u16);
        self.control_in(hc, setup, &mut bytes)?;
        self.descriptor = desc::parse_device(&bytes).map_err(|_| Error::Descriptor("device"))?;
        sys::write_str(&format!(
            "USBD:DESC:DEVICE port={} {}\n",
            self.port,
            hex(&bytes)
        ));
        Ok(())
    }

    /// The whole first configuration descriptor chain.
    fn read_config(&mut self, hc: &mut Hc) -> Result<Config, Error> {
        let mut header = [0u8; desc::CONFIG_LEN];
        let setup = request::get_descriptor(desc::kind::CONFIGURATION, 0, header.len() as u16);
        self.control_in(hc, setup, &mut header)?;
        let total = desc::config_total_len(&header).map_err(|_| Error::Descriptor("config"))?;
        let mut chain = [0u8; DATA_BYTES];
        let chain = chain
            .get_mut(..usize::from(total))
            .ok_or(Error::Descriptor("wTotalLength"))?;
        let setup = request::get_descriptor(desc::kind::CONFIGURATION, 0, total);
        self.control_in(hc, setup, chain)?;
        sys::write_str(&format!(
            "USBD:DESC:CONFIG port={} {}\n",
            self.port,
            hex(chain)
        ));
        desc::parse_config(chain).map_err(|_| Error::Descriptor("config chain"))
    }

    /// SET_CONFIGURATION, the boot protocol (or, for a report-protocol
    /// device, its report descriptor), then the interrupt endpoint.
    fn configure(&mut self, hc: &mut Hc, config: &Config, speed: Speed) -> Result<(), Error> {
        let endpoint = self.hid.endpoint.ok_or(Error::Descriptor("endpoint"))?;
        self.control_out(hc, request::set_configuration(config.value))?;
        if self.hid.protocol == Protocol::None {
            // Report protocol is the default; SET_PROTOCOL is only for boot
            // devices (QEMU's tablet stalls it).
            self.layout = Some(self.read_report_layout(hc)?);
        } else {
            self.control_out(hc, request::set_protocol(self.hid.number, true))?;
        }
        if self.hid.protocol == Protocol::Keyboard {
            // Report only on change. Mice may stall it, so only keyboards
            // (which must support it) get it.
            self.control_out(hc, request::set_idle(self.hid.number))?;
        }
        self.dci = regs::dci(endpoint.address);
        self.report_len = endpoint.max_packet.clamp(1, MAX_REPORT as u16);
        let dequeue = self.interrupt.dequeue_pointer();
        {
            let mut input = input_context(&mut self.mem, hc.info.context_64)?;
            input
                .slot(&SlotContext {
                    route: 0,
                    speed,
                    entries: self.dci,
                    root_port: self.port,
                })
                .map_err(Error::Xhci)?;
            input
                .endpoint(
                    self.dci,
                    &EndpointContext {
                        kind: EndpointType::InterruptIn,
                        max_packet: endpoint.max_packet,
                        interval: regs::interrupt_interval(speed, endpoint.interval),
                        dequeue,
                        average_trb: self.report_len,
                    },
                )
                .map_err(Error::Xhci)?;
        }
        hc.command(trb::configure_endpoint(self.mem.bus(INPUT), self.slot))?;
        self.queue_report(hc)
    }

    /// The interface's report descriptor, parsed for a pointer.
    fn read_report_layout(&mut self, hc: &mut Hc) -> Result<Pointer, Error> {
        let len = usize::from(self.hid.report_len);
        if len == 0 || len > DATA_BYTES {
            return Err(Error::Descriptor("report descriptor length"));
        }
        let mut bytes = [0u8; DATA_BYTES];
        let setup = request::get_report_descriptor(self.hid.number, len as u16);
        self.control_in(hc, setup, &mut bytes[..len])?;
        sys::write_str(&format!(
            "USBD:DESC:REPORT port={} {}\n",
            self.port,
            hex(&bytes[..len])
        ));
        report::parse_pointer(&bytes[..len])
            .map_err(|_| Error::Descriptor("no pointer in the report descriptor"))
    }

    /// Queue the next interrupt-IN transfer.
    pub(super) fn queue_report(&mut self, hc: &mut Hc) -> Result<(), Error> {
        let transfer = trb::interrupt_in(self.mem.bus(REPORT), self.report_len);
        let pointer = self
            .interrupt
            .enqueue(&[transfer], false)
            .map_err(Error::Xhci)?;
        self.in_flight = Some(pointer);
        hc.doorbell(self.slot, self.dci);
        Ok(())
    }

    /// Whether `event` is this device's interrupt-IN completion.
    pub(super) fn owns(&self, event: &Trb) -> bool {
        event.kind() == kind::TRANSFER_EVENT
            && event.slot() == self.slot
            && event.endpoint() == self.dci
    }

    /// Take a completed report into `out`; returns its length, or `None` for
    /// a failed transfer (the caller decides whether to give up on the device).
    pub(super) fn take_report(&mut self, event: &Trb, out: &mut [u8; MAX_REPORT]) -> Option<usize> {
        let pointer = self.in_flight.take()?;
        if event.parameter != pointer || self.interrupt.retire(pointer).is_err() {
            return None;
        }
        if !matches!(event.completion_code(), code::SUCCESS | code::SHORT_PACKET) {
            return None;
        }
        let residual = event.residual().min(u32::from(self.report_len)) as usize;
        let len = usize::from(self.report_len) - residual;
        self.mem.read(REPORT, &mut out[..len]);
        Some(len)
    }

    /// A control transfer with a device-to-host data stage into `out`.
    fn control_in(&mut self, hc: &mut Hc, setup: SetupPacket, out: &mut [u8]) -> Result<(), Error> {
        self.control(hc, &setup)?;
        self.mem.read(DATA, out);
        Ok(())
    }

    fn control_out(&mut self, hc: &mut Hc, setup: SetupPacket) -> Result<(), Error> {
        self.control(hc, &setup)
    }

    fn control(&mut self, hc: &mut Hc, setup: &SetupPacket) -> Result<(), Error> {
        if usize::from(setup.length) > DATA_BYTES {
            return Err(Error::Descriptor("request too long"));
        }
        let (trbs, count) = trb::control_transfer(setup, self.mem.bus(DATA));
        let status = self
            .ep0
            .enqueue(&trbs[..count], true)
            .map_err(Error::Xhci)?;
        hc.doorbell(self.slot, 1);
        let slot = self.slot;
        let event =
            hc.wait(|e| e.kind() == kind::TRANSFER_EVENT && e.slot() == slot && e.endpoint() == 1)?;
        // A failure names the TRB that failed (possibly the data stage); the
        // endpoint is then halted and this device is abandoned.
        if event.parameter != status || event.completion_code() != code::SUCCESS {
            return Err(Error::Completion(setup.request, event.completion_code()));
        }
        self.ep0.retire(status).map_err(Error::Xhci)
    }
}

/// Lay a fresh input context over the device region's first page.
fn input_context(mem: &mut Region, context_64: bool) -> Result<InputContext<'_>, Error> {
    let stride = if context_64 { 16 } else { 8 };
    InputContext::new(mem.dwords(INPUT, INPUT_CONTEXTS * stride), context_64).map_err(Error::Xhci)
}

fn ep0_context(max_packet: u16, dequeue: u64) -> EndpointContext {
    EndpointContext {
        kind: EndpointType::Control,
        max_packet,
        interval: 0,
        dequeue,
        average_trb: 8,
    }
}

/// Lower-case hex of `bytes`, for the descriptor evidence lines.
fn hex(bytes: &[u8]) -> alloc::string::String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
