//! The mass-storage class: sticks served to the kernel as block devices
//! (syscall 33, docs/architecture/usb-storage.md).
//!
//! `class.rs` hands this a SCSI Bulk-Only interface ([`Msc::bind`]): its two
//! bulk pipes are opened and configured with the device's other endpoints.
//! [`Msc::start`] then brings the disk up through `libs/usbmsc` (INQUIRY,
//! TEST UNIT READY, capacity, write protection) and, for 512-byte blocks,
//! registers it with the kernel (`usb<n>`). From then on the driver's loop
//! takes the kernel's requests (`NEXT`), runs them on the stick and answers
//! (`COMPLETE`): [`Msc::serve`]. Writes are flushed (SYNCHRONIZE CACHE) when
//! the kernel flushes and, on their own, [`FLUSH_IDLE_TICKS`] after the last
//! one, so a stick pulled out without a shutdown keeps all but its last
//! second. An unplugged stick is reported (`REMOVE`, [`Msc::close`]): the
//! kernel fails its I/O and the mount degrades; nothing waits for it.
//!
//! Serial evidence: `USBD:MSC:DISK` (registered), `USBD:MSC:SKIP` (a
//! mass-storage interface this driver does not serve), `USBD:MSC:FAIL`,
//! `USBD:MSC:GONE`, and for real-hardware diagnosis (issue #704)
//! `USBD:MSC:REQ` (a request that failed or took over 2 s, and with
//! `trace=1` every request: its op, range, time, and the stalls and resets
//! the transport went through) after the `USBD:MSC:XFER` lines of its
//! failed transfers (`msc_link.rs`).

use alloc::format;
use alloc::vec::Vec;

use usbhid::desc::{Interface, Transfer};
use usbmsc::bot::{reset_recovery, XferError};
use usbmsc::desc::{PROTOCOL_BOT, SUBCLASS_SCSI};
use usbmsc::disk::{Disk, DiskError};
use user::sys::{self, storage_op, storage_status, StorageRequest};

use super::device::Device;
use super::hc::{Hc, BULK_WINDOW};
use super::msc_link::{Link, Patience, Pipes};
use super::Error;

/// The only block size served (the kernel's sector).
const BLOCK: usize = 512;
/// Flush a stick this long (100 Hz) after its last write.
const FLUSH_IDLE_TICKS: u64 = 100;
/// A request that took this long (or failed) is reported (`USBD:MSC:REQ`);
/// `trace=1` reports every one.
const SLOW_MS: u64 = 2000;

/// One Bulk-Only interface and, once started, the kernel's disk on it.
pub(super) struct Msc {
    pipes: Pipes,
    disk: Option<Disk>,
    /// The kernel's disk id (`usb<id>`), once registered.
    id: u64,
    /// The request's data, as the kernel hands it over.
    buffer: Vec<u8>,
    /// When the latest unflushed write landed.
    dirty_since: Option<u64>,
    /// The kernel's disk is dead (gone or failed): nothing more to serve.
    dead: bool,
    /// A request ran out of time before the stick's Bulk-Only reset
    /// recovery finished: run it before the next request.
    reset_pending: bool,
}

impl Msc {
    /// Open the pipes of a SCSI Bulk-Only interface; `None` for any other
    /// mass-storage interface (reported) or one without both bulk endpoints.
    pub(super) fn bind(device: &mut Device, interface: &Interface) -> Result<Option<Msc>, Error> {
        let bulk = (
            interface.endpoint(Transfer::Bulk, true),
            interface.endpoint(Transfer::Bulk, false),
        );
        let (Some(bulk_in), Some(bulk_out)) = bulk else {
            return Ok(None);
        };
        if interface.subclass != SUBCLASS_SCSI || interface.protocol != PROTOCOL_BOT {
            sys::write_str(&format!(
                "USBD:MSC:SKIP port={} interface={} subclass={:#04x} protocol={:#04x} (not SCSI Bulk-Only)\n",
                device.name, interface.number, interface.subclass, interface.protocol
            ));
            return Ok(None);
        }
        let pipes = Pipes {
            interface: interface.number,
            bulk_in: device.open_pipe(&bulk_in)?,
            bulk_out: device.open_pipe(&bulk_out)?,
            in_address: bulk_in.address,
            out_address: bulk_out.address,
        };
        Ok(Some(Msc {
            pipes,
            disk: None,
            id: 0,
            buffer: Vec::new(),
            dirty_since: None,
            dead: false,
            reset_pending: false,
        }))
    }

