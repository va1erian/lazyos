//! The harness-only passkey hook (`HARNESS_SHOW_PASSKEY`) under the timing a
//! real boot has.

use super::*;

#[test]
fn a_passkey_shown_before_the_controller_is_known_reaches_the_simulator_later() {
    // A harness pairing can begin while the slow path of the bring-up is still
    // running: the passkey waits for the version, then goes out.
    let (sim, _, _) = le_world();
    let mut rig = Rig::boot(sim, BondStore::new(), Config::default(), 0);
    rig.host.ctl.harness_passkey = Some(123_456);
    rig.host.send_harness_passkey();
    assert_eq!(
        rig.host.ctl.harness_passkey,
        Some(123_456),
        "no version yet"
    );
    rig.run_for(300);
    rig.host.send_harness_passkey();
    assert_eq!(
        rig.host.ctl.harness_passkey, None,
        "sent once the version is known"
    );
    rig.run_for(10);
    let told = rig.sim.commands_seen().iter().any(|c| {
        matches!(c, bthci::Command::Unknown { opcode, .. }
            if *opcode == bthci::opcode::HARNESS_SHOW_PASSKEY)
    });
    assert!(told, "the simulator was told the passkey");
}
