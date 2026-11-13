//! Scoped ids: what one refresher issued names nothing in another.
use coalesce_machines::{Coalescer, Joined, scoped};

use super::{RULES, Small, fetch, wait};
use crate::*;

#[test]
fn scopes_keep_the_leases_and_waiters_of_separate_refreshers_apart() {
    let mut near = Small::with_scope(1);
    let mut far = Small::with_scope(2);
    assert_eq!((near.scope(), far.scope(), Small::new().scope()), (1, 2, 0));

    // The same calls on both: without scopes these ids would be equal.
    let (near_lease, far_lease) = (fetch(&mut near, 7, 0), fetch(&mut far, 7, 0));
    let (near_waiter, far_waiter) = (wait(&mut near, 7, 1), wait(&mut far, 7, 1));
    assert_eq!((near_lease.scope(), near_waiter.scope()), (1, 1));
    assert_eq!((far_lease.scope(), far_waiter.scope()), (2, 2));
    assert_eq!(near_lease.raw(), scoped(1, 1));
    assert_ne!(near_lease, far_lease);
    assert_ne!(near_waiter, far_waiter);

    // A foreign lease or waiter is stale here, and the refusal changes nothing.
    let before = near.clone();
    assert_eq!(near.store(far_lease, 2, 0, RULES), Err(Refused::Stale));
    assert_eq!(near.fail(far_lease, 2, 5), Err(Refused::Stale));
    assert_eq!(near.abandon(far_lease), Err(Refused::Stale));
    assert_eq!(near.cancel(far_waiter), Err(Refused::Stale));
    assert_eq!(near, before);
    assert_eq!(near.store(near_lease, 2, 0, RULES), Ok(()));
    assert_eq!(near.poll().map(|note| note.waiter), Some(near_waiter));
    assert_eq!(far.cancel(far_waiter), Ok(()));
}

#[test]
fn a_bare_coalescer_in_another_scope_cannot_cancel_a_refresh_waiter() {
    let mut refresher = Small::with_scope(1);
    let mut direct = Coalescer::<2, 3>::new();
    fetch(&mut refresher, 7, 0);
    let waiter = wait(&mut refresher, 7, 1);
    assert_eq!(direct.join(7).map(|_| ()), Ok(()));
    let Ok(Joined::Follower { waiter: twin, .. }) = direct.join(7) else {
        panic!("the second join follows");
    };

    // Same joins, same sequence number, different scope: not the same id.
    assert_eq!(twin.raw(), 2);
    assert_eq!(waiter.raw(), scoped(1, 2));
    let before = refresher.clone();
    assert_eq!(refresher.cancel(twin), Err(Refused::Stale));
    assert_eq!(refresher, before);
    assert_eq!(
        direct.cancel(waiter),
        Err(coalesce_machines::Refused::Stale)
    );
    assert_eq!(refresher.cancel(waiter), Ok(()));
}

#[test]
fn the_last_lease_sequence_is_issued_then_the_scope_is_spent() {
    let mut refresher = Small::with_scope(u16::MAX);
    refresher.leases = coalesce_machines::SEQUENCE_MAX - 1;
    assert_eq!(fetch(&mut refresher, 1, 0).raw(), u64::MAX);
    let before = refresher.clone();
    assert_eq!(refresher.decide(2, 0, 100), Err(Refused::Exhausted));
    assert_eq!(refresher, before);
}
