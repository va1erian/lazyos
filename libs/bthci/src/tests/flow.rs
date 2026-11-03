//! Credit accounting for commands and ACL buffers.

use crate::command::Command;
use crate::event::Event;
use crate::flow::{AclBuffers, AclError, CommandQueue, MAX_CREDITS, MAX_QUEUED_COMMANDS};
use crate::opcode as op;

fn complete(num: u8, opcode: u16) -> Event {
    Event::CommandComplete {
        num_commands: num,
        opcode,
        return_params: alloc::vec![0],
    }
}

#[test]
fn one_credit_at_start_and_commands_go_in_order() {
    let mut q = CommandQueue::new();
    q.push(&Command::Reset).unwrap();
    q.push(&Command::ReadBdAddr).unwrap();
    assert_eq!(q.pop_ready(0).unwrap(), Command::Reset.encode());
    assert_eq!(q.credits(), 0);
    assert!(
        q.pop_ready(1).is_none(),
        "no credit until the controller answers"
    );
    q.on_event(&complete(1, op::RESET));
    assert_eq!(q.in_flight(), 0);
    assert_eq!(q.pop_ready(2).unwrap(), Command::ReadBdAddr.encode());
}

#[test]
fn status_events_grant_credit_too() {
    let mut q = CommandQueue::new();
    q.push(&Command::Reset).unwrap();
    q.pop_ready(0).unwrap();
    q.on_event(&Event::CommandStatus {
        status: 0,
        num_commands: 1,
        opcode: op::CREATE_CONNECTION,
    });
    assert_eq!(q.credits(), 1);
}

#[test]
fn a_larger_grant_allows_pipelining_and_is_clamped() {
    let mut q = CommandQueue::new();
    q.on_event(&complete(255, 0));
    assert_eq!(q.credits(), MAX_CREDITS);
    for _ in 0..20 {
        q.push(&Command::ReadBdAddr).unwrap();
    }
    let mut sent = 0;
    while q.pop_ready(0).is_some() {
        sent += 1;
    }
    assert_eq!(sent, usize::from(MAX_CREDITS));
}

#[test]
fn a_zero_opcode_event_only_grants_credit() {
    let mut q = CommandQueue::new();
    q.push(&Command::Reset).unwrap();
    q.pop_ready(0).unwrap();
    q.on_event(&complete(1, 0));
    assert_eq!(q.in_flight(), 1, "the reset is still unanswered");
    assert_eq!(q.credits(), 1);
}

#[test]
fn queue_is_bounded_and_times_out() {
    let mut q = CommandQueue::new();
    for _ in 0..MAX_QUEUED_COMMANDS {
        q.push(&Command::Reset).unwrap();
    }
    assert!(q.push(&Command::Reset).is_err());
    q.pop_ready(100).unwrap();
    assert!(!q.timed_out(4_000));
    assert!(q.timed_out(5_100));
    q.reset();
    assert_eq!((q.queued(), q.credits(), q.in_flight()), (0, 1, 0));
}

#[test]
fn an_unsolicited_completion_cannot_underflow_in_flight() {
    let mut q = CommandQueue::new();
    for _ in 0..5 {
        q.on_event(&complete(1, op::RESET));
    }
    assert_eq!(q.in_flight(), 0);
}

#[test]
fn acl_buffers_are_consumed_and_returned() {
    let mut b = AclBuffers::new(27, 3);
    b.acquire(0x40).unwrap();
    b.acquire(0x40).unwrap();
    b.acquire(0x41).unwrap();
    assert_eq!(b.free(), 0);
    assert_eq!(b.acquire(0x40), Err(AclError::NoBuffer));
    assert_eq!(b.in_flight(0x40), 2);
    b.complete(0x40, 1).unwrap();
    assert_eq!((b.free(), b.in_flight(0x40)), (1, 1));
    b.acquire(0x41).unwrap();
}

#[test]
fn a_closed_link_gives_its_buffers_back() {
    let mut b = AclBuffers::new(27, 4);
    b.acquire(0x40).unwrap();
    b.acquire(0x40).unwrap();
    b.acquire(0x41).unwrap();
    b.link_closed(0x40);
    assert_eq!((b.free(), b.in_flight(0x40), b.in_flight(0x41)), (3, 0, 1));
    // A late completion for the dead handle is an overcount, not a credit.
    assert_eq!(b.complete(0x40, 2), Err(AclError::Overcount));
    assert_eq!(b.free(), 3);
}

#[test]
fn overcounting_controllers_cannot_inflate_the_pool() {
    let mut b = AclBuffers::new(27, 2);
    b.acquire(0x40).unwrap();
    assert_eq!(b.complete(0x40, 50), Err(AclError::Overcount));
    assert_eq!(b.free(), 2, "never above the controller's total");
    let ev = Event::NumberOfCompletedPackets(alloc::vec![(0x40, 5), (0x99, 1)]);
    assert_eq!(b.on_completed(&ev), 2);
    assert_eq!(b.free(), 2);
}

#[test]
fn link_table_is_bounded() {
    let mut b = AclBuffers::new(27, 100);
    for h in 0..crate::flow::MAX_ACL_LINKS as u16 {
        b.acquire(h).unwrap();
    }
    assert_eq!(b.acquire(0x200), Err(AclError::TooManyLinks));
    assert_eq!(b.free(), 100 - crate::flow::MAX_ACL_LINKS as u16);
}

#[test]
fn the_counters_balance_over_a_long_run() {
    let mut b = AclBuffers::new(27, 5);
    for i in 0..100_000u32 {
        let h = (i % 3) as u16;
        if b.acquire(h).is_err() {
            for h in 0..3 {
                let n = b.in_flight(h);
                b.complete(h, n).unwrap();
            }
        }
    }
    for h in 0..3 {
        b.link_closed(h);
    }
    assert_eq!(b.free(), 5);
}
