//! HiDPI mode setting (docs/hidpi-plan.md, D1): the `display.*` config
//! parser, the scale rule, the adapter checks, and real switches on QEMU's
//! std VGA between the firmware mode and 2560x1440.

use super::*;
use crate::display::bochs::{self, ModeError};
use crate::display::modecfg::{self, DisplayCfg, Problem, Scale};

/// Parse `text` and collect the reported problems as strings.
fn parse_all(text: &str) -> (DisplayCfg, Vec<String>) {
    let mut problems = Vec::new();
    let cfg = modecfg::parse(text, |problem| problems.push(format!("{problem:?}")));
    (cfg, problems)
}

/// Well-formed lines, comments, other keys and the defaults.
pub fn mode_config_parses() -> Result<(), String> {
    let (cfg, problems) = parse_all(
        "root=UUID=x\n# display.mode=1x1\n  display.mode = 2560x1440  # HiDPI\ndisplay.scale=2\nlimit.fd_max=9\n",
    );
    check!(problems.is_empty(), "problems {problems:?}");
    check!(cfg.mode == Some((2560, 1440)), "mode {:?}", cfg.mode);
    check!(cfg.scale == Scale::Fixed(2), "scale {:?}", cfg.scale);
    check!(cfg.max.is_none(), "max {:?}", cfg.max);
    let (cfg, problems) = parse_all("display.scale=AUTO\n");
    check!(
        problems.is_empty() && cfg == DisplayCfg::default(),
        "auto {cfg:?}"
    );
    check!(parse_all("").0 == DisplayCfg::default(), "empty text");
    check!(
        modecfg::parse_mode("3840X2160") == Some((3840, 2160)),
        "upper-case separator"
    );
    // `display.max` (the logical cap, issue #717): same syntax and range as
    // a mode, machine-readable defaults absent.
    let (cfg, problems) = parse_all("# comment\ndisplay.max = 2560x1440 # 1440p panel\n");
    check!(problems.is_empty(), "problems {problems:?}");
    check!(cfg.max == Some((2560, 1440)), "max {:?}", cfg.max);
    check!(cfg.mode.is_none(), "mode {:?}", cfg.mode);
    let (cfg, problems) = parse_all("display.max=3840X2160\ndisplay.max=2560x1440\n");
    check!(
        cfg.max == Some((3840, 2160)),
        "first max wins {:?}",
        cfg.max
    );
    check!(problems.len() == 1, "duplicate {problems:?}");
    Ok(())
}

/// Hostile lines are reported one by one and never half applied; the first
/// of two repeated lines wins.
pub fn mode_config_refuses_hostile_lines() -> Result<(), String> {
    for bad in [
        "display.mode=",
        "display.mode=2560",
        "display.mode=x1440",
        "display.mode=2560x",
        "display.mode=-2560x1440",
        "display.mode=2560x1440x32",
        "display.mode=99999999999x1",
        "display.mode=639x480",
        "display.mode=3841x2160",
        "display.mode=2560 x 1440",
        "display.mode=+2560x1440",
        "display.scale=0",
        "display.scale=3",
        "display.scale=1.5",
        "display.scale=-1",
        "display.mode",
        "display.max=",
        "display.max=2560",
        "display.max=x1440",
        "display.max=2560x",
        "display.max=-1x1440",
        "display.max=639x480",
        "display.max=3841x2160",
        "display.max=2560x2161",
        "display.max=2560x1440x2",
        "display.max=99999999999x1",
        "display.max",
    ] {
        let (cfg, problems) = parse_all(bad);
        check!(cfg == DisplayCfg::default(), "{bad:?} applied: {cfg:?}");
        check!(problems.len() == 1, "{bad:?}: {problems:?}");
    }
    let (_, problems) = parse_all("display.depth=32\n");
    check!(
        problems == [format!("{:?}", Problem::Unknown("depth"))],
        "unknown {problems:?}"
    );
    let (cfg, problems) = parse_all("display.mode=1920x1080\ndisplay.mode=2560x1440\n");
    check!(cfg.mode == Some((1920, 1080)), "first wins {:?}", cfg.mode);
    check!(problems.len() == 1, "duplicate {problems:?}");
    Ok(())
}

