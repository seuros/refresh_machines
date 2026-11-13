#![allow(clippy::expect_used, clippy::panic)]
extern crate std;

use super::*;

mod model;
mod random;
mod scope;

const MAX: u64 = u64::MAX;
const RULES: Policy = Policy::new(10, 5, 20);

type Small = Refresher<3, 2, 3>;

fn fetch<const K: usize, const F: usize, const W: usize>(
    refresher: &mut Refresher<K, F, W>,
    key: u64,
    now: u64,
) -> Lease {
    match refresher.decide(key, now, 100) {
        Ok(Decision::Fetch { lease }) => lease,
        other => panic!("expected a fetch lease, got {other:?}"),
    }
}

fn wait<const K: usize, const F: usize, const W: usize>(
    refresher: &mut Refresher<K, F, W>,
    key: u64,
    now: u64,
) -> WaiterId {
    match refresher.decide(key, now, 100) {
        Ok(Decision::Wait { waiter }) => waiter,
        other => panic!("expected to wait, got {other:?}"),
    }
}

fn revalidation(decision: Result<Decision, Refused>) -> (u64, Revalidation) {
    match decision {
        Ok(Decision::Stale { age, revalidation }) => (age, revalidation),
        other => panic!("expected a stale value, got {other:?}"),
    }
}

#[test]
fn policy_windows_are_half_open_and_start_when_freshness_ends() {
    assert_eq!(RULES.state(0), State::Fresh);
    assert_eq!(RULES.state(9), State::Fresh);
    assert_eq!(RULES.state(10), State::Stale);
    assert!(RULES.while_revalidate(10) && RULES.while_revalidate(14));
    assert!(!RULES.while_revalidate(9) && !RULES.while_revalidate(15));
    assert!(RULES.if_error(10) && RULES.if_error(29));
    assert!(!RULES.if_error(9) && !RULES.if_error(30));
    assert_eq!(RULES.state(29), State::Stale);
    assert_eq!(RULES.state(30), State::Expired);

    let strict = Policy::new(10, 0, 0);
    assert_eq!(strict.state(10), State::Expired);
    let never_fresh = Policy::new(0, 0, 0);
    assert_eq!(never_fresh.state(0), State::Expired);
    let forever = Policy::new(MAX, MAX, MAX);
    assert_eq!(forever.state(MAX - 1), State::Fresh);
    assert_eq!(forever.state(MAX), State::Stale);
    let wide = Policy::new(1, MAX, 0);
    assert!(wide.while_revalidate(MAX) && !wide.if_error(MAX));
}

#[test]
fn miss_is_fetched_once_and_waiters_are_notified_when_it_lands() {
    let mut refresher = Small::new();
    let lease = fetch(&mut refresher, 42, 0);
    assert_eq!(refresher.leading(42), Some(lease));
    let first = wait(&mut refresher, 42, 1);
    let second = wait(&mut refresher, 42, 2);
    assert_eq!(
        (
            refresher.in_flight(),
            refresher.waiting(),
            refresher.pending()
        ),
        (1, 2, 0)
    );
    assert_eq!(refresher.store(lease, 3, 0, RULES), Ok(()));
    assert_eq!(
        (
            refresher.in_flight(),
            refresher.waiting(),
            refresher.pending()
        ),
        (0, 0, 2)
    );
    let note = refresher.poll().expect("first waiter");
    assert_eq!(
        (note.waiter, note.key, note.outcome),
        (first, 42, Outcome::Completed)
    );
    let mut out = [None; 4];
    assert_eq!(refresher.drain(&mut out), 1);
    assert_eq!(out[0].map(|note| note.waiter), Some(second));
    assert_eq!(refresher.decide(42, 4, 100), Ok(Decision::Fresh { age: 1 }));
    assert_eq!(refresher.store(lease, 4, 0, RULES), Err(Refused::Stale));
    assert_eq!(refresher.leading(42), None);
}

