//! `LAZYOS_DEVTOOLS=1` (docs/devtool-packages-plan.md): refused without the
//! desktop and the `rhai` command, and a clear message when a tool is not built.

use crate::devtools_embed::{not_built, requirements, ships, TOOLS};

#[test]
fn unset_needs_nothing() {
    assert_eq!(requirements(false, false, false), Ok(()));
}

#[test]
fn the_desktop_with_rhai_ships_them() {
    assert_eq!(requirements(true, true, true), Ok(()));
}

#[test]
fn a_missing_profile_is_named() {
    let both = requirements(true, false, false).unwrap_err();
    assert!(
        both.contains("LAZYOS_DESKTOP=1 and the `rhai` command"),
        "{both}"
    );
    let rhai = requirements(true, true, false).unwrap_err();
    assert!(rhai.contains("rhai") && !rhai.contains("DESKTOP"), "{rhai}");
    let desktop = requirements(true, false, true).unwrap_err();
    assert!(
        desktop.contains("LAZYOS_DESKTOP=1") && !desktop.contains("rhai` command"),
        "{desktop}"
    );
    assert!(both.contains("run_demo.py --devtools"));
}

#[test]
fn the_terminal_ships_always_and_the_playground_on_request() {
    let tool = |short: &str| TOOLS.iter().find(|tool| tool.short == short).unwrap();
    assert_eq!(tool("terminal").elf, "xui-term.elf");
    assert_eq!(tool("playground").elf, "xui-playground.elf");
    // (devtools, desktop): every desktop has the Terminal, only the switch
    // brings the Playground, and no tool is embedded without a desktop unless
    // the switch demanded it (`requirements` then refuses the build).
    for (devtools, desktop, terminal, playground) in [
        (false, true, true, false),
        (true, true, true, true),
        (false, false, false, false),
    ] {
        assert_eq!(ships(tool("terminal"), devtools, desktop), terminal);
        assert_eq!(ships(tool("playground"), devtools, desktop), playground);
    }
    assert_eq!(TOOLS.iter().filter(|tool| tool.always).count(), 1);
}

#[test]
fn not_built_says_what_to_run() {
    let tool = &TOOLS[0];
    assert!(not_built(tool, false).contains("tools/xui/build.py"));
    assert!(not_built(tool, false).contains(tool.elf));
    assert!(not_built(tool, true).contains("tools/xui/core_packages.py"));
}

#[test]
fn every_tool_is_a_core_stem() {
    for tool in TOOLS {
        assert!(
            crate::core_packages::is_core_stem(tool.short),
            "{} is not in core_packages::is_core_stem",
            tool.short
        );
    }
}
