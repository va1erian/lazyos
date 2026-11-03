//! Flow control toward the controller: command credits and ACL buffers.
//!
//! * The controller grants **command credits** in every Command_Complete and
//!   Command_Status (`Num_HCI_Command_Packets`); the host may have that many
//!   commands outstanding. [`CommandQueue`] holds commands until a credit is
//!   available, in order.
//! * The controller has a fixed number of **ACL buffers** (Read_Buffer_Size);
//!   each ACL packet sent uses one until Number_Of_Completed_Packets returns
//!   it. [`AclBuffers`] counts them per connection so a link that goes away
//!   gives its buffers back (the controller does not report those).
//!
//! Both are bounded: a controller that grants absurd credit, or reports more
//! completions than were sent, cannot make the host send faster than its own
//! queues allow.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::command::Command;
use crate::event::Event;

/// Commands waiting for a credit. The host's own bring-up and pairing keep
/// far fewer; more means a bug or a stuck controller.
pub const MAX_QUEUED_COMMANDS: usize = 32;
/// A credit grant above this is clamped: no real controller allows more than
/// a handful of commands in flight.
pub const MAX_CREDITS: u8 = 8;
/// A command with no completion for this long means a wedged controller.
pub const COMMAND_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, PartialEq, Eq)]
pub struct QueueFull;

/// Sends commands in order, within the controller's credits.
#[derive(Debug)]
pub struct CommandQueue {
    credits: u8,
    in_flight: u8,
    last_sent_ms: u64,
    queue: VecDeque<Vec<u8>>,
}

impl Default for CommandQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandQueue {
    /// A fresh queue assumes one credit, as the specification does until the
    /// controller says otherwise.
    pub fn new() -> CommandQueue {
        CommandQueue {
            credits: 1,
            in_flight: 0,
            last_sent_ms: 0,
            queue: VecDeque::new(),
        }
    }

    pub fn push(&mut self, cmd: &Command) -> Result<(), QueueFull> {
        if self.queue.len() >= MAX_QUEUED_COMMANDS {
            return Err(QueueFull);
        }
        self.queue.push_back(cmd.encode());
        Ok(())
    }

    /// The next command packet if a credit is available; takes the credit.
    pub fn pop_ready(&mut self, now_ms: u64) -> Option<Vec<u8>> {
        if self.credits == 0 {
            return None;
        }
        let packet = self.queue.pop_front()?;
        self.credits -= 1;
        self.in_flight = self.in_flight.saturating_add(1);
        self.last_sent_ms = now_ms;
        Some(packet)
    }

    /// Account for a Command_Complete or Command_Status. The grant replaces
    /// the credit count (it is the controller's absolute figure), clamped.
    /// Other events are ignored. A completion for opcode 0 only grants credit.
    pub fn on_event(&mut self, ev: &Event) {
        let (num, opcode) = match ev {
            Event::CommandComplete {
                num_commands,
                opcode,
                ..
            }
            | Event::CommandStatus {
                num_commands,
                opcode,
                ..
            } => (*num_commands, *opcode),
            _ => return,
        };
        self.credits = num.min(MAX_CREDITS);
        if opcode != 0 {
            self.in_flight = self.in_flight.saturating_sub(1);
        }
    }

    pub fn credits(&self) -> u8 {
        self.credits
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn in_flight(&self) -> u8 {
        self.in_flight
    }

    /// True when a sent command has gone unanswered for [`COMMAND_TIMEOUT_MS`].
    pub fn timed_out(&self, now_ms: u64) -> bool {
        self.in_flight > 0 && now_ms.saturating_sub(self.last_sent_ms) >= COMMAND_TIMEOUT_MS
    }

    /// Forget everything after a controller Reset: queued commands are void
    /// and the credit count restarts at one.
    pub fn reset(&mut self) {
        *self = CommandQueue::new();
    }
}

/// Connections the host tracks ACL buffers for.
pub const MAX_ACL_LINKS: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum AclError {
    /// No free controller buffer: wait for Number_Of_Completed_Packets.
    NoBuffer,
    /// The handle is not tracked and the table is full.
    TooManyLinks,
    /// The controller reported more completions than packets in flight.
    Overcount,
}

/// The controller's ACL buffer pool as the host sees it.
#[derive(Debug, Clone)]
pub struct AclBuffers {
    max_len: u16,
    total: u16,
    free: u16,
    in_flight: Vec<(u16, u16)>,
}

impl AclBuffers {
    /// From Read_Buffer_Size: payload size and number of buffers.
    pub fn new(max_len: u16, total: u16) -> AclBuffers {
        AclBuffers {
            max_len,
            total,
            free: total,
            in_flight: Vec::new(),
        }
    }

    /// Largest payload per ACL packet; fragment larger PDUs to this.
    pub fn max_len(&self) -> u16 {
        self.max_len
    }

    pub fn free(&self) -> u16 {
        self.free
    }

    pub fn in_flight(&self, handle: u16) -> u16 {
        self.in_flight
            .iter()
            .find(|(h, _)| *h == handle)
            .map_or(0, |(_, n)| *n)
    }

    /// Take one buffer for a packet on `handle`.
    pub fn acquire(&mut self, handle: u16) -> Result<(), AclError> {
        if self.free == 0 {
            return Err(AclError::NoBuffer);
        }
        if let Some((_, n)) = self.in_flight.iter_mut().find(|(h, _)| *h == handle) {
            *n += 1;
        } else if self.in_flight.len() >= MAX_ACL_LINKS {
            return Err(AclError::TooManyLinks);
        } else {
            self.in_flight.push((handle, 1));
        }
        self.free -= 1;
        Ok(())
    }

    /// Number_Of_Completed_Packets for one handle. A count above what is in
    /// flight returns only what was in flight and reports the excess.
    pub fn complete(&mut self, handle: u16, count: u16) -> Result<(), AclError> {
        let Some(slot) = self.in_flight.iter_mut().find(|(h, _)| *h == handle) else {
            return if count == 0 {
                Ok(())
            } else {
                Err(AclError::Overcount)
            };
        };
        let returned = slot.1.min(count);
        slot.1 -= returned;
        self.free = (self.free + returned).min(self.total);
        if slot.1 == 0 {
            self.in_flight.retain(|(h, _)| *h != handle);
        }
        if count > returned {
            Err(AclError::Overcount)
        } else {
            Ok(())
        }
    }

    /// A connection ended: its in-flight packets are gone and their buffers
    /// are free again.
    pub fn link_closed(&mut self, handle: u16) {
        let n = self.in_flight(handle);
        self.in_flight.retain(|(h, _)| *h != handle);
        self.free = (self.free + n).min(self.total);
    }

    /// Apply a whole event; returns the number of overcounted entries.
    pub fn on_completed(&mut self, ev: &Event) -> usize {
        let Event::NumberOfCompletedPackets(entries) = ev else {
            return 0;
        };
        entries
            .iter()
            .filter(|(h, c)| self.complete(*h, *c).is_err())
            .count()
    }
}
