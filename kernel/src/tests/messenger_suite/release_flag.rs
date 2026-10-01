//! The `CLOSE_RELEASE` flag through the native syscall (networking plan N2):
//! accepted on `close_endpoint` only, every other flag value refused first.

use super::*;
use crate::ipc::syscalls::CLOSE_RELEASE;

/// `-EINVAL` as `rax` carries it.
const EINVAL_CODE: u64 = (-22i64) as u64;

pub fn release_flag_is_accepted_only_on_close() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let (first, second) = (created.value, created.aux);

        // Flags other than the release bit are refused before anything happens.
        for bad in [2u64, 3, 1 << 32, u64::MAX] {
            let args = MsgArgs {
                handle: first,
                flags: bad,
                ..MsgArgs::default()
            };
            let (code, _) = syscall(OP_CLOSE_ENDPOINT, &args);
            check!(code == EINVAL_CODE, "flags {bad:#x} on close -> {code:#x}");
            check!(
                handles::get(first).is_ok(),
                "a refused close dropped the handle"
            );
        }
        // The release flag on another op is refused too.
        let args = MsgArgs {
            flags: CLOSE_RELEASE,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_CREATE_PAIR, &args);
        check!(
            code == EINVAL_CODE,
            "the release flag on create_pair -> {code:#x}"
        );

        // A release drops the handle; a second one fails; a plain close still works.
        let args = MsgArgs {
            handle: first,
            flags: CLOSE_RELEASE,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_CLOSE_ENDPOINT, &args);
        check!(code == 0, "a release -> {code:#x}");
        check!(handles::get(first).is_err(), "the release left the handle");
        let (code, _) = syscall(OP_CLOSE_ENDPOINT, &args);
        check!(code != 0, "a second release succeeded");
        let args = MsgArgs {
            handle: second,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_CLOSE_ENDPOINT, &args);
        check!(code == 0, "the plain close still works: {code:#x}");
        Ok(())
    })
}