    /// Whether endpoint `dci` is one of this interface's.
    pub(super) fn owns(&self, dci: u8) -> bool {
        self.pipes.bulk_in.dci == dci || self.pipes.bulk_out.dci == dci
    }

    fn link<'a>(
        &'a mut self,
        hc: &'a mut Hc,
        device: &'a mut Device,
        patience: Patience,
    ) -> (Link<'a>, &'a mut Option<Disk>) {
        (
            Link {
                hc,
                device,
                pipes: &mut self.pipes,
                patience,
            },
            &mut self.disk,
        )
    }

    /// After Configure Endpoint: bring the disk up and register it. `false`
    /// when it is not served (reported); the caller drops it.
    pub(super) fn start(&mut self, hc: &mut Hc, device: &mut Device) -> bool {
        let name = device.name.clone();
        let (vendor, product) = (device.descriptor.vendor, device.descriptor.product);
        let slot = device.slot;
        let (mut link, _) = self.link(hc, device, Patience::bring_up());
        let disk = match Disk::bring_up(&mut link, 0) {
            Ok(disk) if disk.block_len() == BLOCK => disk,
            Ok(disk) => return skip(&name, &format!("{}-byte blocks", disk.block_len())),
            Err(error) => return skip(&name, &format!("bring-up: {error:?}")),
        };
        let blocks = disk.capacity.blocks;
        self.id = match sys::storage_register(blocks, !disk.write_protected) {
            Ok(id) => id,
            Err(errno) => return skip(&name, &format!("register errno {errno}")),
        };
        sys::write_str(&format!(
            "USBD:MSC:DISK port={name} slot={slot} id=usb{} vendor={vendor:#06x} product={product:#06x} blocks={blocks} wp={} burst={} packet={}\n",
            self.id,
            u8::from(disk.write_protected),
            self.pipes.bulk_in.context.max_burst,
            self.pipes.bulk_in.context.max_packet,
        ));
        self.disk = Some(disk);
        self.buffer = alloc::vec![0u8; BULK_WINDOW];
        true
    }

    /// Whether the kernel still has requests for this disk to serve.
    pub(super) fn live(&self) -> bool {
        self.disk.is_some() && !self.dead
    }

    /// Take and run one request, waiting for it until `deadline` (0: not at
    /// all); then flush if the stick sat written-to and idle. `trace`
    /// reports every request (`USBD:MSC:REQ`), not only the failed and slow.
    /// Returns whether a request was served.
    pub(super) fn serve(
        &mut self,
        hc: &mut Hc,
        device: &mut Device,
        deadline: u64,
        trace: bool,
    ) -> bool {
        if !self.live() {
            return false;
        }
        let served = match sys::storage_next(self.id, &mut self.buffer, deadline) {
            Ok(Some(request)) => {
                self.run(hc, device, request, trace);
                true
            }
            Ok(None) => false,
            Err(errno) => {
                // The kernel no longer serves this disk; its own log says
                // why (`block: usb<n>: ... timed out`).
                sys::write_str(&format!(
                    "USBD:MSC:FAIL id=usb{} next errno {errno} ({})\n",
                    self.id,
                    next_errno_text(errno)
                ));
                self.dead = true;
                false
            }
        };
        self.flush_idle(hc, device);
        served
    }

    fn run(&mut self, hc: &mut Hc, device: &mut Device, request: StorageRequest, trace: bool) {
        let bytes = request.bytes as usize;
        let id = self.id;
        let Msc {
            pipes,
            disk,
            buffer,
            reset_pending,
            ..
        } = self;
        let Some(disk) = disk.as_mut() else {
            return;
        };
        let mut link = Link {
            hc,
            device,
            pipes,
            patience: Patience::request(),
        };
        let started = sys::monotonic_ms();
        let before = disk.bot.stats;
        let result = recover_pending(&mut link, reset_pending).and_then(|()| {
            match (request.op, buffer.get_mut(..bytes)) {
                (_, None) => Err(DiskError::Range),
                (storage_op::READ, Some(data)) => disk.read(&mut link, request.lba, data),
                (storage_op::WRITE, Some(data)) => disk.write(&mut link, request.lba, data),
                (storage_op::FLUSH, _) => disk.flush(&mut link),
                _ => Err(DiskError::Unsupported),
            }
        });
        let result = out_of_time(result, &link, reset_pending);
        let after = disk.bot.stats;
        let ms = sys::monotonic_ms().saturating_sub(started);
        if trace || result.is_err() || ms >= SLOW_MS {
            sys::write_str(&format!(
                "USBD:MSC:REQ id=usb{id} op={} lba={} bytes={bytes} ms={ms} result={result:?} stalls={} resets={}\n",
                op_name(request.op),
                request.lba,
                after.stalls - before.stalls,
                after.resets - before.resets,
            ));
        }
        let status = match result {
            Ok(()) => storage_status::OK,
            Err(DiskError::WriteProtected) => storage_status::READ_ONLY,
            Err(DiskError::Gone | DiskError::Dead | DiskError::NoMedium) => storage_status::GONE,
            Err(_) => storage_status::IO,
        };
        match (status, request.op) {
            (storage_status::OK, storage_op::WRITE) => self.dirty_since = Some(sys::clock()),
            (storage_status::OK, storage_op::FLUSH) => self.dirty_since = None,
            _ => {}
        }
        let data = match (status, request.op) {
            (storage_status::OK, storage_op::READ) => &self.buffer[..bytes],
            _ => &[][..],
        };
        if status == storage_status::GONE {
            self.dead = true;
            sys::write_str(&format!("USBD:MSC:GONE id=usb{id} ({result:?})\n"));
        }
        if let Err(errno) = sys::storage_complete(id, request.tag, status, data) {
            sys::write_str(&format!(
                "USBD:MSC:FAIL id=usb{id} complete errno {errno}\n"
            ));
        }
    }

    /// SYNCHRONIZE CACHE on a stick written to and then left alone.
    fn flush_idle(&mut self, hc: &mut Hc, device: &mut Device) {
        let idle = self
            .dirty_since
            .is_some_and(|since| sys::clock() >= since + FLUSH_IDLE_TICKS);
        if !idle || !self.live() {
            return;
        }
        self.dirty_since = None;
        let id = self.id;
        let mut pending = self.reset_pending;
        let (mut link, disk) = self.link(hc, device, Patience::idle_flush());
        let result = match disk.as_mut() {
            Some(disk) => {
                recover_pending(&mut link, &mut pending).and_then(|()| disk.flush(&mut link))
            }
            None => Ok(()),
        };
        if let Err(error) = out_of_time(result, &link, &mut pending) {
            sys::write_str(&format!("USBD:MSC:FAIL id=usb{id} idle flush {error:?}\n"));
        }
        self.reset_pending = pending;
    }

    /// The stick went away (or its device is being given up on): the
    /// kernel's disk dies, its I/O fails from now on.
    pub(super) fn close(self) {
        if self.disk.is_none() {
            return;
        }
        if !self.dead {
            let _ = sys::storage_remove(self.id);
        }
        sys::write_str(&format!("USBD:MSC:GONE id=usb{} (detached)\n", self.id));
    }
}

