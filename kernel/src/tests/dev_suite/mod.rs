//! Device core, interrupt delivery and the device syscall (issues #239, #240).
//!
//! One submodule per concern, each with its own `CASES` table, listed in
//! [`SUITE`](super::SUITE) in this order: the core (enumeration, table, drivers)
//! first, then interrupts, the syscall and teardown, and the stress tests last
//! because they are the slowest. Every test that touches shared global state
//! (device table, IRQ state, ACL, quotas, audit) builds a
//! [`fixture::Fixture`], which restores it on every exit path.

use super::*;

mod capacity;
mod class_map;
mod fixture;
mod irq;
mod irq_edge;
mod irq_real;
mod irq_shared;
mod stress;
mod stress_dma;
mod sys_boot_policy;
mod sys_cfg;
mod sys_claim;
mod sys_dma;
mod sys_dma_life;
mod sys_guard;
mod sys_inspect;
mod sys_ops;
mod sys_policy;
mod sys_teardown;
mod sys_usb_policy;
mod table_core;

pub(super) const CORE: &[(&str, Test)] = table_core::CASES;
pub(super) const CAPACITY: &[(&str, Test)] = capacity::CASES;
pub(super) const CLASS_MAP: &[(&str, Test)] = class_map::CASES;
pub(super) const IRQ: &[(&str, Test)] = irq::CASES;
pub(super) const IRQ_EDGE: &[(&str, Test)] = irq_edge::CASES;
pub(super) const IRQ_REAL: &[(&str, Test)] = irq_real::CASES;
pub(super) const IRQ_SHARED: &[(&str, Test)] = irq_shared::CASES;
pub(super) const SYSCALL: &[(&str, Test)] = sys_claim::CASES;
pub(super) const SYSCALL_CFG: &[(&str, Test)] = sys_cfg::CASES;
pub(super) const SYSCALL_GUARD: &[(&str, Test)] = sys_guard::CASES;
pub(super) const SYSCALL_OPS: &[(&str, Test)] = sys_ops::CASES;
pub(super) const SYSCALL_POLICY: &[(&str, Test)] = sys_policy::CASES;
pub(super) const SYSCALL_BOOT_POLICY: &[(&str, Test)] = sys_boot_policy::CASES;
pub(super) const SYSCALL_INSPECT: &[(&str, Test)] = sys_inspect::CASES;
pub(super) const SYSCALL_USB_POLICY: &[(&str, Test)] = sys_usb_policy::CASES;
pub(super) const TEARDOWN: &[(&str, Test)] = sys_teardown::CASES;
pub(super) const DMA: &[(&str, Test)] = sys_dma::CASES;
pub(super) const DMA_LIFE: &[(&str, Test)] = sys_dma_life::CASES;
pub(super) const DMA_STRESS: &[(&str, Test)] = stress_dma::CASES;
pub(super) const STRESS: &[(&str, Test)] = stress::CASES;
