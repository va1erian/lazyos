//! The C ABI from the Rust side: `webd_view_config`'s layout against the header, the engine calls
//! reaching the `webd_engine_*` functions, and the `webd_ui_*` callbacks reaching the sink. The
//! `webd_engine_*` functions are stubs here (the C++ half is not linked), which record their calls.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::mem::{align_of, offset_of, size_of};
use std::sync::{Arc, Mutex};

use super::*;
use crate::events::{EventQueue, Once};

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn log(line: String) {
    LOG.lock().unwrap_or_else(|p| p.into_inner()).push(line);
}

fn take_log() -> Vec<String> {
    std::mem::take(&mut *LOG.lock().unwrap_or_else(|p| p.into_inner()))
}

unsafe fn s(text: *const c_char) -> String {
    if text.is_null() {
        "NULL".into()
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

#[no_mangle]
pub extern "C" fn webd_engine_start() -> c_int {
    log("start".into());
    0
}

#[no_mangle]
pub unsafe extern "C" fn webd_engine_view_create(view: u64, config: *const WebdViewConfig) {
    let c = unsafe { &*config };
    log(format!(
        "create {view} size={} light={} scripts={} {}x{} group={} scheme={} font={} style={}",
        c.struct_size,
        c.light,
        c.scripts,
        c.width,
        c.height,
        c.group,
        c.color_scheme,
        c.default_font_size,
        unsafe { s(c.user_style) }
    ));
    if c.light == 0 {
        log(format!(
            "full {view} features={} profile={} memory_only={} deltas={} suffix={} no_network={} store={} agent={}",
            c.features,
            unsafe { s(c.profile) },
            c.memory_only,
            c.deltas,
            unsafe { s(c.user_agent_suffix) },
            c.no_network,
            c.store_kind,
            unsafe { s(c.user_agent) }
        ));
    }
}

#[no_mangle]
pub extern "C" fn webd_engine_view_destroy(view: u64) {
    log(format!("destroy {view}"));
}

#[no_mangle]
pub unsafe extern "C" fn webd_engine_load_html(
    view: u64,
    html: *const u8,
    len: usize,
    base_url: *const c_char,
) {
    let html = unsafe { std::slice::from_raw_parts(html, len) };
    log(format!(
        "html {view} {} base={}",
        String::from_utf8_lossy(html),
        unsafe { s(base_url) }
    ));
}

#[no_mangle]
pub extern "C" fn webd_engine_resize(view: u64, width: c_int, height: c_int) {
    log(format!("resize {view} {width}x{height}"));
}

#[no_mangle]
pub extern "C" fn webd_engine_set_visible(view: u64, visible: c_int) {
    log(format!("visible {view} {visible}"));
}

#[no_mangle]
pub extern "C" fn webd_engine_refresh(view: u64) {
    log(format!("refresh {view}"));
}

#[no_mangle]
pub extern "C" fn webd_engine_mouse(
    view: u64,
    kind: c_int,
    button: c_int,
    x: c_int,
    y: c_int,
    mods: u32,
) {
    log(format!("mouse {view} {kind} {button} {x} {y} {mods}"));
}

#[no_mangle]
pub extern "C" fn webd_engine_wheel(
    view: u64,
    x: c_int,
    y: c_int,
    tx: c_int,
    ty: c_int,
    mods: u32,
    seq: u64,
) {
    log(format!("wheel {view} {x} {y} {tx} {ty} {mods} {seq}"));
}

#[no_mangle]
pub unsafe extern "C" fn webd_engine_key(
    view: u64,
    press: c_int,
    keysym: u32,
    text: *const c_char,
    mods: u32,
) {
    log(format!("key {view} {press} {keysym} {} {mods}", unsafe {
        s(text)
    }));
}

#[no_mangle]
pub extern "C" fn webd_engine_focus(view: u64, focused: c_int) {
    log(format!("focus {view} {focused}"));
}

#[no_mangle]
pub extern "C" fn webd_engine_run_on_main(task: extern "C" fn(*mut c_void), context: *mut c_void) {
    task(context);
}

#[no_mangle]
pub extern "C" fn webd_engine_quit() {
    log("quit".into());
}

#[no_mangle]
pub unsafe extern "C" fn webd_engine_children(web: *mut c_int, network: *mut c_int) {
    unsafe {
        *web = 41;
        *network = 42;
    }
}

#[test]
fn the_view_config_has_the_headers_layout() {
    // webkit/port/webd/webd_abi.h: eight 4-byte integers, then the pointer, then (P2 slice 3 and the review of
    // 2026-10-22, append-only) the full views' fields.
    assert_eq!(offset_of!(WebdViewConfig, struct_size), 0);
    assert_eq!(offset_of!(WebdViewConfig, light), 4);
    assert_eq!(offset_of!(WebdViewConfig, scripts), 8);
    assert_eq!(offset_of!(WebdViewConfig, width), 12);
    assert_eq!(offset_of!(WebdViewConfig, height), 16);
    assert_eq!(offset_of!(WebdViewConfig, group), 20);
    assert_eq!(offset_of!(WebdViewConfig, color_scheme), 24);
    assert_eq!(offset_of!(WebdViewConfig, default_font_size), 28);
    assert_eq!(offset_of!(WebdViewConfig, user_style), 32);
    assert_eq!(align_of::<WebdViewConfig>(), align_of::<*const c_char>());
    // The appended fields keep their place: a caller's older, shorter struct stays valid (struct_size).
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(offset_of!(WebdViewConfig, features), 40);
        assert_eq!(offset_of!(WebdViewConfig, profile), 48);
        assert_eq!(offset_of!(WebdViewConfig, memory_only), 56);
        assert_eq!(offset_of!(WebdViewConfig, deltas), 60);
        assert_eq!(offset_of!(WebdViewConfig, user_agent_suffix), 64);
        assert_eq!(offset_of!(WebdViewConfig, no_network), 72);
        assert_eq!(offset_of!(WebdViewConfig, store_kind), 76);
        assert_eq!(offset_of!(WebdViewConfig, user_agent), 80);
        assert_eq!(size_of::<WebdViewConfig>(), 88);
    }
    #[cfg(target_pointer_width = "32")]
    {
        assert_eq!(offset_of!(WebdViewConfig, features), 36);
        assert_eq!(offset_of!(WebdViewConfig, store_kind), 60);
        assert_eq!(offset_of!(WebdViewConfig, user_agent), 64);
        assert_eq!(size_of::<WebdViewConfig>(), 68);
    }
}

// One test drives everything that touches the global sink and the stub log, so tests that run in
// parallel cannot see each other's calls.
#[test]
fn engine_calls_reach_the_c_functions_and_callbacks_reach_the_sink() {
    let queue = Arc::new(EventQueue::new().with_memory(|| Some((7, 99))));
    install(queue.clone());
    take_log();
    let engine = FfiEngine;

    engine.view_create(
        5,
        &EngineConfig {
            scripts: true,
            width: 320,
            height: 200,
            group: 3,
            color_scheme: 1,
            default_font_size: 18,
            user_style: "p{}".into(),
            full: None,
        },
    );
    engine.view_create(
        6,
        &EngineConfig {
            width: 1,
            height: 1,
            ..EngineConfig::default()
        },
    );
    // A private window's store reaches the engine as its own store kind, the client's own profile as 0.
    for (view, private_store, user_agent) in [(7, true, ""), (8, false, "Agent/1 (x)")] {
        engine.view_create(
            view,
            &EngineConfig {
                width: 1,
                height: 1,
                full: Some(crate::engine::FullConfig {
                    private_store,
                    memory_only: true,
                    user_agent: user_agent.into(),
                    ..Default::default()
                }),
                ..EngineConfig::default()
            },
        );
    }
    engine.load_html(5, b"<p>hi</p>", "about:blank");
    engine.resize(5, 640, 480);
    engine.set_visible(5, false);
    engine.refresh(5);
    engine.mouse(5, 1, 2, 10, 20, 3);
    engine.wheel(5, 1, 2, -1, 4, 0, 7);
    engine.key(5, true, 0xff0d, "\n", 4);
    engine.focus(5, true);
    engine.view_destroy(5);
    engine.quit();
    assert_eq!(engine.children(), (41, 42));
    let size = size_of::<WebdViewConfig>();
    assert_eq!(
        take_log(),
        vec![
            format!("create 5 size={size} light=1 scripts=1 320x200 group=3 scheme=1 font=18 style=p{{}}"),
            format!("create 6 size={size} light=1 scripts=0 1x1 group=0 scheme=0 font=0 style=NULL"),
            format!("create 7 size={size} light=0 scripts=0 1x1 group=0 scheme=0 font=0 style=NULL"),
            "full 7 features=0 profile=NULL memory_only=1 deltas=0 suffix=NULL no_network=0 store=1 agent=NULL"
                .to_string(),
            format!("create 8 size={size} light=0 scripts=0 1x1 group=0 scheme=0 font=0 style=NULL"),
            "full 8 features=0 profile=NULL memory_only=1 deltas=0 suffix=NULL no_network=0 store=0 agent=Agent/1 (x)"
                .to_string(),
            "html 5 <p>hi</p> base=about:blank".to_string(),
            "resize 5 640x480".to_string(),
            "visible 5 0".to_string(),
            "refresh 5".to_string(),
            "mouse 5 1 2 10 20 3".to_string(),
            "wheel 5 1 2 -1 4 0 7".to_string(),
            "key 5 1 65293 \n 4".to_string(),
            "focus 5 1".to_string(),
            "destroy 5".to_string(),
            "quit".to_string(),
        ]
    );

    extern "C" fn task(context: *mut c_void) {
        unsafe { *(context as *mut u32) += 1 };
    }
    let mut ran = 0u32;
    unsafe { FfiEngine::run_on_main(task, &mut ran as *mut u32 as *mut c_void) };
    assert_eq!(ran, 1);
    assert_eq!(unsafe { FfiEngine::start() }, Ok(()));

    unsafe {
        webd_ui_state(5, c"Title".as_ptr(), 1, 2, std::ptr::null());
        webd_ui_scroll(5, 3, 4, 1);
        webd_ui_frame(5, [9u8; 16].as_ptr(), 2, 2);
        webd_ui_frame(5, std::ptr::null(), 2, 2);
        webd_ui_frame(5, [9u8; 16].as_ptr(), 0, 2);
        webd_ui_link_clicked(5, c"https://example.com/".as_ptr());
        webd_ui_clipboard_write(5, c"copied".as_ptr());
        webd_ui_view_lost(6, 2);
        webd_ui_say(c"ENGINE:UP".as_ptr());
    }
    let drained = queue.drain();
    let state = &drained.states[&5];
    assert_eq!(
        (
            state.title.as_str(),
            state.loading,
            state.finished,
            state.error.as_str()
        ),
        ("Title", true, 2, "")
    );
    assert_eq!(drained.scrolls[&5], (3, 4, true));
    assert_eq!(drained.frames[&5].rgba, vec![9; 16]);
    assert_eq!(drained.frames.len(), 1, "a NULL or empty frame is ignored");
    assert_eq!(
        drained.once,
        vec![
            Once::LinkClicked {
                view: 5,
                url: "https://example.com/".into()
            },
            Once::ClipboardWrite {
                view: 5,
                text: "copied".into()
            },
            Once::Lost { view: 6, reason: 2 },
            Once::Say("ENGINE:UP".into()),
        ]
    );

    let (mut used, mut limit) = (0u64, 0u64);
    assert_eq!(unsafe { webd_ui_user_memory(&mut used, &mut limit) }, 0);
    assert_eq!((used, limit), (7, 99));
    assert_eq!(
        unsafe { webd_ui_user_memory(std::ptr::null_mut(), std::ptr::null_mut()) },
        0
    );
    install(Arc::new(EventQueue::new()));
    assert_ne!(unsafe { webd_ui_user_memory(&mut used, &mut limit) }, 0);
}