/// Run the Bulk-Only reset recovery an earlier operation ran out of time
/// for. If it fails with time left the stick is unusable (`Dead`).
fn recover_pending(link: &mut Link<'_>, pending: &mut bool) -> Result<(), DiskError> {
    if !*pending {
        return Ok(());
    }
    match reset_recovery(link) {
        Ok(()) => {
            *pending = false;
            Ok(())
        }
        Err(XferError::Gone) => Err(DiskError::Gone),
        Err(_) if link.expired() => Err(DiskError::Io),
        Err(_) => Err(DiskError::Dead),
    }
}

/// An operation whose recovery was cut short by its time limit is an I/O
/// error, not a dead stick: the recovery runs before the next request.
fn out_of_time(
    result: Result<(), DiskError>,
    link: &Link<'_>,
    pending: &mut bool,
) -> Result<(), DiskError> {
    match result {
        Err(DiskError::Dead) if link.expired() => {
            *pending = true;
            Err(DiskError::Io)
        }
        other => other,
    }
}

/// What the kernel's refusal of `NEXT` means (`block/provider/sys.rs`).
fn next_errno_text(errno: i64) -> &'static str {
    match errno {
        3 => "the kernel declared the disk dead",
        14 => "bad buffer",
        22 => "bad request",
        _ => "unexpected",
    }
}

fn op_name(op: u64) -> &'static str {
    match op {
        storage_op::READ => "read",
        storage_op::WRITE => "write",
        storage_op::FLUSH => "flush",
        _ => "?",
    }
}

fn skip(port: &str, why: &str) -> bool {
    sys::write_str(&format!("USBD:MSC:SKIP port={port} {why}\n"));
    false
}