#[test]
fn stale_value_is_served_while_exactly_one_caller_revalidates() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(7, 0, 0, RULES), Ok(()));
    assert_eq!(refresher.decide(7, 9, 100), Ok(Decision::Fresh { age: 9 }));
    let (age, lead) = revalidation(refresher.decide(7, 10, 100));
    let Revalidation::Lead(lease) = lead else {
        panic!("first stale caller leads, got {lead:?}");
    };
    assert_eq!(age, 10);
    assert_eq!(
        revalidation(refresher.decide(7, 12, 100)),
        (12, Revalidation::InFlight)
    );
    assert_eq!(refresher.waiting(), 0);
    assert_eq!(refresher.store(lease, 13, 2, RULES), Ok(()));
    assert_eq!(refresher.decide(7, 13, 100), Ok(Decision::Fresh { age: 2 }));
    assert_eq!(refresher.age(7, 20), Some(9));
    assert_eq!(refresher.state(7, 21), Some(State::Stale));
}

#[test]
fn past_the_revalidate_window_callers_block_and_errors_fall_back_to_stale() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(7, 0, 0, RULES), Ok(()));
    // Age 15: stale-while-revalidate is over, stale-if-error still open.
    assert_eq!(refresher.stale_if_error(7, 15), Some(15));
    let lease = fetch(&mut refresher, 7, 15);
    let waiter = wait(&mut refresher, 7, 16);
    assert_eq!(refresher.fail(lease, 17, 5), Ok(Some(17)));
    let note = refresher.poll().expect("waiter woken");
    assert_eq!((note.waiter, note.outcome), (waiter, Outcome::Abandoned));
    // Until the backoff ends everybody is served stale and nobody refetches.
    assert_eq!(
        revalidation(refresher.decide(7, 21, 100)),
        (21, Revalidation::Backoff { until: 22 })
    );
    assert_eq!(refresher.in_flight(), 0);
    // Backoff over: fetch again. A failure past the window has no fallback.
    let lease = fetch(&mut refresher, 7, 22);
    assert_eq!(refresher.fail(lease, 30, 5), Ok(None));
    assert_eq!(refresher.stale_if_error(7, 30), None);
    assert_eq!(refresher.state(7, 30), Some(State::Expired));
    // An expired value is never served, backoff or not.
    let lease = fetch(&mut refresher, 7, 31);
    assert_eq!(refresher.store(lease, 31, 0, RULES), Ok(()));
    assert_eq!(refresher.decide(7, 41, 100).map(|_| ()), Ok(()));
    assert_eq!(
        refresher.in_flight(),
        1,
        "a successful store cleared the backoff"
    );
}

#[test]
fn failure_inside_the_revalidate_window_backs_off_without_stale_if_error() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(1, 0, 0, Policy::new(10, 5, 0)), Ok(()));
    let (_, lead) = revalidation(refresher.decide(1, 10, 100));
    let Revalidation::Lead(lease) = lead else {
        panic!("leads, got {lead:?}");
    };
    assert_eq!(refresher.fail(lease, 11, 2), Ok(None));
    assert_eq!(
        revalidation(refresher.decide(1, 12, 100)),
        (12, Revalidation::Backoff { until: 13 })
    );
    assert!(matches!(
        revalidation(refresher.decide(1, 13, 100)),
        (13, Revalidation::Lead(_))
    ));
    // A miss that fails records nothing.
    let lease = fetch(&mut refresher, 2, 13);
    assert_eq!(refresher.fail(lease, 13, 50), Ok(None));
    assert_eq!(refresher.state(2, 13), None);
    assert!(matches!(
        refresher.decide(2, 13, 100),
        Ok(Decision::Fetch { .. })
    ));
}