/// `display.max` through the boot path (`modeset::apply_config`, issue
/// #717): the config arrives after `screen::init` recorded the 1080p fit, so
/// the cap re-fits the logical screen, re-derives the limits, and stays
/// put when the same text is re-read (a USB boot reads `lazyos.cfg` twice).
/// Everything is restored afterwards.
pub fn mode_display_max_applied() -> Result<(), String> {
    let scale = crate::console::scale();
    crate::display::reset();
    let result = crate::display::with_mode_for_test(2560, 1440, || -> Result<(), String> {
        // The hook fits 2560x1440 under the default cap first: the exact
        // state `lazyos.cfg` arrives at.
        let before = crate::display::logical();
        check!(
            (before.width, before.height) == (1920, 1080),
            "boot fit {before:?} not the default cap"
        );
        crate::display::modeset::apply_config("display.max=2560x1440\n");
        let after = crate::display::logical();
        check!(
            (after.x, after.y, after.width, after.height) == (0, 0, 2560, 1440),
            "refit {after:?}"
        );
        check!(
            crate::display::screen_bytes() == 2560 * 1440 * 4,
            "screen bytes {}",
            crate::display::screen_bytes()
        );
        check!(
            crate::limits::get(crate::limits::Id::SharedBufferMax) >= 2 * 2560 * 1440 * 4,
            "double-buffered 1440p over the limits"
        );
        // Re-read (a second `bootcfg::load`): the logical screen stays.
        crate::display::modeset::apply_config("# reboot-stage pass\ndisplay.max=2560x1440\n");
        let again = crate::display::logical();
        check!(
            again == after,
            "re-read changed the logical {after:?} -> {again:?}"
        );
        Ok(())
    });
    crate::console::set_scale(scale);
    let (width, height, stride, bpp) = crate::display::geometry_for_test();
    let restored = crate::display::init(width, height, stride, bpp);
    let _ = restored;
    crate::limits::init_for_machine(mem::usable_ram(), crate::display::screen_bytes());
    result
}

/// The automatic scale keeps a logical screen of at least 1280x720.
pub fn mode_auto_scale_rule() -> Result<(), String> {
    for (width, height, scale) in [
        (2560, 1440, 2),
        (3840, 2160, 2),
        (2560, 1600, 2),
        (1920, 1080, 1),
        (1280, 720, 1),
        (640, 480, 1),
        (2560, 1439, 1),
        (2559, 1440, 1),
        (0, 0, 1),
    ] {
        let got = modecfg::auto_scale(width, height);
        check!(got == scale, "{width}x{height}: {got} != {scale}");
        check!(
            Scale::Auto.resolve(width, height) == scale
                && Scale::Fixed(1).resolve(width, height) == 1,
            "{width}x{height}: resolve"
        );
    }
    Ok(())
}

/// The adapter limits are enforced before any register is written.
pub fn mode_adapter_checks() -> Result<(), String> {
    let vram = 16 * 1024 * 1024;
    check!(
        bochs::check_mode(2560, 1440, (2560, 1600), vram).is_ok(),
        "1440p in 16 MiB"
    );
    check!(
        bochs::check_mode(2560, 1600, (2560, 1600), vram).is_ok(),
        "1600p in 16 MiB"
    );
    check!(
        matches!(
            bochs::check_mode(3840, 2160, (3840, 2160), vram),
            Err(ModeError::NoVram { .. })
        ),
        "4K needs more than 16 MiB"
    );
    check!(
        matches!(
            bochs::check_mode(2561, 1440, (2560, 1600), vram),
            Err(ModeError::TooLarge { .. })
        ),
        "wider than the adapter"
    );
    check!(
        bochs::check_mode(640, 480, (2560, 1600), 0).is_err(),
        "zero VRAM refused"
    );
    check!(bochs::mode_bytes(2560, 1440) == 14_745_600, "mode bytes");
    Ok(())
}

/// Switch to `width x height`, then prove the whole framebuffer is live:
/// write and read back both far corners and check every geometry consumer.
pub(super) fn switch_and_probe(width: u32, height: u32) -> Result<(), String> {
    crate::display::modeset::switch_to(width, height)
        .map_err(|e| format!("{width}x{height}: {e}"))?;
    let size = crate::display::size();
    check!(
        size == (width as usize, height as usize),
        "{width}x{height}: display size {size:?}"
    );
    check!(
        crate::display::screen_bytes() == bochs::mode_bytes(width, height),
        "{width}x{height}: screen bytes"
    );
    let marker = crate::gfx::Color::rgb(0x12, 0x34, 0x56);
    let corners = [(0, 0), (width as usize - 1, height as usize - 1)];
    let read = crate::console::with_framebuffer(|fb| {
        check!(
            (fb.width(), fb.height()) == (width as usize, height as usize),
            "{width}x{height}: console framebuffer {}x{}",
            fb.width(),
            fb.height()
        );
        let mut out = Vec::new();
        for (x, y) in corners {
            fb.write_pixel(x, y, marker);
            out.push(fb.read_pixel(x, y));
        }
        Ok(out)
    })
    .ok_or("no framebuffer")??;
    for color in read {
        check!(
            (color.r, color.g, color.b) == (0x12, 0x34, 0x56),
            "{width}x{height}: pixel read back {color:?}"
        );
    }
    Ok(())
}

