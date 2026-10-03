//! Per-uid resource quotas (issue #103).

use super::*;
use crate::ipc::channels;
use crate::ipc::credentials::{self, Cred};
use crate::ipc::handles::{self, rights, Error as HandleError, HandleKind};
use crate::ipc::shared;
use crate::quota::{self, Resource};
use libmessenger::{flags, Encoder, Header, Parcel, VERSION};

/// Every quota test starts from an empty ledger, root credentials, empty
/// registries, and a runnable kernel task.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    channels::reset();
    shared::reset();
    handles::reset_for_task(task::current());
    credentials::reset_for_task(task::current());
    quota::reset();
    Ok(())
}

/// Friendly-message adapters for `Result` plumbing.
fn handle_reason(error: HandleError) -> String {
    error.message().into()
}

fn channel_reason(error: channels::Error) -> String {
    error.message().into()
}

/// A minimal one-way parcel for the channel choke-point test.
fn one_way() -> Result<Vec<u8>, String> {
    let mut body = Encoder::new();
    body.u64(1, 7).map_err(|error| error.message())?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: 0x0102_0304,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    Ok(bytes)
}

/// Charges accumulate, record a peak and counters, and releases give the
/// usage back; an unseen uid reads the documented default table.
pub fn charge_release_accounting() -> Result<(), String> {
    fresh()?;
    let uid = 4242;
    let resource = Resource::KernelMemory;
    quota::set_limit(uid, resource, 4096);
    check!(
        quota::usage(uid, resource) == 0,
        "a fresh ledger has nonzero usage"
    );
    check!(
        quota::limit(uid, resource) == 4096,
        "set_limit did not stick: {}",
        quota::limit(uid, resource)
    );
    quota::charge(uid, resource, 1000).map_err(|error| error.message())?;
    quota::charge(uid, resource, 24).map_err(|error| error.message())?;
    check!(
        quota::usage(uid, resource) == 1024,
        "usage is {}, expected 1024",
        quota::usage(uid, resource)
    );
    let stats = quota::stats(uid);
    check!(
        stats.peak[resource.index()] == 1024,
        "peak is {}, expected 1024",
        stats.peak[resource.index()]
    );
    check!(
        stats.charges == 2,
        "charges are {}, expected 2",
        stats.charges
    );
    quota::release(uid, resource, 1000);
    check!(
        quota::usage(uid, resource) == 24,
        "release left {}, expected 24",
        quota::usage(uid, resource)
    );
    check!(
        quota::stats(uid).releases == 1,
        "the release was not counted"
    );

    // A uid with no ledger reads defaults; root reads the uncapped table.
    check!(
        quota::usage(9999, Resource::Handles) == 0,
        "an unseen uid has usage"
    );
    check!(
        quota::limit(9999, Resource::Handles)
            == quota::default_limits_regular()[Resource::Handles.index()],
        "an unseen uid did not read the default handle limit"
    );
    check!(
        quota::limit(0, Resource::Handles) == quota::ROOT_LIMITS[Resource::Handles.index()],
        "root is not uncapped"
    );
    quota::reset();
    Ok(())
}

/// A charge over the limit is refused with a friendly error that names the
/// resource and carries the usage and limit; the boundary is exact.
pub fn denial_friendly_error() -> Result<(), String> {
    fresh()?;
    let uid = 5150;
    let resource = Resource::QueueBytes;
    quota::set_limit(uid, resource, 256);
    quota::charge(uid, resource, 200).map_err(|error| error.message())?;

    let error = quota::charge(uid, resource, 100).expect_err("an over-limit charge succeeded");
    check!(
        error.uid == uid && error.resource == resource && error.usage == 200 && error.limit == 256,
        "the refusal lost its numbers: {error:?}"
    );
    let text = error.message();
    check!(text.contains("uid 5150"), "message lost the uid: {text}");
    check!(
        text.contains("200") && text.contains("256"),
        "message lost the usage/limit: {text}"
    );
    check!(
        text.contains("quota") && text.contains("bytes"),
        "message is not a friendly quota error: {text}"
    );
    check!(
        quota::usage(uid, resource) == 200,
        "a refused charge changed usage"
    );
    check!(
        quota::stats(uid).denials == 1,
        "the refusal was not counted"
    );

    // Exactly to the limit fits; one byte more does not.
    check!(
        quota::check(uid, resource, 56).is_ok(),
        "the boundary refused a fitting charge"
    );
    check!(
        quota::check(uid, resource, 57).is_err(),
        "the boundary allowed an over-limit charge"
    );
    quota::reset();
    Ok(())
}

