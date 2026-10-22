//! A client's own profile and its private windows are different data stores: what the service tells the
//! engine, so the engine can keep them apart (review of 2026-10-22, finding 1).
//!
//! Both are in memory with no folder, in the client's one web process group. The engine joins a new view to
//! a group mate by (group, folder, memory-only, light, network); without one more field the private window
//! was a related page of the client's own view and shared its cookies and storage.
//! (`webd-abi-test store` proves the engine keeps them apart.)

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, web, Fx, Via};
use support::origin;
use xui_webd::engine::EngineConfig;
use xui_webd::fabric::errno::*;
use xui_webd::ident::Origin;
use xui_webd::recording::Call;

fn private() -> wire::ViewConfig {
    wire::ViewConfig {
        ephemeral: true,
        ..browser()
    }
}

/// The engine configuration the service sent for `view`.
fn engine_config(calls: &[Call], view: u64) -> EngineConfig {
    calls
        .iter()
        .find_map(|call| match call {
            Call::ViewCreate(id, config) if *id == view => Some(config.clone()),
            _ => None,
        })
        .expect("the engine was told to create the view")
}

#[test]
fn a_private_window_has_a_store_of_its_own_beside_its_clients_profile() {
    let mut fx = Fx::new("private-store");
    let (app, other) = (origin(7), origin(8));
    let (own, _) = fx.view(app, Via::Browser, web(), 4, 4);
    let (window, _) = fx.view(app, Via::Browser, private(), 4, 4);
    let (window2, _) = fx.view(app, Via::Browser, private(), 4, 4);
    let (persistent, _) = fx.view(app, Via::Browser, browser(), 4, 4);
    let (theirs, _) = fx.view(other, Via::Browser, web(), 4, 4);
    let calls = fx.engine.take();
    let full = |view| engine_config(&calls, view).full.expect("a full view");
    let group = |view| engine_config(&calls, view).group;

    // The client's own profile and its private windows share the client's group...
    assert_eq!(group(own), group(window));
    assert_eq!(group(window), group(window2));
    // ...both are in memory with no folder: nothing else in the configuration tells them apart...
    for view in [own, window, window2] {
        assert!(full(view).memory_only);
        assert_eq!(full(view).profile, "");
    }
    // ...so the store kind must.
    assert!(!full(own).private_store, "the client's own profile");
    assert!(full(window).private_store, "a private window");
    assert!(
        full(window2).private_store,
        "private windows of one client share their private store"
    );
    assert!(
        !full(persistent).private_store,
        "the browser profile is on disk and named by its folder"
    );
    assert!(!full(theirs).private_store);
    assert_ne!(
        group(own),
        group(theirs),
        "another client has its own group"
    );
}

#[test]
fn a_clients_own_profile_and_private_window_count_as_two_web_processes() {
    // Why: the process cap counts what the engine really starts. With the store kind in the engine's
    // match, a client's own view and its private window are two web processes; a second private window
    // joins the first's.
    let mut fx = Fx::new("private-process-count");
    let apps: Vec<Origin> = (1..=4).map(origin).collect();
    fx.view(apps[0], Via::Browser, web(), 4, 4);
    fx.view(apps[0], Via::Browser, private(), 4, 4);
    // A second private window of the client joins its first: no new process.
    fx.view(apps[0], Via::Browser, private(), 4, 4);
    fx.view(apps[1], Via::Net, web(), 4, 4);
    fx.view(apps[2], Via::Net, web(), 4, 4);
    // Four processes now: the client's two, and two more.
    assert_eq!(fx.create_view(apps[3], Via::Net, web(), 4, 4), Err(EDQUOT));
    assert!(fx
        .create_view(apps[0], Via::Browser, private(), 4, 4)
        .is_ok());
}
