#![allow(clippy::expect_used, clippy::panic)]
extern crate std;

use super::*;
use core::assert_matches;

mod flight;
mod limits;
mod model;
mod policy;
mod random;
mod scope;
mod sequence;
mod sweep;

const MAX: Duration = Duration::MAX;
/// One second short of the end of time.
const NEAR: Duration = MAX.saturating_sub(t(1));
const RULES: Policy = Policy::new(t(10), t(5), t(20));

/// Test time in whole seconds since the epoch the test picked.
const fn t(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

type Small = Refresher<3, 2, 3>;

/// A lease rebuilt from an id the test knows has a nonzero sequence.
fn lease_of(raw: u64) -> Lease {
    Lease::from_raw(raw).expect("a nonzero sequence")
}

/// A waiter id rebuilt from an id the test knows has a nonzero sequence.
fn waiter_of(raw: u64) -> WaiterId {
    WaiterId::from_raw(raw).expect("a nonzero sequence")
}

fn fetch<const K: usize, const F: usize, const W: usize>(
    refresher: &mut Refresher<K, F, W>,
    key: u64,
    now: u64,
) -> Lease {
    match refresher.decide(key, t(now), t(100)) {
        Ok(Decision::Fetch { lease }) => lease,
        other => panic!("expected a fetch lease, got {other:?}"),
    }
}

fn wait<const K: usize, const F: usize, const W: usize>(
    refresher: &mut Refresher<K, F, W>,
    key: u64,
    now: u64,
) -> WaiterId {
    match refresher.decide(key, t(now), t(100)) {
        Ok(Decision::Wait { waiter, flight }) => {
            // The flight named is the one the key's live lease rides on.
            let lease = refresher.leading(key).expect("someone leads");
            assert_eq!(refresher.flight(lease), Some(flight));
            waiter
        }
        other => panic!("expected to wait, got {other:?}"),
    }
}

/// A refresher in `scope` with `left` ids still to give: the way to the end
/// of the sequence is the public `resumed`, the one a restart uses.
fn near_the_end<const K: usize, const F: usize, const W: usize>(
    scope: u16,
    left: u64,
) -> Refresher<K, F, W> {
    Refresher::resumed(scope, periscope_machines::SEQUENCE_MAX - left)
        .expect("short of the end is within the sequence")
}

/// Which methods [`names_nothing`] hands an id to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum As {
    /// `store`, `fail`, `abandon` and `flight`.
    Lease,
    /// `cancel`.
    Waiter,
    /// All of them.
    Either,
}

/// `raw` through the methods `role` names: all refuse (or answer `None`),
/// and the refresher is left as it was.
fn names_nothing<const K: usize, const F: usize, const W: usize>(
    refresher: &Refresher<K, F, W>,
    raw: u64,
    role: As,
) {
    let mut probe = refresher.clone();
    let now = refresher.now();
    if role != As::Waiter {
        let lease = lease_of(raw);
        assert_eq!(probe.store(lease, now, t(0), RULES), Err(Refusal::Stale));
        assert_eq!(probe.fail(lease, now, t(1)), Err(Refusal::Stale));
        assert_eq!(probe.abandon(lease), Err(Refusal::Stale));
        assert_eq!(probe.flight(lease), None);
    }
    if role != As::Lease {
        assert_eq!(
            probe.cancel(waiter_of(raw)),
            Err(Refusal::Stale),
            "id {raw:#x} cancelled a waiter"
        );
    }
    assert_eq!(probe, *refresher, "id {raw:#x} changed the refresher");
}

fn revalidation(decision: Result<Decision, Refusal>) -> (Duration, Revalidation) {
    match decision {
        Ok(Decision::Stale { age, revalidation }) => (age, revalidation),
        other => panic!("expected a stale value, got {other:?}"),
    }
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
    assert_eq!(refresher.store(lease, t(3), t(0), RULES), Ok(()));
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
        (note.waiter(), note.key(), note.outcome()),
        (first, 42, Outcome::Completed)
    );
    let mut out = [Notification::default(); 4];
    assert_eq!(refresher.drain(&mut out), 1);
    assert_eq!(out[0].waiter(), second);
    assert_eq!(
        refresher.decide(42, t(4), t(100)),
        Ok(Decision::Fresh { age: t(1) })
    );
    assert_eq!(
        refresher.store(lease, t(4), t(0), RULES),
        Err(Refusal::Stale)
    );
    assert_eq!(refresher.leading(42), None);
}