/// A denied charge leaves usage alone; releasing part of it restores
/// headroom so the same amount fits again.
pub fn release_restores_headroom() -> Result<(), String> {
    fresh()?;
    let uid = 6262;
    let resource = Resource::Handles;
    quota::set_limit(uid, resource, 100);
    quota::charge(uid, resource, 100).map_err(|error| error.message())?;
    check!(
        quota::charge(uid, resource, 1).is_err(),
        "the limit was not enforced"
    );
    quota::release(uid, resource, 40);
    quota::charge(uid, resource, 40).map_err(|error| error.message())?;
    check!(
        quota::usage(uid, resource) == 100,
        "usage after release+recharge is {}",
        quota::usage(uid, resource)
    );
    check!(
        quota::charge(uid, resource, 1).is_err(),
        "headroom was restored beyond the limit"
    );
    check!(
        quota::stats(uid).over_releases == 0,
        "balanced releases reported an over-release"
    );
    quota::reset();
    Ok(())
}

/// Two tasks of one uid share a single per-uid handle limit through the
/// handle-table choke point, and teardown of either gives headroom back.
pub fn per_uid_aggregation() -> Result<(), String> {
    fresh()?;
    let uid = 7373;
    let first = task::MAX_TASKS - 1;
    let second = task::MAX_TASKS - 2;
    quota::set_limit(uid, Resource::Handles, 2);
    credentials::set(first, Cred::new(uid, 0, 0, 0, 0));
    credentials::set(second, Cred::new(uid, 0, 0, 0, 0));
    handles::reset_for_task(first);
    handles::reset_for_task(second);

    handles::open_for_task(first, HandleKind::Object, rights::CALL, 11).map_err(handle_reason)?;
    check!(
        quota::usage(uid, Resource::Handles) == 1,
        "the first task's handle was not charged"
    );
    handles::open_for_task(second, HandleKind::Object, rights::CALL, 12).map_err(handle_reason)?;
    check!(
        quota::usage(uid, Resource::Handles) == 2,
        "the two tasks did not aggregate: {}",
        quota::usage(uid, Resource::Handles)
    );

    // Each table still has room (MAX_HANDLES); the shared uid limit is what
    // refuses the third handle.
    check!(
        handles::open_for_task(first, HandleKind::Object, rights::CALL, 13)
            == Err(HandleError::Quota),
        "the per-uid aggregate handle limit was not enforced"
    );
    check!(
        quota::usage(uid, Resource::Handles) == 2,
        "a refused open leaked usage"
    );

    // Resetting one task's table releases only its handles.
    handles::reset_for_task(first);
    check!(
        quota::usage(uid, Resource::Handles) == 1,
        "teardown released the wrong number of handles"
    );
    handles::reset_for_task(second);
    check!(
        quota::usage(uid, Resource::Handles) == 0,
        "teardown stranded uid usage"
    );
    credentials::reset_for_task(first);
    credentials::reset_for_task(second);
    quota::reset();
    Ok(())
}

