//! `LAZYOS_TRACE` for the std apps (docs/tracing-plan.md, T3), read the way
//! `kernel/build.rs` and `user/build.rs` read it: `LAZYOS_TRACE=0` compiles
//! `xui_app::trace`'s macros out (the arguments stay type-checked, nothing is
//! evaluated); the old switches imply it back on. `tools/xui/build.py` passes
//! the environment through, so one `LAZYOS_TRACE=0 cargo build` makes an
//! image whose apps print no markers at all.

use std::env;
use std::ffi::OsStr;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LAZYOS_TRACE");
    println!("cargo:rustc-check-cfg=cfg(lazyos_trace)");
    let on = |name: &str| {
        println!("cargo:rerun-if-env-changed={name}");
        env::var_os(name).as_deref() == Some(OsStr::new("1"))
    };
    let aliased = ["LAZYOS_LABEL_TRACE", "LAZYOS_USB_TRACE", "LAZYOS_UI_PROBE"].map(on);
    let trace_off = env::var_os("LAZYOS_TRACE").as_deref() == Some(OsStr::new("0"));
    if aliased.contains(&true) || !trace_off {
        println!("cargo:rustc-cfg=lazyos_trace");
    }
}
