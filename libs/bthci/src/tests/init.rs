//! The init step function against a scripted controller.

use alloc::vec::Vec;

use crate::addr::BdAddr;
use crate::command::Command;
use crate::event::Event;
use crate::init::{Init, InitConfig, InitError, Input, Phase};
use crate::opcode::{self as op, status};
use crate::ret::{self, BufferSize, LocalVersion};

const ADDR: BdAddr = BdAddr([1, 2, 3, 4, 5, 6]);

/// What a plain controller answers.
fn answer(cmd: &Command, le: bool) -> Event {
    let opcode = cmd.opcode();
    let ok = |rp: Vec<u8>| Event::CommandComplete {
        num_commands: 1,
        opcode,
        return_params: rp,
    };
    match cmd {
        Command::ReadBufferSize => ok(ret::encode_buffer_size(BufferSize {
            acl_len: 339,
            acl_num: 12,
        })),
        Command::LeReadBufferSize if le => ok(ret::encode_le_buffer_size(BufferSize {
            acl_len: 251,
            acl_num: 8,
        })),
        Command::ReadBdAddr => ok(ret::encode_bd_addr(ADDR)),
        Command::ReadLocalVersion => ok(ret::encode_local_version(&LocalVersion {
            hci_version: 9,
            hci_revision: 1,
            lmp_version: 9,
            manufacturer: 2,
            lmp_subversion: 3,
        })),
        Command::LeCreateConnection { .. } => Event::CommandStatus {
            status: 0,
            num_commands: 1,
            opcode,
        },
        c if !le && c.opcode() >> 10 == op::OGF_LE => ok(alloc::vec![status::UNKNOWN_COMMAND]),
        _ => ok(alloc::vec![0]),
    }
}

/// Run to a terminal phase; returns the commands in order and the phases seen
/// at each send.
fn run(cfg: &InitConfig, le: bool) -> (Init, Vec<Command>, Vec<Phase>) {
    let mut init = Init::new(cfg);
    let mut sent = Vec::new();
    let mut phases = Vec::new();
    let mut step = init.step(0, Input::Start);
    let mut now = 0;
    while let Some(cmd) = step.send.take() {
        phases.push(step.phase);
        let ev = answer(&cmd, le);
        sent.push(cmd);
        now += 1;
        step = init.step(now, Input::Event(&ev));
    }
    (init, sent, phases)
}

fn cfg_with_bonds() -> InitConfig {
    InitConfig {
        le_accept_list: alloc::vec![(0, ADDR), (1, BdAddr([9; 6]))],
        ..InitConfig::default()
    }
}

#[test]
fn the_fast_path_comes_first() {
    let (init, sent, phases) = run(&cfg_with_bonds(), true);
    assert_eq!(init.phase(), Phase::Complete);
    let pos = |pred: &dyn Fn(&Command) -> bool| sent.iter().position(pred).unwrap();
    let reset = pos(&|c| *c == Command::Reset);
    let scan = pos(&|c| matches!(c, Command::WriteScanEnable(2)));
    let mask = pos(&|c| matches!(c, Command::SetEventMask(_)));
    let auto = pos(&|c| {
        matches!(
            c,
            Command::LeCreateConnection {
                filter_policy: 1,
                ..
            }
        )
    });
    let class = pos(&|c| matches!(c, Command::WriteClassOfDevice(_)));
    let name = pos(&|c| matches!(c, Command::WriteLocalName(_)));
    let bdaddr = pos(&|c| *c == Command::ReadBdAddr);
    assert_eq!(reset, 0);
    assert!(
        mask < scan && scan < auto,
        "masks, then page scan, then LE auto-connect"
    );
    assert!(
        auto < class && class < name && name < bdaddr,
        "cosmetic commands last"
    );
    // The phase flips to SlowPath exactly at the first slow command.
    assert_eq!(phases[auto], Phase::FastPath);
    assert_eq!(phases[class], Phase::SlowPath);
    assert!(
        sent.iter()
            .filter(|c| matches!(c, Command::LeAddDeviceToAcceptList { .. }))
            .count()
            == 2
    );
    assert!(sent.contains(&Command::LeClearAcceptList));
}

