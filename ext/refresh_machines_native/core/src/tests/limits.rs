//! The ends of time, of the id space and of zero-sized tables.
use super::{MAX, NEAR, RULES, Small, revalidation, std, t};
use crate::*;

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
    assert!(matches!(
        refresher.decide(2, NEAR, t(1)),
        Ok(Decision::Wait { .. })
    ));
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
    assert!(matches!(
        revalidation(refresher.decide(2, MAX, t(1))),
        (age, Revalidation::Lead(_)) if age == t(1)
    ));
}

#[test]
fn exhausted_lease_ids_refuse_without_mutating() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(1, t(0), t(0), RULES), Ok(()));
    let Ok(Decision::Fetch { lease: overdue }) = refresher.decide(2, t(0), t(1)) else {
        panic!("leads");
    };
    refresher.leases = u64::MAX;
    let before = refresher.clone();
    assert_eq!(refresher.decide(3, t(20), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher.decide(2, t(20), t(100)), Err(Refusal::Exhausted));
    assert_eq!(refresher, before);
    assert_eq!(
        revalidation(refresher.decide(1, t(12), t(100))),
        (t(12), Revalidation::Unavailable(Refusal::Exhausted))
    );
    assert_eq!(refresher.leading(2), Some(overdue));
    assert_eq!(
        refresher.store(Lease::from_raw(u64::MAX), t(12), t(0), RULES),
        Err(Refusal::Stale)
    );
    assert_eq!(Lease::from_raw(overdue.raw()), overdue);
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
        std::format!("{}", Refusal::Rejected),
        "lead duration must be positive"
    );
}