/// Native syscall 11 copies the caller's usage/limits block in resource
/// order and refuses a null pointer.
pub fn syscall_introspection() -> Result<(), String> {
    fresh()?;
    let uid = credentials::of(task::current()).uid;
    quota::set_limit(uid, Resource::QueueDepth, 77);
    quota::charge(uid, Resource::QueueDepth, 5).map_err(|error| error.message())?;

    let mut words = [0u64; quota::STATS_WORDS];
    let code = process::dispatch_for_test(11, words.as_mut_ptr() as u64, 0, 0);
    check!(code == 0, "quota syscall returned {code:#x}");
    for resource in Resource::ALL {
        let index = resource.index();
        check!(
            words[index * 2] == quota::usage(uid, resource),
            "usage word for {resource:?} is {}, expected {}",
            words[index * 2],
            quota::usage(uid, resource)
        );
        check!(
            words[index * 2 + 1] == quota::limit(uid, resource),
            "limit word for {resource:?} is {}, expected {}",
            words[index * 2 + 1],
            quota::limit(uid, resource)
        );
    }
    check!(
        words[Resource::QueueDepth.index() * 2] == 5,
        "queued usage word is {}, expected 5",
        words[Resource::QueueDepth.index() * 2]
    );
    check!(
        words[Resource::QueueDepth.index() * 2 + 1] == 77,
        "queued limit word is {}, expected 77",
        words[Resource::QueueDepth.index() * 2 + 1]
    );
    check!(
        process::dispatch_for_test(11, 0, 0, 0) == 0u64.wrapping_sub(14),
        "a null stats buffer was not refused with -EFAULT"
    );
    quota::reset();
    Ok(())
}

/// A shared buffer charges its frames to the creator's uid as kernel
/// memory, and closing it gives the charge back.
pub fn shared_buffer_charge() -> Result<(), String> {
    fresh()?;
    let uid = credentials::of(task::current()).uid;
    let resource = Resource::KernelMemory;
    let handle = shared::create(2 * 4096, shared::flags::READ | shared::flags::WRITE)
        .map_err(|error| error.message())?;
    let info = shared::info(handle).map_err(|error| error.message())?;
    check!(
        quota::usage(uid, resource) == info.size,
        "kernel memory usage is {}, buffer is {} bytes",
        quota::usage(uid, resource),
        info.size
    );
    shared::close(handle).map_err(|error| error.message())?;
    check!(
        quota::usage(uid, resource) == 0,
        "closing the buffer stranded {} bytes",
        quota::usage(uid, resource)
    );
    fresh()
}

/// Queueing a message charges its sender's uid for both depth and bytes,
/// the next message is refused at the depth limit, and delivery releases
/// the charge.
pub fn channel_queue_charge() -> Result<(), String> {
    fresh()?;
    let uid = 9090;
    credentials::set_current(Cred::new(uid, 0, 0, 0, 0));
    quota::set_limit(uid, Resource::QueueDepth, 1);
    let (client, server) = channels::create().map_err(channel_reason)?;
    let first = one_way()?;
    let bytes = first.len() as u64;
    channels::send(client, &first).map_err(channel_reason)?;
    check!(
        quota::usage(uid, Resource::QueueDepth) == 1,
        "the queued message was not charged to its sender's uid"
    );
    check!(
        quota::usage(uid, Resource::QueueBytes) == bytes,
        "queued bytes are {}, expected {bytes}",
        quota::usage(uid, Resource::QueueBytes)
    );
    check!(
        channels::send(client, &one_way()?) == Err(channels::Error::QuotaExceeded),
        "the per-uid queue depth limit was not enforced"
    );
    check!(
        quota::usage(uid, Resource::QueueDepth) == 1,
        "a refused send leaked a queue slot"
    );

    let message = channels::try_recv(server)
        .map_err(channel_reason)?
        .ok_or("the queued message vanished")?;
    check!(!message.bytes.is_empty(), "the delivered message is empty");
    check!(
        quota::usage(uid, Resource::QueueDepth) == 0,
        "delivery did not release the queue slot"
    );
    check!(
        quota::usage(uid, Resource::QueueBytes) == 0,
        "delivery did not release the queued bytes"
    );

    channels::close_endpoint(client).map_err(channel_reason)?;
    channels::close_endpoint(server).map_err(channel_reason)?;
    handles::reset_for_task(task::current());
    credentials::reset_for_task(task::current());
    quota::reset();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("quota_charge_release_accounting", charge_release_accounting),
    ("quota_denial_friendly_error", denial_friendly_error),
    ("quota_release_restores_headroom", release_restores_headroom),
    ("quota_per_uid_aggregation", per_uid_aggregation),
    ("quota_syscall_introspection", syscall_introspection),
    ("quota_shared_buffer_charge", shared_buffer_charge),
    ("quota_channel_queue_charge", channel_queue_charge),
];
