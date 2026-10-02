//! Compile the doomgeneric engine into `libdoomgeneric.a` for the musl target.
//!
//! The engine source is not in the tree: `tools/doom/build.py` fetches it at a
//! pinned revision (hash-checked) and points `DOOMGENERIC_SRC` at its
//! `doomgeneric/` directory. Host builds (`cargo test`, which only exercises the
//! pure modules of the library) skip the C entirely, so the tests need no C
//! toolchain and no engine source.

use std::env;
use std::path::PathBuf;

/// The engine's translation units: the upstream `Makefile`'s `SRC_DOOM` minus
/// its X11 platform file (`doomgeneric_xlib.c`), whose role `src/hooks.rs` plays.
const SOURCES: &[&str] = &[
    "dummy", "am_map", "doomdef", "doomstat", "dstrings", "d_event", "d_items", "d_iwad",
    "d_loop", "d_main", "d_mode", "d_net", "f_finale", "f_wipe", "g_game", "hu_lib",
    "hu_stuff", "info", "i_cdmus", "i_endoom", "i_joystick", "i_scale", "i_sound", "i_system",
    "i_timer", "memio", "m_argv", "m_bbox", "m_cheat", "m_config", "m_controls", "m_fixed",
    "m_menu", "m_misc", "m_random", "p_ceilng", "p_doors", "p_enemy", "p_floor", "p_inter",
    "p_lights", "p_map", "p_maputl", "p_mobj", "p_plats", "p_pspr", "p_saveg", "p_setup",
    "p_sight", "p_spec", "p_switch", "p_telept", "p_tick", "p_user", "r_bsp", "r_data",
    "r_draw", "r_main", "r_plane", "r_segs", "r_sky", "r_things", "sha1", "sounds",
    "statdump", "st_lib", "st_stuff", "s_sound", "tables", "v_video", "wi_stuff",
    "w_checksum", "w_file", "w_main", "w_wad", "z_zone", "w_file_stdc", "i_input",
    "i_video", "doomgeneric",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DOOMGENERIC_SRC");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" || target_env != "musl" {
        return;
    }
    let source = env::var_os("DOOMGENERIC_SRC").map(PathBuf::from).unwrap_or_else(|| {
        panic!("DOOMGENERIC_SRC is unset; build Doom with `python tools/doom/build.py`")
    });
    let mut build = cc::Build::new();
    build
        .include(&source)
        // The upstream Makefile's platform defines, minus `SNDSERV` (no sound
        // server: sound is off, `FEATURE_SOUND` stays undefined).
        .define("NORMALUNIX", None)
        .define("LINUX", None)
        .define("_DEFAULT_SOURCE", None)
        // 1990s C: implicit declarations and int/pointer mixing are part of
        // the engine as written; it is compiled as-is, never patched.
        .flag_if_supported("-std=gnu89")
        .flag_if_supported("-Wno-implicit-function-declaration")
        .flag_if_supported("-Wno-int-conversion")
        .flag_if_supported("-fno-strict-aliasing")
        .warnings(false);
    for name in SOURCES {
        let file = source.join(format!("{name}.c"));
        println!("cargo:rerun-if-changed={}", file.display());
        build.file(file);
    }
    build.compile("doomgeneric");
}
