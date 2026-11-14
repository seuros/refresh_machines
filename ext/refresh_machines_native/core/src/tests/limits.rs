//! The ends of time, of the id space and of zero-sized tables.
use periscope_machines::SEQUENCE_MAX;

use super::{MAX, NEAR, RULES, Small, fetch, lease_of, near_the_end, revalidation, std, t};
use crate::*;
use core::assert_matches;

#[test]
fn duration_limits_saturate_instead_of_wrapping() {
    let mut refresher = Small::new();
    // An age that would overflow saturates and reads as expired, not fresh.
    assert_eq!(
        refresher.put(1, t(0), NEAR, Policy::new(MAX, t(0), t(0))),
        Ok(())
    );
    assert_eq!(refresher.state(1, t(0)), Some(State::Fresh));
    assert_eq!(refresher.age(1, t(5)), Some(MAX));
    assert_eq!(refresher.state(1, t(5)), Some(State::Expired));

    // A deadline past the end of time never comes early.
    let Ok(Decision::Fetch { lease }) = refresher.decide(2, t(10), MAX) else {
        panic!("leads");
    };
    assert_matches!(refresher.decide(2, NEAR, t(1)), Ok(Decision::Wait { .. }));
    assert_eq!(
        refresher.store(lease, NEAR, t(0), Policy::new(t(1), MAX, MAX)),
        Ok(())
    );
    let (age, lead) = revalidation(refresher.decide(2, MAX, MAX));
    let Revalidation::Lead(lease) = lead else {
        panic!("leads, got {lead:?}");
    };
    assert_eq!(age, t(1));
    // A backoff past the end of time saturates too.
    assert_eq!(refresher.fail(lease, MAX, MAX), Ok(Some(t(1))));
    assert_eq!(refresher.now(), MAX);
    assert_matches!(
        revalidation(refresher.decide(2, MAX, t(1))),
        (age, Revalidation::Lead(_)) if age == t(1)
    );
}

#[test]
fn a_spent_id_space_refuses_without_mutating() {
    // Exactly the two ids the one refresh below takes: its lease and its flight.
    let mut refresher: Small = near_the_end(0, 2);
    assert_eq!(refresher.put(1, t(0), t(0), RULES), Ok(()));
    let Ok(Decision::Fetch { lease: overdue }) = refresher.decide(2, t(0), t(1)) else {
        panic!("leads");
    };
    assert_eq!(refresher.issued(), SEQUENCE_MAX);
    let before = refresher.clone();
    // A new key, the takeover of an overdue lease and a wait all need an id.
    assert_eq!(refresher.decide(3, t(20), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher.decide(2, t(20), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher.decide(2, t(0), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher, before);
    // A servable stale value is still served; only its refresh is unavailable.
    assert_eq!(
        revalidation(refresher.decide(1, t(12), t(100))),
        (t(12), Revalidation::Unavailable(Refusal::Exhausted))
    );
    // Honoured, so it records the clock it depended on, and only the clock.
    let mut advanced = before;
    assert_eq!(advanced.put(9, t(12), t(0), RULES), Ok(()));
    assert_eq!(advanced.forget(9), Forgetting::Forgotten);
    assert_eq!(refresher, advanced);
    assert_eq!(refresher.leading(2), Some(overdue));
    // Neither the far end of another scope nor the flight's id names a live lease.
    for raw in [u64::MAX, SEQUENCE_MAX] {
        assert_eq!(
            refresher.store(lease_of(raw), t(12), t(0), RULES),
            Err(Refusal::Stale)
        );
    }
    // What is registered still reports.
    assert_eq!(overdue.raw(), SEQUENCE_MAX - 1);
    assert_eq!(refresher.store(overdue, t(12), t(0), RULES), Ok(()));
}

#[test]
fn full_refresh_slots_are_reported_before_a_spent_id_space() {
    // Two refreshes, two ids each: the sequence ends as the slots fill.
    let mut refresher: Small = near_the_end(0, 4);
    assert_eq!(refresher.put(1, t(0), t(0), RULES), Ok(()));
    let first = fetch(&mut refresher, 2, 0);
    fetch(&mut refresher, 3, 0);
    let before = refresher.clone();
    // No refresh slot and no id: the full table is what the caller hears,
    // as it is from the coalescer, where the sequence is checked last.
    assert_eq!(refresher.decide(4, t(0), t(100)), Err(Refusal::FlightsFull));
    assert_eq!(refresher, before);
    assert_eq!(
        revalidation(refresher.decide(1, t(12), t(100))),
        (t(12), Revalidation::Unavailable(Refusal::FlightsFull))
    );
    // Waiting takes an id from the same sequence, so it is refused too.
    let before = refresher.clone();
    assert_eq!(refresher.decide(2, t(12), t(100)), Err(Refusal::Exhausted));
    // With a slot free again, the spent sequence is what is left to report.
    assert_eq!(refresher, before);
    assert_eq!(refresher.abandon(first), Ok(()));
    let before = refresher.clone();
    assert_eq!(refresher.decide(4, t(12), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher, before);
    assert_eq!(
        revalidation(refresher.decide(1, t(12), t(100))),
        (t(12), Revalidation::Unavailable(Refusal::Exhausted))
    );
}

#[test]
fn a_new_refresh_needs_two_ids_and_takes_neither_when_one_is_left() {
    let mut refresher: Small = near_the_end(5, 3);
    let Ok(Decision::Fetch { lease: overdue }) = refresher.decide(1, t(0), t(1)) else {
        panic!("leads");
    };
    assert_eq!(refresher.issued(), SEQUENCE_MAX - 1);
    // One id left, a refresh slot free: a lease without a flight is no refresh.
    let before = refresher.clone();
    assert_eq!(refresher.decide(2, t(0), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher, before);
    // The id was not consumed: a takeover, which needs only a lease, gets it.
    let heir = fetch(&mut refresher, 1, 5);
    assert_eq!(heir.raw(), periscope_machines::scoped(5, SEQUENCE_MAX));
    assert_eq!(
        refresher.flight(heir).map(FlightId::raw),
        Some(overdue.raw() + 1)
    );
    assert_eq!(refresher.issued(), SEQUENCE_MAX);
    let before = refresher.clone();
    assert_eq!(refresher.decide(1, t(5), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher.decide(1, t(200), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher, before);
}

#[test]
fn zero_capacity_refuses_everything() {
    let mut refresher = Refresher::<0, 0, 0>::default();
    assert_eq!(refresher.decide(1, t(0), t(1)), Err(Refusal::FlightsFull));
    assert_eq!(
        refresher.put(1, t(0), t(0), RULES),
        Err(Refusal::EntriesFull)
    );
    assert_eq!(
        (
            refresher.capacity(),
            refresher.flight_capacity(),
            refresher.waiter_capacity()
        ),
        (0, 0, 0)
    );
    let roomy = Small::new();
    assert_eq!((roomy.flight_capacity(), roomy.waiter_capacity()), (2, 3));
    assert_eq!(
        std::format!("{}", Refusal::ZeroLead),
        "lead duration must be positive"
    );
}
