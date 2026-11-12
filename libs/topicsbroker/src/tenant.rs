//! Who a request is charged to, and who may touch a subscription.
//!
//! Two identities travel with every request, and they must not be mixed up:
//!
//! * the **slot** is the sender's task slot. It names one thread of one
//!   process, is reused after the task exits, and so is good for exactly one
//!   thing: addressing the subscription a task created (who may pull, ack,
//!   unsubscribe or set the bell);
//! * the **tenant** is whom the budgets are charged to: every thread and
//!   process of one app in one login session is one tenant, so spawning
//!   threads buys no extra allowance, and a task that inherits a dead task's
//!   slot but is another app is another tenant, with a fresh budget of its
//!   own and none of its predecessor's charges.
//!
//! Both come from the kernel's stamp on the message, never from its body.

/// Whom a byte, a subscription or a retained value is charged to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Tenant {
    /// A platform task (no label, no login session: the services `init`
    /// starts before login, the rule the kernel uses for the `WaitFor`
    /// reserve). Trusted, so each task slot keeps an allowance of its own: a
    /// chatty service cannot crowd out another, and a reused slot only ever
    /// inherits charges from a platform task.
    Platform(u64),
    /// An app (or any other labelled or logged-in task): all its threads and
    /// processes in one session share one budget.
    App { label: u32, uid: u32, session: u64 },
}

impl Tenant {
    /// The tenant the kernel's stamp on a message names.
    pub fn from_stamp(label_id: u32, uid: u32, session: u64, slot: u64) -> Tenant {
        if label_id == 0 && session == 0 {
            Tenant::Platform(slot)
        } else {
            Tenant::App {
                label: label_id,
                uid,
                session,
            }
        }
    }

    /// Whether the tenant is a platform task.
    pub fn is_platform(&self) -> bool {
        matches!(self, Tenant::Platform(_))
    }
}

/// The sender of a broker request: its slot (authorization) and its tenant
/// (accounting). A subscription answers to the client that created it, so a
/// task that merely reuses the slot of an exited one, but is another tenant,
/// cannot pull, ack, unsubscribe or ring it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Client {
    pub slot: u64,
    pub tenant: Tenant,
}

impl Client {
    pub fn new(slot: u64, tenant: Tenant) -> Client {
        Client { slot, tenant }
    }

    /// The client a kernel stamp names.
    pub fn from_stamp(label_id: u32, uid: u32, session: u64, slot: u64) -> Client {
        Client::new(slot, Tenant::from_stamp(label_id, uid, session, slot))
    }
}