#[test]
fn stale_value_is_served_while_exactly_one_caller_revalidates() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(7, t(0), t(0), RULES), Ok(()));
    assert_eq!(
        refresher.decide(7, t(9), t(100)),
        Ok(Decision::Fresh { age: t(9) })
    );
    let (age, lead) = revalidation(refresher.decide(7, t(10), t(100)));
    let Revalidation::Lead(lease) = lead else {
        panic!("first stale caller leads, got {lead:?}");
    };
    assert_eq!(age, t(10));
    assert_eq!(
        revalidation(refresher.decide(7, t(12), t(100))),
        (t(12), Revalidation::InFlight)
    );
    assert_eq!(refresher.waiting(), 0);
    assert_eq!(refresher.store(lease, t(13), t(2), RULES), Ok(()));
    assert_eq!(
        refresher.decide(7, t(13), t(100)),
        Ok(Decision::Fresh { age: t(2) })
    );
    assert_eq!(refresher.age(7, t(20)), Some(t(9)));
    assert_eq!(refresher.state(7, t(21)), Some(State::Stale));
}

#[test]
fn past_the_revalidate_window_callers_block_and_errors_fall_back_to_stale() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(7, t(0), t(0), RULES), Ok(()));
    // Age 15: stale-while-revalidate is over, stale-if-error still open.
    assert_eq!(refresher.stale_if_error(7, t(15)), Some(t(15)));
    let lease = fetch(&mut refresher, 7, 15);
    let waiter = wait(&mut refresher, 7, 16);
    assert_eq!(refresher.fail(lease, t(17), t(5)), Ok(Some(t(17))));
    let note = refresher.poll().expect("waiter woken");
    assert_eq!(
        (note.waiter(), note.outcome()),
        (waiter, Outcome::Abandoned)
    );
    // Until the backoff ends everybody is served stale and nobody refetches.
    assert_eq!(
        revalidation(refresher.decide(7, t(21), t(100))),
        (t(21), Revalidation::Backoff { until: t(22) })
    );
    assert_eq!(refresher.in_flight(), 0);
    // Backoff over: fetch again. A failure past the window has no fallback.
    let lease = fetch(&mut refresher, 7, 22);
    assert_eq!(refresher.fail(lease, t(30), t(5)), Ok(None));
    assert_eq!(refresher.stale_if_error(7, t(30)), None);
    assert_eq!(refresher.state(7, t(30)), Some(State::Expired));
    // An expired value is never served, backoff or not.
    let lease = fetch(&mut refresher, 7, 31);
    assert_eq!(refresher.store(lease, t(31), t(0), RULES), Ok(()));
    assert_eq!(refresher.decide(7, t(41), t(100)).map(|_| ()), Ok(()));
    assert_eq!(
        refresher.in_flight(),
        1,
        "a successful store cleared the backoff"
    );
}

#[test]
fn failure_inside_the_revalidate_window_backs_off_without_stale_if_error() {
    let mut refresher = Small::new();
    assert_eq!(
        refresher.put(1, t(0), t(0), Policy::new(t(10), t(5), t(0))),
        Ok(())
    );
    let (_, lead) = revalidation(refresher.decide(1, t(10), t(100)));
    let Revalidation::Lead(lease) = lead else {
        panic!("leads, got {lead:?}");
    };
    assert_eq!(refresher.fail(lease, t(11), t(2)), Ok(None));
    assert_eq!(
        revalidation(refresher.decide(1, t(12), t(100))),
        (t(12), Revalidation::Backoff { until: t(13) })
    );
    assert_matches!(
        revalidation(refresher.decide(1, t(13), t(100))),
        (age, Revalidation::Lead(_)) if age == t(13)
    );
    // A miss that fails records nothing.
    let lease = fetch(&mut refresher, 2, 13);
    assert_eq!(refresher.fail(lease, t(13), t(50)), Ok(None));
    assert_eq!(refresher.state(2, t(13)), None);
    assert_matches!(
        refresher.decide(2, t(13), t(100)),
        Ok(Decision::Fetch { .. })
    );
}