#[test]
fn abandon_records_no_failure_and_the_next_caller_leads() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(7, 0, 0, RULES), Ok(()));
    let lease = fetch(&mut refresher, 7, 20);
    let waiter = wait(&mut refresher, 7, 20);
    assert_eq!(refresher.abandon(lease), Ok(()));
    assert_eq!(refresher.abandon(lease), Err(Refused::Stale));
    assert_eq!(refresher.fail(lease, 20, 1), Err(Refused::Stale));
    assert_eq!(refresher.poll().map(|note| note.waiter), Some(waiter));
    assert_ne!(fetch(&mut refresher, 7, 20), lease);
}

#[test]
fn overdue_lease_is_taken_over_and_waiters_follow_the_new_leader() {
    let mut refresher = Small::new();
    let Ok(Decision::Fetch { lease: dead }) = refresher.decide(5, 0, 10) else {
        panic!("leads");
    };
    let waiter = wait(&mut refresher, 5, 9);
    // At the deadline the next caller leads instead of waiting.
    let Ok(Decision::Fetch { lease }) = refresher.decide(5, 10, 10) else {
        panic!("takes over");
    };
    assert_ne!(lease, dead);
    assert_eq!(refresher.leading(5), Some(lease));
    assert_eq!((refresher.in_flight(), refresher.waiting()), (1, 1));
    assert_eq!(refresher.store(dead, 11, 0, RULES), Err(Refused::Stale));
    assert_eq!(refresher.fail(dead, 11, 0), Err(Refused::Stale));
    assert_eq!(refresher.abandon(dead), Err(Refused::Stale));
    assert_eq!(refresher.store(lease, 12, 0, RULES), Ok(()));
    let note = refresher.poll().expect("waiter kept across the takeover");
    assert_eq!((note.waiter, note.outcome), (waiter, Outcome::Completed));

    // The same for a background revalidation.
    let (_, lead) = revalidation(refresher.decide(5, 22, 3));
    let Revalidation::Lead(dead) = lead else {
        panic!("leads, got {lead:?}");
    };
    assert_eq!(
        revalidation(refresher.decide(5, 24, 3)).1,
        Revalidation::InFlight
    );
    let (_, lead) = revalidation(refresher.decide(5, 25, 3));
    assert!(matches!(lead, Revalidation::Lead(lease) if lease != dead));
}

#[test]
fn reap_revokes_overdue_leases_and_wakes_their_waiters() {
    let mut refresher = Small::new();
    let Ok(Decision::Fetch { lease: slow }) = refresher.decide(1, 0, 5) else {
        panic!("leads");
    };
    let Ok(Decision::Fetch { lease: patient }) = refresher.decide(2, 0, 50) else {
        panic!("leads");
    };
    let waiter = wait(&mut refresher, 1, 1);
    assert_eq!(refresher.reap(4), Ok(0));
    assert_eq!(refresher.reap(5), Ok(1));
    assert_eq!(refresher.store(slow, 5, 0, RULES), Err(Refused::Stale));
    let note = refresher.poll().expect("waiter woken");
    assert_eq!((note.waiter, note.outcome), (waiter, Outcome::Abandoned));
    assert_eq!(refresher.leading(2), Some(patient));
    assert_eq!(refresher.reap(4), Err(Refused::TimeRegressed));
    // An overdue lease nobody reaped or replaced still reports.
    assert_eq!(refresher.store(patient, 90, 0, RULES), Ok(()));
}

