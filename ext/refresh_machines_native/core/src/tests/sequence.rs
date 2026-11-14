//! One sequence per refresher: a lease, a waiter id and a flight id are never
//! equal, and a restart resumes the sequence instead of starting it again.
use matryona_machines::testing::refuses;
use periscope_machines::{SEQUENCE_MAX, scoped};

use super::model::{Draws, scope_for, seeded};
use super::std::collections::BTreeSet;
use super::std::vec::Vec;
use super::{As, Small, fetch, lease_of, names_nothing, t, wait, waiter_of};
use crate::*;

/// The review's sequence: with leases and waiter ids on separate counters, a
/// lease cancelled another key's waiter and a waiter id abandoned another
/// key's refresh.
#[test]
fn regression_a_lease_and_a_waiter_id_never_name_each_other() {
    let mut refresher = Refresher::<4, 4, 4>::with_scope(2);
    let l1 = fetch(&mut refresher, 1, 0);
    let waiter = wait(&mut refresher, 1, 0);
    let l2 = fetch(&mut refresher, 2, 0);
    // One sequence: lease, its flight, the waiter, the next lease.
    assert_eq!(
        (l1.raw(), waiter.raw(), l2.raw()),
        (scoped(2, 1), scoped(2, 3), scoped(2, 4))
    );
    assert_ne!(l2.raw(), waiter.raw());

    refuses(&mut refresher, Refusal::Stale, |r| {
        r.cancel(waiter_of(l2.raw()))
    });
    refuses(&mut refresher, Refusal::Stale, |r| {
        r.abandon(lease_of(waiter.raw()))
    });

    // No id of one kind works as another, through any method.
    let flights = [l1, l2].map(|lease| refresher.flight(lease).expect("live"));
    names_nothing(&refresher, waiter.raw(), As::Lease);
    for lease in [l1, l2] {
        names_nothing(&refresher, lease.raw(), As::Waiter);
    }
    for flight in flights {
        names_nothing(&refresher, flight.raw(), As::Either);
    }

    // Key 1's waiter and key 2's refresh are both still there.
    assert_eq!((refresher.waiting(), refresher.in_flight()), (1, 2));
    assert_eq!(refresher.leading(2), Some(l2));
    assert_eq!(refresher.abandon(l2), Ok(()));
    assert_eq!(refresher.cancel(waiter), Ok(()));
}

