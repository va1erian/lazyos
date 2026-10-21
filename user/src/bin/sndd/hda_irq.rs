//! The HDA card's interrupt side: waiting for and acknowledging the
//! controller's interrupt. A child module of `hda_card` so it can reach the
//! card's private fields.

use hda::regs::{Regs, RIRBSTS};
use user::sys;
use user::trace;

use super::HdaCard;

impl HdaCard {
    pub(crate) fn wait_event(&mut self) {
        let Some(irq) = self.claimed.irq else {
            return sys::nap();
        };
        if let Ok(message) = irq.recv_with(&mut self.irq_buf, Some(sys::clock() + 1)) {
            self.handle_irq(&message);
        }
    }

    pub(crate) fn service_irq(&mut self) {
        let Some(irq) = self.claimed.irq else {
            return;
        };
        if let Ok(Some(message)) = irq.poll_recv_with(&mut self.irq_buf) {
            self.handle_irq(&message);
        }
    }

    /// Acknowledge one interrupt: only the kernel (slot 0) may send one.
    /// Clearing the stream's status (and the response ring's) deasserts the
    /// line before the kernel is told to unmask it.
    fn handle_irq(&mut self, message: &user::messenger::Message) {
        if message.sender != 0 || user::dev::parse_irq_body(&message.parcel.body).is_none() {
            trace::line!(trace::Channel::Sndd, "IRQ:REJECT sender={}", message.sender);
            return;
        }
        self.irqs += 1;
        let regs = self.controller.regs_mut();
        let _ = self.stream.take_status(regs);
        regs.write8(RIRBSTS, 0xFF);
        let _ = user::dev::irq_ack(self.claimed.handle);
    }

    pub(crate) fn irq_report(&self) -> (bool, u64) {
        (self.claimed.irq.is_some(), self.irqs)
    }
}