#[test]
fn capacity_refusals_change_nothing() {
    let mut refresher = Small::new();
    let one = fetch(&mut refresher, 1, 0);
    let two = fetch(&mut refresher, 2, 0);
    let before = refresher.clone();
    assert_eq!(refresher.decide(3, 0, 100), Err(Refused::FlightsFull));
    assert_eq!(refresher, before);

    for _ in 0..3 {
        wait(&mut refresher, 1, 0);
    }
    let before = refresher.clone();
    assert_eq!(refresher.decide(2, 5, 100), Err(Refused::WaitersFull));
    assert_eq!(refresher.decide(1, 5, 0), Err(Refused::Rejected));
    assert_eq!(refresher, before);
    assert_eq!(refresher.now(), 0);

    assert_eq!(refresher.store(one, 0, 0, RULES), Ok(()));
    assert_eq!(refresher.put(8, 0, 0, RULES), Ok(()));
    assert_eq!(refresher.put(9, 0, 0, RULES), Ok(()));
    assert_eq!((refresher.len(), refresher.capacity()), (3, 3));
    let before = refresher.clone();
    assert_eq!(refresher.put(10, 0, 0, RULES), Err(Refused::EntriesFull));
    assert_eq!(refresher.store(two, 0, 0, RULES), Err(Refused::EntriesFull));
    assert_eq!(refresher, before);
    // The lease survived the refusal: make room and report again.
    assert!(refresher.forget(9));
    assert!(!refresher.forget(9));
    assert_eq!(refresher.store(two, 0, 0, RULES), Ok(()));
    // Replacing a known key needs no free slot.
    assert_eq!(refresher.put(8, 0, 3, RULES), Ok(()));
    assert_eq!(refresher.age(8, 0), Some(3));
}

#[test]
fn stale_value_is_still_served_when_the_revalidation_cannot_be_registered() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(7, 0, 0, RULES), Ok(()));
    fetch(&mut refresher, 1, 0);
    fetch(&mut refresher, 2, 0);
    assert_eq!(
        revalidation(refresher.decide(7, 11, 100)),
        (11, Revalidation::Unavailable(Refused::FlightsFull))
    );
    assert_eq!(refresher.now(), 11);
    // Past the window there is nothing to serve, so the refusal is the answer.
    assert_eq!(refresher.decide(7, 15, 100), Err(Refused::FlightsFull));
    assert_eq!(refresher.now(), 11);
}

#[test]
fn time_regression_is_refused_everywhere_and_changes_nothing() {
    let mut refresher = Small::new();
    let lease = fetch(&mut refresher, 1, 10);
    let before = refresher.clone();
    let mut out = [None; 2];
    assert_eq!(refresher.decide(1, 9, 100), Err(Refused::TimeRegressed));
    assert_eq!(
        refresher.store(lease, 9, 0, RULES),
        Err(Refused::TimeRegressed)
    );
    assert_eq!(refresher.fail(lease, 9, 0), Err(Refused::TimeRegressed));
    assert_eq!(refresher.put(2, 9, 0, RULES), Err(Refused::TimeRegressed));
    assert_eq!(refresher.reap(9), Err(Refused::TimeRegressed));
    assert_eq!(refresher.sweep(9, &mut out), Err(Refused::TimeRegressed));
    assert_eq!(refresher, before);
    assert_eq!(refresher.store(lease, 10, 0, RULES), Ok(()));
    // Queries are pure: an earlier time reads the age the value was stored with.
    assert_eq!(refresher.age(1, 3), Some(0));
}

#[test]
fn sweep_drops_expired_metadata_in_bounded_resumable_batches() {
    let mut refresher = Refresher::<4, 2, 2>::new();
    for key in 0..3 {
        assert_eq!(refresher.put(key, 0, 0, Policy::new(1, 0, 0)), Ok(()));
    }
    assert_eq!(refresher.put(3, 0, 0, RULES), Ok(()));
    let busy = fetch(&mut refresher, 2, 5);
    let mut out = [None; 1];
    assert_eq!(refresher.sweep(5, &mut []), Ok(0));
    assert_eq!(refresher.sweep(5, &mut out), Ok(1));
    assert_eq!(out, [Some(0)]);
    let mut out = [None; 4];
    assert_eq!(refresher.sweep(5, &mut out), Ok(1));
    assert_eq!(out, [Some(1), None, None, None]);
    // Key 2 is expired but being refetched; key 3 is still fresh.
    assert_eq!(refresher.sweep(5, &mut out), Ok(0));
    assert_eq!(refresher.len(), 2);
    assert_eq!(refresher.abandon(busy), Ok(()));
    assert_eq!(refresher.sweep(5, &mut out), Ok(1));
    assert_eq!(out[0], Some(2));
    assert!(!refresher.is_empty());
}