#[test]
fn no_lease_ever_equals_a_waiter_or_flight_id_live_or_past() {
    type Core = Refresher<3, 3, 4>;
    for seed in 1..=24_u64 {
        let mut rng = seeded(seed);
        let scope = scope_for(seed, 11);
        let mut refresher = Core::with_scope(scope);
        let (mut leases, mut others) = (BTreeSet::new(), BTreeSet::new());
        let mut live: Vec<Lease> = Vec::new();
        let mut now = 0;
        for _ in 0..2_000 {
            now += rng.below(3);
            let key = rng.below(5);
            let pick = rng.index(live.len().max(1));
            let lease = live.get(pick).copied();
            let issued = refresher.issued();
            let led = match rng.below(8) {
                0..=3 => match refresher.decide(key, t(now), t(1 + rng.below(4))) {
                    Ok(
                        Decision::Fetch { lease }
                        | Decision::Stale {
                            revalidation: Revalidation::Lead(lease),
                            ..
                        },
                    ) => Some(lease),
                    Ok(Decision::Wait { waiter, flight }) => {
                        assert!(others.insert(waiter.raw()), "waiter id reissued");
                        assert!(others.contains(&flight.raw()), "an unknown flight");
                        None
                    }
                    Ok(Decision::Fresh { .. } | Decision::Stale { .. }) | Err(_) => None,
                },
                4 => {
                    let rules = Policy::new(t(rng.below(3)), t(rng.below(4)), t(rng.below(4)));
                    let _ = lease.map(|lease| refresher.store(lease, t(now), t(0), rules));
                    None
                }
                5 => {
                    let _ = lease.map(|lease| refresher.fail(lease, t(now), t(rng.below(3))));
                    None
                }
                6 => {
                    let _ = refresher.reap(t(now));
                    None
                }
                _ => {
                    let _ = refresher.poll();
                    None
                }
            };
            if let Some(lease) = led {
                assert!(leases.insert(lease.raw()), "lease reissued");
                // A new refresh brings a new flight; a takeover keeps the old one.
                let flight = refresher.flight(lease).expect("just issued");
                let took_over = refresher.issued() == issued + 1;
                assert_eq!(others.insert(flight.raw()), !took_over);
                live.push(lease);
            }
            live.retain(|lease| refresher.flight(*lease).is_some());
            // Every id is of exactly one kind, and the two sets are all of them.
            assert!(leases.is_disjoint(&others));
            assert_eq!((leases.len() + others.len()) as u64, refresher.issued());
            // A lease, live or long gone, is nobody's waiter; a waiter or
            // flight id, live or long gone, is nobody's lease.
            let nth = rng.index(16);
            if let Some(raw) = leases.iter().rev().nth(nth) {
                names_nothing(&refresher, *raw, As::Waiter);
            }
            if let Some(raw) = others.iter().rev().nth(nth) {
                names_nothing(&refresher, *raw, As::Lease);
            }
        }
        assert!(
            leases.len() > 50 && others.len() > 50,
            "{} leases, {} waiter and flight ids",
            leases.len(),
            others.len()
        );
    }
}

#[test]
fn a_resumed_refresher_continues_the_sequence() {
    let mut first = Small::with_scope(6);
    let lease = fetch(&mut first, 7, 0);
    let waiter = wait(&mut first, 7, 0);
    let flight = first.flight(lease).expect("live");
    assert_eq!(first.issued(), 3);

    // The restart: empty, at time zero, the saved count, the same scope.
    let mut second = Small::resumed(6, first.issued()).expect("within the sequence");
    assert_eq!((second.scope(), second.issued()), (6, 3));
    assert_eq!(
        (second.len(), second.in_flight(), second.now()),
        (0, 0, t(0))
    );
    // The same key is refreshed and waited on again, under new ids.
    let again = fetch(&mut second, 7, 0);
    let follower = wait(&mut second, 7, 0);
    assert_eq!((again.raw(), follower.raw()), (scoped(6, 4), scoped(6, 6)));
    // What outlived the restart names nothing here.
    for raw in [lease.raw(), waiter.raw(), flight.raw()] {
        names_nothing(&second, raw, As::Either);
    }

    // A fresh refresher in the same scope would have honoured both.
    let mut naive = Small::with_scope(6);
    assert_eq!(fetch(&mut naive, 9, 0), lease);
    assert_eq!(wait(&mut naive, 9, 0), waiter);
    assert_eq!(naive.cancel(waiter), Ok(()));
    assert_eq!(naive.abandon(lease), Ok(()));

    // Resuming from nothing is a new refresher.
    assert_eq!(Small::resumed(6, 0), Some(Small::with_scope(6)));
    assert_eq!(Small::resumed(0, 0), Some(Small::new()));
}

#[test]
fn resumed_answers_none_exactly_when_the_issuer_does() {
    for issued in [0, 1, SEQUENCE_MAX, SEQUENCE_MAX + 1, u64::MAX] {
        for scope in [0, 1, u16::MAX] {
            let refresher = Small::resumed(scope, issued);
            let issuer = periscope_machines::Issuer::resumed(scope, issued);
            assert_eq!(refresher.is_none(), issuer.is_none());
            assert_eq!(refresher.is_none(), issued > SEQUENCE_MAX);
            if let Some(refresher) = refresher {
                assert_eq!((refresher.scope(), refresher.issued()), (scope, issued));
            }
        }
    }
}
