//! Scoped ids: what one refresher issued names nothing in another.
use coalesce_machines::{Coalescer, Joined};
use periscope_machines::{SEQUENCE_MAX, scoped, sequence_of};

use super::{As, RULES, Small, fetch, names_nothing, near_the_end, t, wait};
use crate::*;

/// The review's sequence: identical calls on two instances, then each is
/// handed the other's ids through every method that takes one.
#[test]
fn regression_a_lease_or_waiter_from_another_refresher_names_nothing_here() {
    let mut near = Small::with_scope(1);
    let mut far = Small::with_scope(2);
    assert_eq!((near.scope(), far.scope(), Small::new().scope()), (1, 2, 0));

    // The same calls on both: without scopes these ids would be equal.
    let (near_lease, far_lease) = (fetch(&mut near, 7, 0), fetch(&mut far, 7, 0));
    let (near_waiter, far_waiter) = (wait(&mut near, 7, 1), wait(&mut far, 7, 1));
    assert_eq!((near_lease.scope(), near_waiter.scope()), (1, 1));
    assert_eq!((far_lease.scope(), far_waiter.scope()), (2, 2));
    assert_eq!(sequence_of(near_lease.raw()), sequence_of(far_lease.raw()));
    assert_eq!(
        sequence_of(near_waiter.raw()),
        sequence_of(far_waiter.raw())
    );
    assert_ne!(near_lease, far_lease);
    assert_ne!(near_waiter, far_waiter);

    // Each refuses the other's ids and is left unchanged.
    for (here, lease, waiter) in [
        (&near, far_lease, far_waiter),
        (&far, near_lease, near_waiter),
    ] {
        names_nothing(here, lease.raw(), As::Lease);
        names_nothing(here, waiter.raw(), As::Waiter);
    }

    // Their own ids still work.
    assert_eq!(near.store(near_lease, t(2), t(0), RULES), Ok(()));
    assert_eq!(near.poll().map(Notification::waiter), Some(near_waiter));
    assert_eq!(far.cancel(far_waiter), Ok(()));
    assert_eq!(far.abandon(far_lease), Ok(()));
}

#[test]
fn refreshers_left_in_the_same_scope_still_alias() {
    // Scopes are the caller's job: nothing tells these two apart.
    let mut one = Small::with_scope(5);
    let mut other = Small::with_scope(5);
    let lease = fetch(&mut one, 7, 0);
    assert_eq!(lease, fetch(&mut other, 8, 0));
    let waiter = wait(&mut one, 7, 1);
    assert_eq!(waiter, wait(&mut other, 8, 1));
    // Ids from `one` cancel and settle the unrelated refresh in `other`.
    assert_eq!(other.cancel(waiter), Ok(()));
    assert_eq!(other.abandon(lease), Ok(()));
    assert_eq!((one.in_flight(), one.waiting()), (1, 1));
}

#[test]
fn raw_values_are_the_published_layout() {
    // Scope zero is one plain counter for every kind: a refresh takes a
    // lease then its flight, a wait takes a waiter id.
    let mut plain = Small::new();
    let first = fetch(&mut plain, 7, 0);
    assert_eq!(first.raw(), 1);
    assert_eq!(plain.flight(first).map(FlightId::raw), Some(2));
    assert_eq!(wait(&mut plain, 7, 0).raw(), 3);
    let second = fetch(&mut plain, 8, 0);
    assert_eq!(second.raw(), 4);
    assert_eq!(plain.flight(second).map(FlightId::raw), Some(5));
    assert_eq!(plain.issued(), 5);

    // A scope sits in the top 16 bits above the same counter.
    let mut top = Small::with_scope(u16::MAX);
    let lease = fetch(&mut top, 7, 0);
    assert_eq!(lease.raw(), (0xFFFF << 48) | 1);
    assert_eq!(lease.raw(), scoped(u16::MAX, 1));
    assert_eq!(wait(&mut top, 7, 0).raw(), (0xFFFF << 48) | 3);
    let mut three = Small::with_scope(3);
    assert_eq!(fetch(&mut three, 7, 0).raw(), 0x0003_0000_0000_0001);
}

