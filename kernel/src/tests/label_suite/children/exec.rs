//! A real ring-3 `execve` of a bound program: the hook in `sys_execve`, end
//! to end.
//!
//! A forked task of the app's label (a few bytes of machine code in its own
//! page) `execve`s a helper program written to the ABI ramfs; the helper
//! itself `execve`s a path that is not there, which a child label refuses
//! with `EPERM` before it looks, and exits 7 for that or 3 for the `ENOENT` a
//! task that may start programs gets. So the exit status 7 says that the exec
//! went through the real syscall path, the image took the child label and the
//! label's rule applied; a task of another label running the very same helper
//! is the control (3). Nothing is ever forked or started.

use super::*;
use crate::fs::vfs::Id;
use crate::mem::vma::{Kind as VmaKind, Prot};

/// Where the forked task's code is mapped in its own address space.
const PEER_CODE: u64 = 0x0060_0000;
/// The helper program, in the ABI ramfs.
const HELPER_PATH: &str = "/tmp/lazyos-child-helper";
/// The helper's exit status when its `execve` was refused with `EPERM`.
const REFUSED: u64 = 7;
/// Its exit status when the `execve` was allowed to fail with `ENOENT`.
const ALLOWED: u64 = 3;

/// A program that `execve(path, NULL, NULL)`s and then runs `epilogue` with
/// the result in `rax`; the path follows the code, which ends in `exit_group`.
fn exec_program(path: &str, epilogue: &[u8]) -> Vec<u8> {
    let mut code = vec![
        0xb8, 0x3b, 0x00, 0x00, 0x00, // mov eax, 59 (execve)
        0x48, 0x8d, 0x3d, // lea rdi, [rip + rel32] -> the path, after the code
    ];
    // `rel32` counts from the end of the `lea` (offset 12) to the path.
    let rel = 6 + epilogue.len() as u32;
    code.extend_from_slice(&rel.to_le_bytes());
    code.extend_from_slice(&[
        0x31, 0xf6, // xor esi, esi (argv: none)
        0x31, 0xd2, // xor edx, edx (envp: none)
        0x0f, 0x05, // syscall
    ]);
    code.extend_from_slice(epilogue);
    code.extend_from_slice(path.as_bytes());
    code.push(0);
    code
}

/// The forked task's epilogue: `exit_group(rax)`, the failure of its
/// `execve` as an exit status.
const STUB_EPILOGUE: [u8; 9] = [
    0x89, 0xc7, //                   mov edi, eax
    0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, 231 (exit_group)
    0x0f, 0x05, //                   syscall
];

/// The helper's epilogue: `exit_group(7)` when its own `execve` returned
/// `-EPERM` (`-1`), `exit_group(3)` otherwise (`-ENOENT`: the path is not
/// there, which is what a task allowed to start programs gets). Nothing is
/// ever started.
const HELPER_EPILOGUE: [u8; 25] = [
    0x48, 0x83, 0xf8, 0xff, //       cmp rax, -1
    0x75, 0x07, //                   jne +7
    0xbf, 0x07, 0x00, 0x00, 0x00, // mov edi, 7
    0xeb, 0x05, //                   jmp +5
    0xbf, 0x03, 0x00, 0x00, 0x00, // mov edi, 3
    0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, 231 (exit_group)
    0x0f, 0x05, //                   syscall
];

/// What the helper tries to start.
const NOWHERE: &str = "/lazyos-no-such-program";

/// A static ELF64 whose one segment is `code`, entered at its first byte.
fn elf_of(code: &[u8]) -> Vec<u8> {
    let mut elf = service_suite::minimal_elf();
    elf.resize(0x100 + code.len(), 0);
    elf[0x100..].copy_from_slice(code);
    let size = (code.len() as u64).to_le_bytes();
    // `p_filesz` and `p_memsz` of the only program header.
    elf[64 + 32..64 + 40].copy_from_slice(&size);
    elf[64 + 40..64 + 48].copy_from_slice(&size);
    elf
}

