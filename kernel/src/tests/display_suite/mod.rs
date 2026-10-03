//! The kernel side of the userspace compositor: syscall 12 grants the
//! screen to a task, queues input for it, and presents damage
//! rectangles. These tests drive the same entry point the `int 0x80`
//! gate uses (`dispatch_for_test`) on a scratch user task, so the whole
//! path runs without a scheduler. Display device grant (issue #113).

use super::*;
use crate::input::keyboard::Key;

/// Two's-complement `-errno`, the syscall error encoding.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Slot of a scratch user task with its own handle table; `current()` is
/// pointed at it so the grant's `task::current()` checks see a user.
fn scratch_task() -> Result<usize, String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    // A fork inherits its parent's credentials, and `bind` needs
    // `CAP_SYS_ADMIN`: start from the root identity, not whatever an
    // earlier suite stamped on the kernel task.
    crate::ipc::credentials::reset_for_task(task::KERNEL_TASK);
    let slot = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(slot);
    Ok(slot)
}

/// One drained event: `(kind, a)` from the 16-byte kernel record.
fn event_at(events: &[u8], index: usize) -> (u32, i32) {
    let base = index * 16;
    (
        u32::from_le_bytes(events[base..base + 4].try_into().unwrap()),
        i32::from_le_bytes(events[base + 4..base + 8].try_into().unwrap()),
    )
}

mod bind_and_input;
mod buffers;
mod keys;
mod large_screens;
mod logical;
mod modes;
mod modifiers;
mod present;
mod slots;
mod wheel;

pub(super) use bind_and_input::*;
pub(super) use buffers::*;
pub(super) use keys::*;
pub(super) use large_screens::*;
pub(super) use logical::*;
pub(super) use modes::*;
pub(super) use modifiers::*;
pub(super) use present::*;
pub(super) use slots::*;
pub(super) use wheel::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("display_kernel_bind_refused", kernel_bind_refused),
    (
        "display_bind_input_present_roundtrip",
        bind_input_present_roundtrip,
    ),
    (
        "display_modifier_keys_reach_compositor",
        modifier_keys_reach_compositor,
    ),
    ("display_modifier_hotkey_soak", modifier_hotkey_soak),
    (
        "display_modifier_per_key_transitions",
        modifier_per_key_transitions,
    ),
    ("display_modifier_per_key_soak", modifier_per_key_soak),
    ("display_nav_and_function_keys", nav_and_function_keys),
    ("display_ctrl_letter_is_letter", ctrl_letter_is_letter),
    ("display_key_decode_soak", key_decode_soak),
    ("display_close_buffer_releases", close_buffer_releases),
    (
        "display_close_buffer_refuses_screen",
        close_buffer_refuses_screen,
    ),
    ("display_close_buffer_soak", close_buffer_soak),
    (
        "display_present_damage_rows_arithmetic",
        present_damage_rows_arithmetic,
    ),
    ("display_present_one_pixel", present_one_pixel),
    ("display_present_partial_unmap", present_partial_unmap),
    ("display_present_small_soak", present_small_soak),
    ("display_slots_attach_rules", slots_attach_rules),
    (
        "display_slots_present_and_release",
        slots_present_and_release,
    ),
    ("display_slots_damage_clipping", slots_damage_clipping),
    (
        "display_slots_double_buffer_ordering",
        slots_double_buffer_ordering,
    ),
    ("display_slots_present_soak", slots_present_soak),
    ("display_mouse_packet_decoding", mouse_packet_decoding),
    ("display_wheel_reaches_compositor", wheel_reaches_compositor),
    ("display_plain_mouse_and_resync", plain_mouse_and_resync),
    ("display_wheel_soak_bounded_queue", wheel_soak_bounded_queue),
    ("display_logical_bind_sizes", logical_bind_sizes),
    (
        "display_logical_present_offsets_and_clips",
        logical_present_offsets_and_clips,
    ),
    (
        "display_large_screens_fit_the_budgets",
        large_screens_fit_the_budgets,
    ),
    ("display_mode_config_parses", mode_config_parses),
    (
        "display_mode_config_refuses_hostile_lines",
        mode_config_refuses_hostile_lines,
    ),
    ("display_mode_auto_scale_rule", mode_auto_scale_rule),
    ("display_mode_adapter_checks", mode_adapter_checks),
    ("display_mode_switch_roundtrip", mode_switch_roundtrip),
    ("display_mode_switch_refusals", mode_switch_refusals),
    ("display_mode_switch_soak", mode_switch_soak),
    (
        "display_mode_refused_switch_restores_registers",
        mode_refused_switch_restores_registers,
    ),
];
