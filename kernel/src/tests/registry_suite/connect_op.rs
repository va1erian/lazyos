//! `OP_CONNECT` through the native gate (issue #483).

use super::*;
use crate::ipc::syscalls::OP_CONNECT;

/// `OP_CONNECT` returns a fresh handle to the caller's own channel, the
/// service receives the other end as `Connected`, an unknown name is
/// `ENOENT`, and a labelled task's connect obeys the same name policy as a
/// resolve (here: none loaded, so a plain task may connect).
pub fn syscall_connect() -> Result<(), String> {
    fresh()?;
    let (listen, clients) = channels::create().map_err(friendly)?;
    let entry = handles::get(clients).map_err(friendly)?;
    registry::register(
        task::KERNEL_TASK,
        "os.example.connect",
        entry.kind,
        entry.rights,
        entry.object_id,
        &[],
        0,
    )
    .map_err(reason)?;
    in_space(|| -> Result<(), String> {
        let connect = |name: &str| -> Result<(u64, MsgResult), String> {
            let body = registry::wire::encode_connect_args(&registry::wire::ConnectArgs {
                name: name.into(),
            })
            .map_err(friendly)?;
            let request = encode_parcel(registry::method::CONNECT, body)?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            Ok(dispatch(OP_CONNECT, &args))
        };
        let (code, result) = connect("os.example.connect")?;
        check!(code == 0, "OP_CONNECT -> {code:#x} ({})", result.status);
        let own = handles::get(result.value).map_err(friendly)?;
        check!(
            own.kind == HandleKind::Channel && own.object_id != entry.object_id,
            "the connection aliases the registered endpoint: {own:?}"
        );
        let notice = channels::try_recv(listen)
            .map_err(friendly)?
            .ok_or("the service got no Connected notice")?;
        check!(
            notice.method == registry::method::CONNECTED && notice.handles.len() == 1,
            "the notice is method {} with {} handles",
            notice.method,
            notice.handles.len()
        );
        let (code, _) = connect("os.example.nobody")?;
        check!(code == failed(errno::ENOENT), "unknown name -> {code:#x}");
        Ok(())
    })?;
    channels::reset();
    registry::reset();
    Ok(())
}
