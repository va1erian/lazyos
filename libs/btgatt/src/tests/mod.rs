use std::vec::Vec;

mod client;
mod discovery;
mod fuzz;
mod hostile;
pub(crate) mod server;

use crate::hogp::{Hogp, HogpEvent, Step};
use server::Server;

/// A 70-octet Report Map: longer than one read at the test server's MTU of
/// 64 (63 octets), so the map takes a Read Blob.
pub fn long_map() -> Vec<u8> {
    (0..70u8).collect()
}

/// Carry a step's PDUs to the server and its answers back until the driver
/// has nothing more to send; returns every event.
pub fn run(hogp: &mut Hogp, server: &mut Server, first: Step) -> Vec<HogpEvent> {
    let mut events = first.events;
    let mut queue = first.send;
    let mut guard = 0;
    while !queue.is_empty() {
        guard += 1;
        assert!(guard < 1000, "the conversation does not end");
        let pdu = queue.remove(0);
        if let Some(answer) = server.handle(&pdu) {
            let step = hogp.on_pdu(guard, &answer);
            queue.extend(step.send);
            events.extend(step.events);
        }
    }
    events
}