#[test]
fn a_takeover_issues_the_next_lease_for_the_same_flight() {
    let mut refresher = Small::with_scope(4);
    let Ok(Decision::Fetch { lease: dead }) = refresher.decide(7, t(0), t(10)) else {
        panic!("leads");
    };
    let flight = refresher.flight(dead).expect("live");
    let waiter = wait(&mut refresher, 7, 9);
    // Overdue but not replaced: still the lease that reports, still mapped.
    assert_eq!(refresher.flight(dead), Some(flight));
    let heir = fetch(&mut refresher, 7, 10);
    // The flight took the id between them, the waiter the next; the
    // takeover took only a lease.
    assert_eq!((dead.raw(), heir.raw()), (scoped(4, 1), scoped(4, 4)));
    assert_eq!(flight.raw(), scoped(4, 2));
    assert_eq!(refresher.issued(), 4);
    assert_eq!(refresher.flight(heir), Some(flight));
    // The dead lease was issued here, provably, and is stale all the same.
    names_nothing(&refresher, dead.raw(), As::Lease);
    // The waiter is woken with the flight it was told, under the new lease.
    assert_eq!(refresher.fail(heir, t(11), t(0)), Ok(None));
    let note = refresher.poll().expect("waiter kept across the takeover");
    assert_eq!(
        (note.waiter(), note.flight(), note.outcome()),
        (waiter, flight, Outcome::Abandoned)
    );
    assert_eq!(refresher.flight(heir), None);
}

#[test]
fn ended_or_never_issued_ids_of_this_scope_are_stale() {
    // Repeats stay stale because nothing records how a lease ended.
    let mut refresher = Small::with_scope(6);
    let stored = fetch(&mut refresher, 7, 0);
    let gone = wait(&mut refresher, 7, 0);
    assert_eq!(refresher.cancel(gone), Ok(()));
    assert_eq!(refresher.store(stored, t(1), t(0), RULES), Ok(()));
    let failed = fetch(&mut refresher, 8, 1);
    assert_eq!(refresher.fail(failed, t(1), t(5)), Ok(None));
    let dropped = fetch(&mut refresher, 9, 1);
    assert_eq!(refresher.abandon(dropped), Ok(()));
    let live = fetch(&mut refresher, 10, 1);
    for lease in [stored, failed, dropped] {
        names_nothing(&refresher, lease.raw(), As::Lease);
    }
    names_nothing(&refresher, gone.raw(), As::Waiter);
    // Ids of this scope the sequence has not reached yet.
    assert_eq!(Lease::from_raw(scoped(6, 0)), None);
    for sequence in [refresher.issued() + 1, SEQUENCE_MAX] {
        names_nothing(&refresher, scoped(6, sequence), As::Either);
    }
    assert_eq!(refresher.abandon(live), Ok(()));
}

#[test]
fn a_bare_coalescer_in_another_scope_cannot_cancel_a_refresh_waiter() {
    let mut refresher = Small::with_scope(1);
    let mut direct = Coalescer::<2, 3>::new();
    fetch(&mut refresher, 7, 0);
    let waiter = wait(&mut refresher, 7, 1);
    // The same three ids drawn from a coalescer directly: a reserved one
    // where the refresher drew its lease, then the flight, then the waiter.
    assert_eq!(direct.reserve(), Ok(1));
    assert_eq!(direct.join(7).map(|_| ()), Ok(()));
    let Ok(Joined::Follower { waiter: twin, .. }) = direct.join(7) else {
        panic!("the second join follows");
    };

    // Same sequence number, different scope: not the same id.
    assert_eq!(twin.raw(), 3);
    assert_eq!(waiter.raw(), scoped(1, 3));
    let before = refresher.clone();
    assert_eq!(refresher.cancel(twin), Err(Refusal::Stale));
    assert_eq!(refresher, before);
    assert_eq!(
        direct.cancel(waiter),
        Err(coalesce_machines::Refusal::Stale)
    );
    assert_eq!(refresher.cancel(waiter), Ok(()));
}

#[test]
fn the_last_sequence_is_issued_as_a_flight_or_a_waiter_then_the_scope_is_spent() {
    // As a flight: the refresh that takes the last two ids.
    let mut refresher: Small = near_the_end(u16::MAX, 2);
    let lease = fetch(&mut refresher, 1, 0);
    assert_eq!(lease.raw(), u64::MAX - 1);
    assert_eq!(refresher.flight(lease).map(FlightId::raw), Some(u64::MAX));
    let before = refresher.clone();
    assert_eq!(refresher.decide(2, t(0), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher.decide(1, t(0), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher, before);

    // As a waiter id.
    let mut refresher: Small = near_the_end(u16::MAX, 3);
    fetch(&mut refresher, 1, 0);
    assert_eq!(wait(&mut refresher, 1, 0).raw(), u64::MAX);
    let before = refresher.clone();
    assert_eq!(refresher.decide(1, t(0), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher, before);
}
