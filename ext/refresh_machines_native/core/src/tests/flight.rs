//! Leases and the coalesce flights they ride on.
use coalesce_machines::scoped;

use super::{RULES, Small, fetch, revalidation, t, wait};
use crate::*;

#[test]
fn a_live_lease_names_the_flight_its_waiters_are_woken_with() {
    let mut refresher = Small::with_scope(3);
    let lease = fetch(&mut refresher, 42, 0);
    let other = fetch(&mut refresher, 43, 0);
    let flight = refresher.flight(lease).expect("a live lease has a flight");
    assert_eq!(flight.scope(), 3);
    assert_ne!(refresher.flight(other), Some(flight));
    let waiter = wait(&mut refresher, 42, 1);
    // Asking is pure.
    let before = refresher.clone();
    assert_eq!(refresher.flight(lease), Some(flight));
    assert_eq!(refresher, before);

    assert_eq!(refresher.store(lease, t(2), t(0), RULES), Ok(()));
    let mut out = [Notification::default(); 2];
    assert_eq!(refresher.drain(&mut out), 1);
    assert_eq!(
        out[0],
        Notification {
            waiter,
            flight,
            key: 42,
            outcome: Outcome::Completed,
        }
    );
    assert_eq!(out[1], Notification::default());
    // Reported: the lease is stale and names no flight any more.
    assert_eq!(refresher.flight(lease), None);
    assert!(refresher.flight(other).is_some());
}

#[test]
fn a_wait_names_the_flight_of_the_live_lease_it_shares() {
    let mut refresher = Small::with_scope(3);
    let Ok(Decision::Fetch { lease: dead }) = refresher.decide(5, t(0), t(10)) else {
        panic!("leads");
    };
    let other = fetch(&mut refresher, 6, 0);
    let flight = refresher.flight(dead).expect("live");
    let Ok(Decision::Wait {
        waiter: first,
        flight: shared,
    }) = refresher.decide(5, t(1), t(10))
    else {
        panic!("waits");
    };
    assert_eq!(shared, flight);
    assert_eq!(shared.scope(), 3);
    // Another key waits on another flight.
    let Ok(Decision::Wait { flight: apart, .. }) = refresher.decide(6, t(1), t(10)) else {
        panic!("waits");
    };
    assert_eq!(Some(apart), refresher.flight(other));
    assert_ne!(apart, shared);

    // After a takeover the flight is the same one, now under the new lease.
    let Ok(Decision::Fetch { lease: heir }) = refresher.decide(5, t(10), t(10)) else {
        panic!("takes over");
    };
    let Ok(Decision::Wait {
        waiter: second,
        flight: again,
    }) = refresher.decide(5, t(11), t(10))
    else {
        panic!("waits");
    };
    assert_ne!(second, first);
    assert_eq!(again, shared);
    assert_eq!(refresher.flight(heir), Some(again));
    assert_eq!(refresher.flight(dead), None);

    // Both are woken with the flight they were told.
    assert_eq!(refresher.store(heir, t(12), t(0), RULES), Ok(()));
    for waiter in [first, second] {
        let note = refresher.poll().expect("woken");
        assert_eq!((note.waiter, note.flight), (waiter, shared));
    }
}

#[test]
fn a_takeover_issues_a_new_lease_for_the_same_flight() {
    let mut refresher = Small::new();
    let Ok(Decision::Fetch { lease: dead }) = refresher.decide(5, t(0), t(10)) else {
        panic!("leads");
    };
    let flight = refresher.flight(dead).expect("live");
    let waiter = wait(&mut refresher, 5, 9);
    // Overdue but not replaced: still the lease that reports, still mapped.
    assert_eq!(refresher.flight(dead), Some(flight));
    let Ok(Decision::Fetch { lease: heir }) = refresher.decide(5, t(10), t(10)) else {
        panic!("takes over");
    };
    assert_ne!(heir, dead);
    assert_eq!(refresher.flight(dead), None);
    assert_eq!(refresher.flight(heir), Some(flight));
    assert_eq!(refresher.fail(heir, t(11), t(0)), Ok(None));
    let note = refresher.poll().expect("waiter kept across the takeover");
    assert_eq!(
        (note.waiter, note.flight, note.outcome),
        (waiter, flight, Outcome::Abandoned)
    );
    assert_eq!(refresher.flight(heir), None);
}

#[test]
fn every_way_a_lease_ends_also_ends_its_mapping() {
    let mut refresher = Small::new();
    let abandoned = fetch(&mut refresher, 1, 0);
    assert!(refresher.flight(abandoned).is_some());
    assert_eq!(refresher.abandon(abandoned), Ok(()));
    assert_eq!(refresher.flight(abandoned), None);

    let Ok(Decision::Fetch { lease: reaped }) = refresher.decide(1, t(0), t(5)) else {
        panic!("leads");
    };
    let first = refresher.flight(reaped).expect("live");
    assert_eq!(refresher.reap(t(5)), Ok(1));
    assert_eq!(refresher.flight(reaped), None);

    // A later refresh of the same key rides a new flight.
    let again = fetch(&mut refresher, 1, 6);
    let second = refresher.flight(again).expect("live");
    assert_ne!(second, first);
    assert_eq!(refresher.store(again, t(7), t(0), RULES), Ok(()));

    // A background revalidation has a flight like any other lease.
    let (_, lead) = revalidation(refresher.decide(1, t(18), t(100)));
    let Revalidation::Lead(lease) = lead else {
        panic!("leads, got {lead:?}");
    };
    assert!(
        refresher
            .flight(lease)
            .is_some_and(|flight| flight != second)
    );
}

#[test]
fn leases_that_were_never_issued_or_belong_elsewhere_name_no_flight() {
    let mut near = Small::with_scope(1);
    let mut far = Small::with_scope(2);
    let near_lease = fetch(&mut near, 7, 0);
    let far_lease = fetch(&mut far, 7, 0);
    assert_eq!(near.flight(far_lease), None);
    assert_eq!(far.flight(near_lease), None);
    for raw in [0, 1, scoped(1, 2), u64::MAX] {
        assert_eq!(near.flight(Lease::from_raw(raw)), None);
    }
    // A lease is not its flight: the two are counted separately.
    let flight = near.flight(near_lease).expect("live");
    assert_eq!(near.flight(Lease::from_raw(flight.raw())), Some(flight));
    assert_eq!(wait(&mut near, 7, 1).scope(), 1);
    let second = fetch(&mut near, 8, 1);
    let second_flight = near.flight(second).expect("live");
    assert_ne!(second.raw(), second_flight.raw());
    assert_eq!(near.flight(Lease::from_raw(second_flight.raw())), None);
}

#[test]
fn coalesce_refusals_convert() {
    for (from, to) in [
        (
            coalesce_machines::Refusal::FlightsFull,
            Refusal::FlightsFull,
        ),
        (
            coalesce_machines::Refusal::WaitersFull,
            Refusal::WaitersFull,
        ),
        (coalesce_machines::Refusal::Exhausted, Refusal::Exhausted),
        (coalesce_machines::Refusal::Stale, Refusal::Stale),
    ] {
        assert_eq!(Refusal::from(from), to);
    }
}
