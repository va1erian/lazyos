//! The malicious-client probe routine (`xui_webd_client::attack`) against the service over the fake
//! web. The kernel's scope gate is not here (the in-memory fabric delivers any interface id, so the
//! `net` and `browser` scopes are *allowed* through it) — that gate is proven only in QEMU
//! (`tools/webd/attack_run.py`). This checks the resource and flood probes the fabric does reproduce,
//! and that the routine never panics and leaves the service clean.

mod support;

use support::full::web;
use support::TestTransport;
use xui_webd_client::attack::{self, VIEWS_PER_CLIENT};

#[test]
fn the_resource_and_flood_probes_behave_against_the_service() {
    let world = web();
    // Every view the routine makes is this one client (label 7), so the per-client view cap applies.
    let probes = attack::run(|| TestTransport::full(&world.world, 7));

    // The view scope is allowed (through the fabric the net/browser scopes are too: not asserted here).
    assert_eq!(attack::outcome_of(&probes, "view_ok"), Some("OK"));
    // The flood of 1000 oneway mouse moves does not stop webd answering the next call.
    assert_eq!(attack::outcome_of(&probes, "flood_alive"), Some("OK"));
    // The per-client cap is reached at VIEWS_PER_CLIENT live views and the next is ENOSPC (28).
    assert_eq!(
        attack::outcome_of(&probes, "view_cap"),
        Some(format!("REFUSED:28:{VIEWS_PER_CLIENT}").as_str())
    );
    // webd still serves after the flood and the cap.
    assert_eq!(attack::outcome_of(&probes, "alive_after"), Some("OK"));
}
