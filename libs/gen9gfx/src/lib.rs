//! Intel Gen9 (Skylake to Coffee Lake, the Kaby Lake HD Graphics 620 of the
//! i3-7100U box) display and HDMI audio logic as host-testable `no_std` code
//! (`docs/kabylake-gpu-plan.md`, section 3.2).
//!
//! Nothing here touches hardware: the machine side (`gpud`) will hand the
//! register accessors an [`Mmio`] and these modules do the parsing and the
//! arithmetic that can be settled on a host:
//!
//! * [`edid`]: the base block and its extension list (timings, descriptors);
//! * [`cea`]: the CEA-861 extension (video and audio data blocks, speaker
//!   allocation, HDMI vendor blocks);
//! * [`eld`]: the codec-side "EDID-like data" blob, parsed and built from an
//!   EDID;
//! * [`vbt`]: the Video BIOS Table's header and child device list, enough to
//!   say which DDI port is HDMI, which DDC pin it uses and whether an LSPCON
//!   sits behind it;
//! * [`hdmi_audio`]: N and CTS for the audio clock regeneration packet;
//! * [`regs`]: the few register offsets the plan's first stages need, each
//!   marked as to be confirmed against a register dump from the box.
//!
//! Every parser takes hostile input (a monitor's EDID, firmware's VBT) and
//! must neither panic nor index out of bounds; `fuzz::run` is the byte
//! entry point the seeded tests and the cargo-fuzz target share.

#![no_std]

extern crate alloc;

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod cea;
pub mod edid;
pub mod eld;
pub mod hdmi_audio;
pub mod mmio;
pub mod regs;
pub mod vbt;

#[cfg(any(test, feature = "fuzz"))]
pub mod build;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

#[cfg(test)]
mod tests_edid;
#[cfg(test)]
mod tests_eld;
#[cfg(test)]
mod tests_real;
#[cfg(test)]
mod tests_vbt;

pub use mmio::Mmio;
