//! Releasing an endpoint handle without ending the side (networking plan N2).
//!
//! An explicit close ends a channel side whoever else holds a handle to it; a
//! receiver that was handed somebody else's endpoint (a notify endpoint, found
//! by name) must be able to drop its handle without killing that service. The
//! `CLOSE_RELEASE` flag of `close_endpoint` does that: the side closes only
//! when no other handle names it.

use super::*;
use crate::ipc::handles::rights;

/// Releasing one of several handles to a side leaves the side open and usable;
/// releasing the last one closes it, exactly as a close would.
pub fn release_keeps_the_side_while_others_hold_it() -> Result<(), String> {
    fresh()?;
    let (a, b) = channels::create().map_err(reason)?;
    let b2 = handles::duplicate(b, rights::ALL).map_err(|e| String::from(e.message()))?;
    let hello = parcel(7, flags::ONE_WAY, "hello")?;

    channels::release_endpoint(b).map_err(reason)?;
    check!(
        handles::get(b).is_err(),
        "the released handle is still in the table"
    );
    channels::send(a, &hello).map_err(reason)?;
    let got = channels::recv(b2, None).map_err(reason)?;
    check!(
        payload(&got.bytes)? == "hello",
        "the surviving handle did not receive the message"
    );

    channels::release_endpoint(b2).map_err(reason)?;
    check!(
        channels::send(a, &hello) == Err(ChannelError::PeerDied),
        "releasing the last handle did not close the side"
    );
    check!(
        channels::recv(a, None) == Err(ChannelError::PeerDied),
        "the peer did not see the side close"
    );
    Ok(())
}

/// The contrast the flag exists for: an explicit close ends the side for every
/// holder, which is why a received handle must not be closed that way.
pub fn close_ends_the_side_for_everyone() -> Result<(), String> {
    fresh()?;
    let (a, b) = channels::create().map_err(reason)?;
    let b2 = handles::duplicate(b, rights::ALL).map_err(|e| String::from(e.message()))?;
    channels::close_endpoint(b).map_err(reason)?;
    check!(
        channels::send(a, &parcel(7, flags::ONE_WAY, "x")?) == Err(ChannelError::PeerDied),
        "an explicit close left the side open for the other handle"
    );
    // The other handle is a dead side now; it can still be dropped.
    check!(
        channels::release_endpoint(b2).is_ok(),
        "the surviving handle could not be released"
    );
    Ok(())
}

/// A release of a handle that is gone, was never an endpoint or never existed
/// fails cleanly and changes nothing.
pub fn release_refuses_what_it_cannot_release() -> Result<(), String> {
    fresh()?;
    let (a, b) = channels::create().map_err(reason)?;
    channels::release_endpoint(b).map_err(reason)?;
    check!(
        channels::release_endpoint(b).is_err(),
        "a handle was released twice"
    );
    check!(
        channels::release_endpoint(0xFFFF_FFFF).is_err(),
        "a handle that never existed was released"
    );
    check!(
        channels::release_endpoint(u64::MAX).is_err(),
        "a wild handle was released"
    );
    // `a` is untouched by all that, and now sees its peer gone.
    check!(
        channels::recv(a, None) == Err(ChannelError::PeerDied),
        "the peer of a released last handle is not dead"
    );
    Ok(())
}

/// Soak: many pairs with a shared handle released in both orders never leak a
/// handle or a channel, and the side closes exactly when the last handle goes.
pub fn release_soak() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    let baseline = handles::count_for_task(me);
    let live = channels::stats().queued;
    let hello = parcel(7, flags::ONE_WAY, "x")?;
    for round in 0..20_000u32 {
        let (a, b) = channels::create().map_err(reason)?;
        let b2 = handles::duplicate(b, rights::ALL).map_err(|e| String::from(e.message()))?;
        let (first, second) = if round % 2 == 0 { (b, b2) } else { (b2, b) };
        channels::release_endpoint(first).map_err(reason)?;
        check!(
            channels::send(a, &hello).is_ok(),
            "round {round}: the side closed with a holder left"
        );
        channels::release_endpoint(second).map_err(reason)?;
        check!(
            channels::send(a, &hello) == Err(ChannelError::PeerDied),
            "round {round}: the side stayed open with no holder"
        );
        channels::close_endpoint(a).map_err(reason)?;
    }
    check!(
        handles::count_for_task(me) == baseline,
        "{} handles leaked",
        handles::count_for_task(me) - baseline
    );
    check!(
        channels::stats().queued == live,
        "messages were left queued"
    );
    Ok(())
}