/// A forked task, credentialled `cred`, that will `execve` the helper from
/// ring 3 when it is next scheduled.
fn spawn_peer(cred: Cred) -> Result<usize, String> {
    let slot = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
    credentials::set(slot, cred);
    let pml4 = x86_64::PhysAddr::new(task::harness::pml4(slot).ok_or("the peer has no table")?);
    let pages = process::map_range_kind(
        pml4,
        PEER_CODE,
        PEER_CODE + 4096,
        Prot::READ | Prot::WRITE | Prot::EXEC,
        VmaKind::Anon,
    )
    .map_err(|error| format!("map the program: {error}"))?;
    let (_, phys) = *pages.first().ok_or("no code page")?;
    let code = exec_program(HELPER_PATH, &STUB_EPILOGUE);
    let page = mem::phys_to_virt(x86_64::PhysAddr::new(phys)).as_mut_ptr::<u8>();
    // SAFETY: `phys` is the frame `map_range_kind` just allocated for the
    // peer, reached through the physical-memory map; the stub is under 64
    // bytes and the frame is a page.
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), page, code.len()) };
    check!(
        task::harness::set_user_frame(slot, PEER_CODE, PEER_CODE + 4096),
        "could not point the peer at its program"
    );
    Ok(slot)
}

/// Let `slot` run, parking the kernel task a tick at a time, until it exits;
/// its status.
fn run_to_exit(slot: usize) -> Result<u64, String> {
    for _ in 0..400 {
        task::wait_sleep(task::ticks() + 1);
        if let Some(status) = task::reap_child_slot(slot) {
            return Ok(status);
        }
    }
    Err(format!("the peer {slot} did not exit"))
}

/// The newest audit record that satisfies `wanted`.
fn find_event(wanted: impl Fn(&audit::AuditEvent) -> bool) -> Option<audit::AuditEvent> {
    audit::recent(64).into_iter().find(|event| wanted(event))
}

/// An app's own ring-3 `execve` of its declared helper enters the child
/// label for real (the exit status), and the same helper run under another
/// label does not.
pub fn child_label_real_execve() -> Result<(), String> {
    fresh_children()?;
    task::set_blocked(false);
    x86_64::instructions::interrupts::without_interrupts(|| {
        // The labels and the binding, as `pkgd` loads them.
        let app = labels::intern(APP).map_err(|_| "intern the app")?;
        let other = labels::intern(OTHER).map_err(|_| "intern the other app")?;
        let child =
            labels::intern("app:org.test.browser/helper").map_err(|_| "intern the child")?;
        for id in [app, other, child] {
            acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
        }
        check!(
            children::bind(child, HELPER_PATH) == Ok(Vec::new()),
            "binding the helper"
        );
        crate::fs::install_abi_ramfs_for_test();
        let _ = crate::fs::abi_create(Id::current(), HELPER_PATH, 0o755);
        let helper = elf_of(&exec_program(NOWHERE, &HELPER_EPILOGUE));
        crate::fs::abi_write(Id::current(), HELPER_PATH, 0, &helper).map_err(|e| e.message())?;

        // The app's task: the helper's own `execve` is refused, so it exits 7.
        let before = audit::total();
        let peer = spawn_peer(Cred::new(UID, UID, 0, app, SESSION))?;
        let status = run_to_exit(peer)?;
        check!(
            status == REFUSED,
            "the app's helper exited {status} (wanted {REFUSED}: its execve refused as a child)"
        );
        check!(audit::total() > before, "nothing was audited");
        let entered = find_event(|event| {
            event.allow
                && event.actor_slot == peer
                && event.label_id == app
                && event.interface_id == credentials::AUDIT_INTERFACE
                && event.txn_id == peer as u64
        });
        check!(
            entered.is_some(),
            "the entry into the child was not audited"
        );
        let refused = find_event(|event| {
            !event.allow
                && event.actor_slot == peer
                && event.label_id == child
                && event.reason_code == reason::CHILD_MAY_NOT_START
                && event.method == fnv1a32("execve")
        });
        check!(refused.is_some(), "the refused execve was not audited");

        // The control: the same program under another app's label is not a
        // child of it, so the helper runs with that app's own label and its
        // `execve` gets as far as `ENOENT`.
        let peer = spawn_peer(Cred::new(UID, UID, 0, other, SESSION))?;
        let status = run_to_exit(peer)?;
        check!(
            status == ALLOWED,
            "the other app's helper exited {status} (wanted {ALLOWED}: execve allowed to fail)"
        );
        Ok(())
    })
}