#[test]
fn info_is_collected() {
    let (init, _, _) = run(&InitConfig::default(), true);
    let info = init.info();
    assert_eq!(info.bd_addr, Some(ADDR));
    assert_eq!((info.acl.acl_len, info.acl.acl_num), (339, 12));
    assert_eq!((info.le_acl.acl_len, info.le_acl.acl_num), (251, 8));
    assert!(info.le_supported);
    assert_eq!(info.version.unwrap().manufacturer, 2);
    assert!(init.is_armed());
}

#[test]
fn no_accept_list_means_no_auto_connect() {
    let (_, sent, _) = run(&InitConfig::default(), true);
    assert!(!sent.iter().any(|c| matches!(
        c,
        Command::LeCreateConnection { .. } | Command::LeClearAcceptList
    )));
}

#[test]
fn a_controller_without_le_skips_every_le_command() {
    let (init, sent, _) = run(&cfg_with_bonds(), false);
    assert_eq!(init.phase(), Phase::Complete);
    assert!(!init.info().le_supported);
    let le: Vec<_> = sent
        .iter()
        .filter(|c| c.opcode() >> 10 == op::OGF_LE)
        .collect();
    assert_eq!(le, [&Command::LeReadBufferSize], "only the probe is sent");
    assert!(sent.contains(&Command::WriteScanEnable(2)));
}

#[test]
fn page_scan_can_be_left_off() {
    let cfg = InitConfig {
        page_scan: false,
        ..InitConfig::default()
    };
    let (_, sent, _) = run(&cfg, true);
    assert!(!sent
        .iter()
        .any(|c| matches!(c, Command::WriteScanEnable(_))));
}

#[test]
fn a_required_command_failing_aborts() {
    let mut init = Init::new(&InitConfig::default());
    init.step(0, Input::Start);
    let ev = Event::CommandComplete {
        num_commands: 1,
        opcode: op::RESET,
        return_params: alloc::vec![0x03],
    };
    let step = init.step(1, Input::Event(&ev));
    assert_eq!(
        step.phase,
        Phase::Failed(InitError::CommandFailed {
            opcode: op::RESET,
            status: 3
        })
    );
    assert!(step.send.is_none());
    // A terminal machine ignores everything after.
    assert!(init.step(2, Input::Tick).send.is_none());
}

#[test]
fn unrelated_events_and_wrong_opcodes_are_ignored() {
    let mut init = Init::new(&InitConfig::default());
    init.step(0, Input::Start);
    let other = Event::CommandComplete {
        num_commands: 1,
        opcode: op::READ_BD_ADDR,
        return_params: alloc::vec![0],
    };
    assert!(init.step(1, Input::Event(&other)).send.is_none());
    let conn = Event::LinkKeyRequest { addr: ADDR };
    assert!(init.step(1, Input::Event(&conn)).send.is_none());
    assert_eq!(init.phase(), Phase::FastPath);
}

#[test]
fn reset_is_retried_once_then_times_out() {
    let mut init = Init::new(&InitConfig::default());
    let first = init.step(0, Input::Start);
    assert_eq!(first.deadline_ms, Some(3_000));
    assert!(init.step(2_999, Input::Tick).send.is_none());
    let retry = init.step(3_000, Input::Tick);
    assert_eq!(retry.send, Some(Command::Reset));
    let dead = init.step(6_000, Input::Tick);
    assert_eq!(
        dead.phase,
        Phase::Failed(InitError::Timeout { opcode: op::RESET })
    );
}

#[test]
fn a_short_response_is_a_bad_response() {
    let mut init = Init::new(&InitConfig::default());
    init.step(0, Input::Start);
    let ok = Event::CommandComplete {
        num_commands: 1,
        opcode: op::RESET,
        return_params: alloc::vec![0],
    };
    init.step(1, Input::Event(&ok));
    let short = Event::CommandComplete {
        num_commands: 1,
        opcode: op::READ_BUFFER_SIZE,
        return_params: alloc::vec![0, 1],
    };
    let step = init.step(2, Input::Event(&short));
    assert_eq!(
        step.phase,
        Phase::Failed(InitError::BadResponse {
            opcode: op::READ_BUFFER_SIZE
        })
    );
}

#[test]
fn start_twice_does_nothing() {
    let mut init = Init::new(&InitConfig::default());
    init.step(0, Input::Start);
    assert!(init.step(1, Input::Start).send.is_none());
}
