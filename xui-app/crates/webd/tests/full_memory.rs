//! The user-memory gate (docs/webd-plan.md "P5 as built", 11 item 7): a view that would start a web
//! process (about 107 MiB) is refused with `EDQUOT` once the user's memory charge cannot hold one more,
//! rather than letting the child's `exec` fail with `ENOMEM` (as the old in-process browser did).

#![cfg(not(feature = "host"))]

mod support;

use support::full::{browser, web, Fx, Via};
use support::origin;
use xui_webd::fabric::errno::*;
use xui_webd::limits::WEB_PROCESS_RESERVE_BYTES as RESERVE;
use xui_webd::recording::Call as EngineCall;

fn creates(fx: &Fx) -> usize {
    fx.engine
        .calls()
        .iter()
        .filter(|call| matches!(call, EngineCall::ViewCreate(..)))
        .count()
}

/// A full view through `os.lazy.webd.net.v1` with the web policy.
fn net_view(fx: &mut Fx, who: u32) -> Result<(u64, u64), i64> {
    fx.create_view(origin(who), Via::Net, web(), 8, 8)
}

#[test]
fn memory_pressure_refuses_a_view_that_would_start_a_new_web_process() {
    let mut fx = Fx::new("mem-pressure");
    let app = origin(7);
    // 8 GiB limit, 200 MiB used: room for a process. The view is made.
    fx.set_memory(200 << 20, 8 << 30);
    let (first, _) = fx
        .create_view(app, Via::Browser, browser(), 8, 8)
        .expect("a view fits when the charge is low");

    // Within one process' reserve of the limit: a view that would start a *new* web process (another
    // profile) is refused, the engine never hears of it, and no handle leaks.
    fx.set_memory((8 << 30) - (100 << 20), 8 << 30);
    let made = creates(&fx);
    let handles = fx.fabric.open_handles();
    assert_eq!(net_view(&mut fx, 7), Err(EDQUOT));
    assert_eq!(creates(&fx), made, "the engine heard of a refused view");
    assert_eq!(fx.fabric.open_handles(), handles, "the endpoint leaked");

    // A view that joins an existing (client, profile) group starts no process: still made.
    let (second, _) = fx
        .create_view(app, Via::Browser, browser(), 8, 8)
        .expect("a view that reuses an existing process is not gated by memory");
    assert_ne!(second, first);

    // When the host cannot read the charge (limit 0) the gate is off: the fixed cap is the only bound.
    fx.set_memory(u64::MAX, 0);
    net_view(&mut fx, 8).expect("no gating when the charge is unknown");
}

#[test]
fn memory_pressure_refuses_a_light_view_that_would_start_its_clients_process() {
    // A client's light views share one web process: the first starts it and is gated like a full view's
    // new process; one more light view of a client that has one live starts nothing.
    let mut fx = Fx::new("mem-light");
    let docs = origin(9);
    fx.set_memory(200 << 20, 8 << 30);
    assert!(fx.rig.create(docs, Default::default(), 8, 8).is_ok());
    fx.set_memory((8 << 30) - (10 << 20), 8 << 30);
    assert!(
        fx.rig.create(docs, Default::default(), 8, 8).is_ok(),
        "a second light view of the same client starts no process"
    );
    let handles = fx.fabric.open_handles();
    let other = fx.rig.create(origin(10), Default::default(), 8, 8);
    assert_eq!(
        other.map(|_| ()),
        Err(EDQUOT),
        "another client's first light view"
    );
    assert_eq!(fx.fabric.open_handles(), handles, "the endpoint leaked");
}

#[test]
fn the_memory_gate_boundary_and_a_charge_over_the_limit() {
    let limit: u64 = 1 << 30;
    // Exactly a reserve of room: the new process fits (the gate refuses only past the limit).
    let mut fx = Fx::new("mem-boundary");
    fx.set_memory(limit - RESERVE, limit);
    net_view(&mut fx, 7).expect("a reserve's worth of room is enough");
    // One byte less: refused.
    let mut fx = Fx::new("mem-boundary-1");
    fx.set_memory(limit - RESERVE + 1, limit);
    assert_eq!(net_view(&mut fx, 7), Err(EDQUOT));
    // A charge already over the limit (memory mapped before can pass a quota): refused, and the sum does
    // not wrap at the top of the range.
    let mut fx = Fx::new("mem-over");
    fx.set_memory(limit + 4096, limit);
    assert_eq!(net_view(&mut fx, 7), Err(EDQUOT));
    fx.set_memory(u64::MAX - 1, limit);
    assert_eq!(net_view(&mut fx, 7), Err(EDQUOT));
}