#[test]
fn abandon_records_no_failure_and_the_next_caller_leads() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(7, t(0), t(0), RULES), Ok(()));
    let lease = fetch(&mut refresher, 7, 20);
    let waiter = wait(&mut refresher, 7, 20);
    assert_eq!(refresher.abandon(lease), Ok(()));
    assert_eq!(refresher.abandon(lease), Err(Refusal::Stale));
    assert_eq!(refresher.fail(lease, t(20), t(1)), Err(Refusal::Stale));
    assert_eq!(refresher.poll().map(Notification::waiter), Some(waiter));
    assert_ne!(fetch(&mut refresher, 7, 20), lease);
}

#[test]
fn overdue_lease_is_taken_over_and_waiters_follow_the_new_leader() {
    let mut refresher = Small::new();
    let Ok(Decision::Fetch { lease: dead }) = refresher.decide(5, t(0), t(10)) else {
        panic!("leads");
    };
    let waiter = wait(&mut refresher, 5, 9);
    // At the deadline the next caller leads instead of waiting.
    let Ok(Decision::Fetch { lease }) = refresher.decide(5, t(10), t(10)) else {
        panic!("takes over");
    };
    assert_ne!(lease, dead);
    assert_eq!(refresher.leading(5), Some(lease));
    assert_eq!((refresher.in_flight(), refresher.waiting()), (1, 1));
    assert_eq!(
        refresher.store(dead, t(11), t(0), RULES),
        Err(Refusal::Stale)
    );
    assert_eq!(refresher.fail(dead, t(11), t(0)), Err(Refusal::Stale));
    assert_eq!(refresher.abandon(dead), Err(Refusal::Stale));
    assert_eq!(refresher.store(lease, t(12), t(0), RULES), Ok(()));
    let note = refresher.poll().expect("waiter kept across the takeover");
    assert_eq!(
        (note.waiter(), note.outcome()),
        (waiter, Outcome::Completed)
    );

    // The same for a background revalidation.
    let (_, lead) = revalidation(refresher.decide(5, t(22), t(3)));
    let Revalidation::Lead(dead) = lead else {
        panic!("leads, got {lead:?}");
    };
    assert_eq!(
        revalidation(refresher.decide(5, t(24), t(3))).1,
        Revalidation::InFlight
    );
    let (_, lead) = revalidation(refresher.decide(5, t(25), t(3)));
    assert_matches!(lead, Revalidation::Lead(lease) if lease != dead);
}

#[test]
fn reap_revokes_overdue_leases_and_wakes_their_waiters() {
    let mut refresher = Small::new();
    let Ok(Decision::Fetch { lease: slow }) = refresher.decide(1, t(0), t(5)) else {
        panic!("leads");
    };
    let Ok(Decision::Fetch { lease: patient }) = refresher.decide(2, t(0), t(50)) else {
        panic!("leads");
    };
    let waiter = wait(&mut refresher, 1, 1);
    assert_eq!(refresher.reap(t(4)), Ok(0));
    assert_eq!(refresher.reap(t(5)), Ok(1));
    assert_eq!(
        refresher.store(slow, t(5), t(0), RULES),
        Err(Refusal::Stale)
    );
    let note = refresher.poll().expect("waiter woken");
    assert_eq!(
        (note.waiter(), note.outcome()),
        (waiter, Outcome::Abandoned)
    );
    assert_eq!(refresher.leading(2), Some(patient));
    assert_eq!(refresher.reap(t(4)), Err(Refusal::TimeRegressed));
    // An overdue lease nobody reaped or replaced still reports.
    assert_eq!(refresher.store(patient, t(90), t(0), RULES), Ok(()));
}