/// The boot geometry to restore after a test, and a run of `body` that
/// always restores it (mode, limits, console scale).
pub(super) fn with_boot_mode(
    body: impl FnOnce((u32, u32)) -> Result<(), String>,
) -> Result<(), String> {
    let (width, height, ..) = crate::display::geometry_for_test();
    let boot = (width as u32, height as u32);
    let scale = crate::console::scale();
    crate::display::reset();
    let outcome = body(boot);
    let restored = crate::display::modeset::switch_to(boot.0, boot.1);
    crate::console::set_scale(scale);
    outcome?;
    restored.map_err(|e| format!("restoring {}x{}: {e}", boot.0, boot.1))
}

/// A real switch to 2560x1440 and back on the std VGA adapter. A machine
/// without one must refuse cleanly and keep its mode.
pub fn mode_switch_roundtrip() -> Result<(), String> {
    if bochs::find().is_err() {
        let before = crate::display::size();
        check!(
            crate::display::modeset::switch_to(2560, 1440).is_err(),
            "switch without an adapter"
        );
        check!(crate::display::size() == before, "mode changed anyway");
        return Ok(());
    }
    with_boot_mode(|boot| {
        switch_and_probe(2560, 1440)?;
        check!(
            crate::limits::get(crate::limits::Id::SharedBufferMax)
                >= 3 * bochs::mode_bytes(2560, 1440),
            "limits not re-derived for 1440p"
        );
        switch_and_probe(boot.0, boot.1)
    })
}

/// Modes beyond the adapter are refused and leave the screen untouched;
/// a mode switch is refused while a compositor holds the display.
pub fn mode_switch_refusals() -> Result<(), String> {
    let Ok(adapter) = bochs::find() else {
        return Ok(());
    };
    let ((max_w, max_h), vram) = adapter.limits();
    check!(
        vram >= bochs::mode_bytes(2560, 1440),
        "VRAM {vram} too small for 1440p"
    );
    let before = crate::display::size();
    check!(
        crate::display::modeset::switch_to(max_w + 1, max_h).is_err(),
        "wider than the adapter accepted"
    );
    check!(
        crate::display::size() == before,
        "refused switch changed the mode"
    );
    with_boot_mode(|_| {
        let slot = scratch_task()?;
        let mut info = [0u64; crate::display::INFO_WORDS];
        let code =
            process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
        check!(code == 0, "bind -> {code:#x}");
        let refused = crate::display::modeset::switch_to(2560, 1440).is_err();
        let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
        task::harness::switch_current(task::KERNEL_TASK);
        task::harness::reset();
        let _ = slot;
        check!(code == 0, "unbind -> {code:#x}");
        check!(refused, "switch accepted under a bound compositor");
        Ok(())
    })
}

/// Soak: many switches between the boot mode, 1440p and 1080p, every one
/// probed, then the console scale toggled under text output.
pub fn mode_switch_soak() -> Result<(), String> {
    if bochs::find().is_err() {
        return Ok(());
    }
    with_boot_mode(|boot| {
        let modes = [(2560, 1440), (1920, 1080), boot];
        for round in 0..200 {
            let (width, height) = modes[round % modes.len()];
            switch_and_probe(width, height).map_err(|e| format!("round {round}: {e}"))?;
        }
        for round in 0..50 {
            crate::console::set_scale(1 + round % 2);
            check!(crate::console::scale() == 1 + round % 2, "scale {round}");
            let line = format!("display soak line {round}: the quick brown fox\n");
            crate::console::write_for_test(&line);
        }
        Ok(())
    })
}

/// A mode the adapter does not keep is undone: after the refusal path the
/// mode registers are exactly the saved ones, and the console's framebuffer
/// (still the old geometry) reads back what it writes at its far corner.
/// Repeated, so a register left behind by one round shows in the next.
pub fn mode_refused_switch_restores_registers() -> Result<(), String> {
    let Ok(adapter) = bochs::find() else {
        return Ok(());
    };
    let (width, height) = crate::display::size();
    for round in 0..50 {
        let outcome = crate::console::with_framebuffer(|fb| {
            let before = bochs::Registers::save();
            adapter.program_then_refuse_for_test(2560, 1440);
            let after = bochs::Registers::save();
            check!(
                after == before,
                "round {round}: registers {after:?} != {before:?}"
            );
            let marker = crate::gfx::Color::rgb(0x31, 0x42, 0x53);
            let (x, y) = (width - 1, height - 1);
            fb.write_pixel(x, y, marker);
            let color = fb.read_pixel(x, y);
            check!(
                (color.r, color.g, color.b) == (0x31, 0x42, 0x53),
                "round {round}: corner reads {color:?}"
            );
            Ok(())
        });
        outcome.ok_or("no framebuffer")??;
    }
    Ok(())
}