#[test]
fn waiters_can_cancel_before_and_after_the_refresh_settles() {
    let mut refresher = Small::new();
    let lease = fetch(&mut refresher, 1, 0);
    let early = wait(&mut refresher, 1, 0);
    let late = wait(&mut refresher, 1, 0);
    assert_eq!(refresher.cancel(early), Ok(()));
    assert_eq!(refresher.cancel(early), Err(Refused::Stale));
    assert_eq!(refresher.abandon(lease), Ok(()));
    assert_eq!(refresher.pending(), 1);
    assert_eq!(refresher.cancel(late), Ok(()));
    assert_eq!(refresher.poll(), None);
    assert_eq!(refresher.cancel(WaiterId::from_raw(0)), Err(Refused::Stale));
}

#[test]
fn u64_limits_saturate_instead_of_wrapping() {
    let mut refresher = Small::new();
    // An age that would overflow saturates and reads as expired, not fresh.
    assert_eq!(refresher.put(1, 0, MAX - 1, Policy::new(MAX, 0, 0)), Ok(()));
    assert_eq!(refresher.state(1, 0), Some(State::Fresh));
    assert_eq!(refresher.age(1, 5), Some(MAX));
    assert_eq!(refresher.state(1, 5), Some(State::Expired));

    // A deadline past the end of time never comes early.
    let Ok(Decision::Fetch { lease }) = refresher.decide(2, 10, MAX) else {
        panic!("leads");
    };
    assert!(matches!(
        refresher.decide(2, MAX - 1, 1),
        Ok(Decision::Wait { .. })
    ));
    assert_eq!(
        refresher.store(lease, MAX - 1, 0, Policy::new(1, MAX, MAX)),
        Ok(())
    );
    let (age, lead) = revalidation(refresher.decide(2, MAX, MAX));
    let Revalidation::Lead(lease) = lead else {
        panic!("leads, got {lead:?}");
    };
    assert_eq!(age, 1);
    // A backoff past the end of time saturates too.
    assert_eq!(refresher.fail(lease, MAX, MAX), Ok(Some(1)));
    assert_eq!(refresher.now(), MAX);
    assert!(matches!(
        revalidation(refresher.decide(2, MAX, 1)),
        (1, Revalidation::Lead(_))
    ));
}

#[test]
fn exhausted_lease_ids_refuse_without_mutating() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(1, 0, 0, RULES), Ok(()));
    let Ok(Decision::Fetch { lease: overdue }) = refresher.decide(2, 0, 1) else {
        panic!("leads");
    };
    refresher.leases = MAX;
    let before = refresher.clone();
    assert_eq!(refresher.decide(3, 20, 100), Err(Refused::Exhausted));
    assert_eq!(refresher.decide(2, 20, 100), Err(Refused::Exhausted));
    assert_eq!(refresher, before);
    assert_eq!(
        revalidation(refresher.decide(1, 12, 100)),
        (12, Revalidation::Unavailable(Refused::Exhausted))
    );
    assert_eq!(refresher.leading(2), Some(overdue));
    assert_eq!(
        refresher.store(Lease::from_raw(MAX), 12, 0, RULES),
        Err(Refused::Stale)
    );
    assert_eq!(Lease::from_raw(overdue.raw()), overdue);
}

#[test]
fn zero_capacity_refuses_everything() {
    let mut refresher = Refresher::<0, 0, 0>::default();
    assert_eq!(refresher.decide(1, 0, 1), Err(Refused::FlightsFull));
    assert_eq!(refresher.put(1, 0, 0, RULES), Err(Refused::EntriesFull));
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
        std::format!("{}", Refused::Rejected),
        "lead duration must be positive"
    );
}
