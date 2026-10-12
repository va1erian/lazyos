//! Compile the Kart-Public engine and LazyOS's platform layer into
//! `libsrb2kart.a` for the musl target.
//!
//! The engine source is not in the tree: `tools/srb2kart/build.py` fetches the
//! v1.6 release archive (hash-checked) and points `SRB2KART_SRC` at its root
//! (the directory holding `src/`). We do not drive upstream's Makefile: the
//! file list below is its `OBJS` (the core objects, `blua`, `md5`) minus the
//! platform ones, which `c/lazyos/` replaces (`src/sdl/` is the model,
//! `src/dummy/` the checklist). Host builds (`cargo test`, which only exercises
//! the pure modules of the library) skip the C entirely, so the tests need no C
//! toolchain and no engine source.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Upstream `src/` files: the Makefile's core `OBJS`, minus `i_main`,
/// `i_system`, `i_net`, `i_sound`, `i_cdmus` (ours), `vid_copy.s` (x86
/// assembly, `NOASM`), `discord`/`stun` (off), `apng` (`NOPNG`) and the two
/// [`SHIMMED`] files.
const CORE: &[&str] = &[
    "comptime",
    "string",
    "md5",
    "d_main",
    "d_net",
    "d_netfil",
    "d_netcmd",
    "dehacked",
    "z_zone",
    "f_finale",
    "f_wipe",
    "g_game",
    "g_input",
    "am_map",
    "command",
    "console",
    "hu_stuff",
    "y_inter",
    "st_stuff",
    "k_kart",
    "m_aatree",
    "m_anigif",
    "m_argv",
    "m_bbox",
    "m_cheat",
    "m_cond",
    "m_fixed",
    "m_menu",
    "m_misc",
    "m_random",
    "m_queue",
    "info",
    "p_ceilng",
    "p_enemy",
    "p_floor",
    "p_inter",
    "p_lights",
    "p_map",
    "p_maputl",
    "p_mobj",
    "p_polyobj",
    "p_saveg",
    "p_setup",
    "p_sight",
    "p_spec",
    "p_telept",
    "p_tick",
    "p_user",
    "p_slopes",
    "tables",
    "r_bsp",
    "r_data",
    "r_draw",
    "r_fps",
    "r_main",
    "r_plane",
    "r_segs",
    "r_sky",
    "r_splats",
    "r_things",
    "screen",
    "v_video",
    "s_sound",
    "sounds",
    "w_wad",
    "filesrch",
    "http-mserv",
    "i_tcp",
    "i_time",
    "lzf",
    "b_bot",
];

/// The embedded Lua 5.1 interpreter (`src/blua/`) and SRB2's bindings
/// (`src/lua_*.c`): `blua/Makefile.cfg`'s object list.
const LUA: &[&str] = &[
    "blua/lapi",
    "blua/lbaselib",
    "blua/ldo",
    "blua/lfunc",
    "blua/linit",
    "blua/liolib",
    "blua/llex",
    "blua/lmem",
    "blua/lobject",
    "blua/lstate",
    "blua/lstrlib",
    "blua/ltablib",
    "blua/lundump",
    "blua/lzio",
    "blua/lauxlib",
    "blua/lcode",
    "blua/ldebug",
    "blua/ldump",
    "blua/lgc",
    "blua/lopcodes",
    "blua/lparser",
    "blua/lstring",
    "blua/ltable",
    "blua/ltm",
    "blua/lvm",
    "lua_script",
    "lua_baselib",
    "lua_blockmaplib",
    "lua_mathlib",
    "lua_hooklib",
    "lua_consolelib",
    "lua_infolib",
    "lua_mobjlib",
    "lua_playerlib",
    "lua_skinlib",
    "lua_thinkerlib",
    "lua_maplib",
    "lua_hudlib",
    "lua_hudlib_drawlist",
];

/// Upstream files compiled with a force-included shim from `c/lazyos/`: they
/// only build with libcurl (`HAVE_CURL`, `MASTERSERVER`), which this build
/// leaves out, because they use names that exist only inside those blocks
/// (`mserv.c`'s locking macros and ids, `d_clisrv.c`'s `CL_DOWNLOADHTTPFILES`).
/// The shim defines those as inert values, so the files are compiled as they
/// are.
const SHIMMED: &[(&str, &str)] = &[("mserv", "mserv_shim.h"), ("d_clisrv", "d_clisrv_shim.h")];

/// Our platform layer, `c/lazyos/`.
const PLATFORM: &[&str] = &[
    "i_main", "i_system", "i_video", "i_sound", "i_net", "i_cdmus", "i_input",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=c/lazyos");
    println!("cargo:rerun-if-env-changed=SRB2KART_SRC");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" || target_env != "musl" {
        return;
    }
    let root = env::var_os("SRB2KART_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!("SRB2KART_SRC is unset; build SRB2Kart with `python tools/srb2kart/build.py`")
        });
    let src = root.join("src");
    let ours = PathBuf::from("c/lazyos");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));

    // `comptime.c` includes this under `COMPVERSION` (upstream's `comptime.sh`
    // writes it from git; the archive has no git).
    fs::write(
        out.join("comptime.h"),
        "const char *compbranch = \"v1.6\";\nconst char *comprevision = \"lazyos\";\n",
    )
    .expect("write comptime.h");

    let base = || {
        let mut build = cc::Build::new();
        build
            .include(&src)
            .include(&ours)
            .include(&out)
            // Software renderer only, every optional library off
            // (docs/srb2kart-port-plan.md). MD5 stays: it verifies the data
            // files. Not NONET: upstream's NONET build does not compile
            // (i_tcp.c's ban list, mserv.c); the network driver is simply
            // never started until K3 (I_InitNetwork says no).
            .define("UNIXCOMMON", None)
            .define("LINUX", None)
            .define("LINUX64", None)
            .define("NOHW", None)
            .define("NONX86", None)
            .define("NOASM", None)
            .define("NOMIXER", None)
            .define("NOGME", None)
            .define("NOCURL", None)
            .define("NOPNG", None)
            .define("NOZLIB", None)
            .define("NOTHREADS", None)
            .define("NOUPNP", None)
            // m_menu.c only compiles its joystick page with this (upstream
            // gets it from src/sdl/sdlmain.h, which needs SDL).
            .define("JOYSTICK_HOTPLUG", None)
            .define("HAVE_BLUA", None)
            .define("LUA_USE_POSIX", None)
            .define("COMPVERSION", None)
            .define("_GNU_SOURCE", None)
            // C99-era code with GNU extensions; no AVX (LazyOS saves FXSAVE
            // state only), and it is compiled as it is, never patched.
            .flag_if_supported("-std=gnu99")
            .flag_if_supported("-O2")
            .flag_if_supported("-fwrapv")
            .flag_if_supported("-fno-strict-aliasing")
            .flag_if_supported("-fno-sanitize=undefined")
            .flag_if_supported("-fno-exceptions")
            .warnings(false);
        build
    };

    let mut build = base();
    for name in CORE.iter().chain(LUA) {
        let file = src.join(format!("{name}.c"));
        println!("cargo:rerun-if-changed={}", file.display());
        build.file(file);
    }
    for name in PLATFORM {
        build.file(ours.join(format!("{name}.c")));
    }
    build.compile("srb2kart");

    for (name, shim) in SHIMMED {
        let file = src.join(format!("{name}.c"));
        println!("cargo:rerun-if-changed={}", file.display());
        base()
            .flag("-include")
            .flag(format!("c/lazyos/{shim}"))
            .file(file)
            .compile(&format!("srb2kart_{name}"));
    }
}
