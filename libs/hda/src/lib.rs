//! The logic of the Intel High Definition Audio driver, as host-testable
//! `no_std` code (issue #497, driver-plan stage D7).
//!
//! The second sound card behind `os.lazy.audio.v1`, and the audio half of the
//! proof that the device core is not virtio-shaped: a PCI controller with one
//! memory BAR, command rings and a buffer descriptor list in DMA memory, and
//! codecs on a serial link that are discovered, not assumed. `sndd` drives it
//! with the same `dev_*` ops as virtio-sound and no new one. This crate is
//! what can be tested without the machine:
//!
//! * [`regs`]: the controller registers and the [`Regs`] accessor;
//! * [`controller`]: link reset, codec discovery, the CORB/RIRB transport;
//! * [`verbs`]: codec verbs and parameters;
//! * [`codec`]: the widget graph and the output path through it;
//! * [`program`]: switching the path on and binding the converter;
//! * [`format`]: stream format words and supported rates;
//! * [`stream`]: an output stream descriptor and its buffer descriptor list;
//! * [`cursor`]: completed periods from the cyclic link position.
//!
//! Real machines put more on the codec (jack sensing, vendor verbs, a DSP mode
//! on recent Intel platforms); the walk here is the generic one every
//! specification-conforming codec answers, which is what QEMU's `intel-hda`
//! and most boards' analog outputs need.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod codec;
pub mod controller;
pub mod cursor;
pub mod format;
pub mod program;
pub mod regs;
pub mod stream;
pub mod verbs;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_codec;

pub use controller::{Controller, ControllerError, RingMemory};
pub use regs::{Mmio, Regs};

/// PCI class and subclass of an HDA controller (multimedia, audio device).
pub const PCI_CLASS: u8 = 0x04;
pub const PCI_SUBCLASS: u8 = 0x03;

/// Whether a PCI function is an HDA controller (any vendor: Intel, AMD and
/// others implement the same register interface under this class).
pub fn is_controller(class: u8, subclass: u8) -> bool {
    class == PCI_CLASS && subclass == PCI_SUBCLASS
}
