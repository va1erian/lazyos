//! A resolved handle cannot hang up a service (audit #3).
//!
//! Every resolver aliases the side the owner published, so an explicit close
//! through one used to end that side for every client. Resolution now hands
//! out the published rights less `CONTROL`, and a close without `CONTROL`
//! only releases the caller's handle.

use super::*;
use crate::ipc::handles::rights;

/// Publish `callable` under `name` as the kernel task.
fn publish(name: &str, callable: u64) -> Result<(), String> {
    let published = handles::get(callable).map_err(friendly)?;
    registry::register(
        task::KERNEL_TASK,
        name,
        published.kind,
        published.rights,
        published.object_id,
        &[0xfeed],
        0,
    )
    .map_err(reason)
}

/// A client's explicit close of its resolved handle drops only that handle:
/// the service side stays open for the owner, and the owner's own close (it
/// holds `CONTROL`) still ends the side.
pub fn resolved_close_keeps_the_service() -> Result<(), String> {
    fresh()?;
    let (service, callable) = channels::create().map_err(friendly)?;
    publish("os.example.close", callable)?;
    let resolved = registry::resolve(task::KERNEL_TASK, "os.example.close").map_err(reason)?;
    let entry = handles::get(resolved).map_err(friendly)?;
    check!(
        entry.rights & rights::CONTROL == 0,
        "resolve handed out CONTROL ({:#x})",
        entry.rights
    );
    check!(
        entry.rights & rights::CALL != 0,
        "resolve dropped CALL ({:#x})",
        entry.rights
    );

    channels::close_endpoint(resolved).map_err(friendly)?;
    check!(
        handles::get(resolved).is_err(),
        "the closed handle is still in the table"
    );
    let hello = string_parcel(7, "hello")?;
    check!(
        channels::send(service, &hello).is_ok(),
        "a client's close ended the service side for everyone"
    );
    let got = channels::recv(callable, None).map_err(friendly)?;
    check!(!got.bytes.is_empty(), "the owner's handle received nothing");

    channels::close_endpoint(callable).map_err(friendly)?;
    check!(
        channels::send(service, &hello) == Err(channels::Error::PeerDied),
        "the owner's close (with CONTROL) left the side open"
    );
    Ok(())
}

/// Soak: many clients resolve and close in turn; the side never closes under
/// the owner and no handle leaks.
pub fn resolved_close_soak() -> Result<(), String> {
    fresh()?;
    let (service, callable) = channels::create().map_err(friendly)?;
    publish("os.example.close-soak", callable)?;
    let baseline = handles::count_for_task(task::KERNEL_TASK);
    let hello = string_parcel(7, "x")?;
    for round in 0..5_000u32 {
        let resolved =
            registry::resolve(task::KERNEL_TASK, "os.example.close-soak").map_err(reason)?;
        channels::close_endpoint(resolved).map_err(friendly)?;
        check!(
            channels::send(service, &hello).is_ok(),
            "round {round}: a client's close ended the side"
        );
        channels::recv(callable, None).map_err(friendly)?;
    }
    check!(
        handles::count_for_task(task::KERNEL_TASK) == baseline,
        "{} handles leaked",
        handles::count_for_task(task::KERNEL_TASK) - baseline
    );
    Ok(())
}