#[test]
fn capacity_refusals_change_nothing() {
    let mut refresher = Small::new();
    let one = fetch(&mut refresher, 1, 0);
    let two = fetch(&mut refresher, 2, 0);
    let before = refresher.clone();
    assert_eq!(refresher.decide(3, t(0), t(100)), Err(Refusal::FlightsFull));
    assert_eq!(refresher, before);

    for _ in 0..3 {
        wait(&mut refresher, 1, 0);
    }
    let before = refresher.clone();
    assert_eq!(refresher.decide(2, t(5), t(100)), Err(Refusal::WaitersFull));
    assert_eq!(refresher.decide(1, t(5), t(0)), Err(Refusal::ZeroLead));
    assert_eq!(refresher, before);
    assert_eq!(refresher.now(), t(0));

    assert_eq!(refresher.store(one, t(0), t(0), RULES), Ok(()));
    assert_eq!(refresher.put(8, t(0), t(0), RULES), Ok(()));
    assert_eq!(refresher.put(9, t(0), t(0), RULES), Ok(()));
    assert_eq!((refresher.len(), refresher.capacity()), (3, 3));
    let before = refresher.clone();
    assert_eq!(
        refresher.put(10, t(0), t(0), RULES),
        Err(Refusal::EntriesFull)
    );
    assert_eq!(
        refresher.store(two, t(0), t(0), RULES),
        Err(Refusal::EntriesFull)
    );
    assert_eq!(refresher, before);
    // The lease survived the refusal: make room and report again.
    assert_eq!(refresher.forget(9), Forgetting::Forgotten);
    let before = refresher.clone();
    assert_eq!(refresher.forget(9), Forgetting::NotHeld);
    assert_eq!(refresher, before);
    assert_eq!(refresher.store(two, t(0), t(0), RULES), Ok(()));
    // Replacing a known key needs no free slot.
    assert_eq!(refresher.put(8, t(0), t(3), RULES), Ok(()));
    assert_eq!(refresher.age(8, t(0)), Some(t(3)));
}

#[test]
fn stale_value_is_still_served_when_the_revalidation_cannot_be_registered() {
    let mut refresher = Small::new();
    assert_eq!(refresher.put(7, t(0), t(0), RULES), Ok(()));
    fetch(&mut refresher, 1, 0);
    fetch(&mut refresher, 2, 0);
    assert_eq!(
        revalidation(refresher.decide(7, t(11), t(100))),
        (t(11), Revalidation::Unavailable(Refusal::FlightsFull))
    );
    assert_eq!(refresher.now(), t(11));
    // Honoured, yet the registration that failed left nothing behind: with the
    // clock already there, the same answer leaves the whole value as it was.
    let before = refresher.clone();
    assert_eq!(
        revalidation(refresher.decide(7, t(11), t(100))),
        (t(11), Revalidation::Unavailable(Refusal::FlightsFull))
    );
    assert_eq!(refresher, before);
    // Past the window there is nothing to serve, so the refusal is the answer.
    assert_eq!(
        refresher.decide(7, t(15), t(100)),
        Err(Refusal::FlightsFull)
    );
    assert_eq!(refresher.now(), t(11));
}

#[test]
fn time_regression_is_refused_everywhere_and_changes_nothing() {
    let mut refresher = Small::new();
    let lease = fetch(&mut refresher, 1, 10);
    let before = refresher.clone();
    let mut out = [77; 2];
    assert_eq!(
        refresher.decide(1, t(9), t(100)),
        Err(Refusal::TimeRegressed)
    );
    assert_eq!(
        refresher.store(lease, t(9), t(0), RULES),
        Err(Refusal::TimeRegressed)
    );
    assert_eq!(
        refresher.fail(lease, t(9), t(0)),
        Err(Refusal::TimeRegressed)
    );
    assert_eq!(
        refresher.put(2, t(9), t(0), RULES),
        Err(Refusal::TimeRegressed)
    );
    assert_eq!(refresher.reap(t(9)), Err(Refusal::TimeRegressed));
    assert_eq!(refresher.sweep(t(9), &mut out), Err(Refusal::TimeRegressed));
    assert_eq!(out, [77; 2]);
    assert_eq!(refresher, before);
    assert_eq!(refresher.store(lease, t(10), t(0), RULES), Ok(()));
    // Queries are pure: an earlier time reads the age the value was stored with.
    assert_eq!(refresher.age(1, t(3)), Some(t(0)));
}

#[test]
fn waiters_can_cancel_before_and_after_the_refresh_settles() {
    let mut refresher = Small::new();
    let lease = fetch(&mut refresher, 1, 0);
    let early = wait(&mut refresher, 1, 0);
    let late = wait(&mut refresher, 1, 0);
    assert_eq!(refresher.cancel(early), Ok(()));
    assert_eq!(refresher.cancel(early), Err(Refusal::Stale));
    assert_eq!(refresher.abandon(lease), Ok(()));
    assert_eq!(refresher.pending(), 1);
    assert_eq!(refresher.cancel(late), Ok(()));
    assert_eq!(refresher.poll(), None);
    // Zero is no id at all: it cannot even be rebuilt as one.
    assert_eq!((Lease::from_raw(0), WaiterId::from_raw(0)), (None, None));
}
