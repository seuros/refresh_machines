//! Random operation streams checked against the naive model.
use super::model::*;
use super::std::vec::Vec;
use crate::*;

/// The random inputs one step may use.
#[derive(Clone, Copy)]
struct Draw {
    key: u64,
    now: u64,
    lease: u64,
    policy: [u64; 3],
    age: u64,
    regressed: bool,
}

fn draw(rng: &mut Rng, model: &Model) -> Draw {
    // Mostly forward, sometimes still, occasionally backwards.
    let now = match rng.below(12) {
        0 => model.now.saturating_sub(1 + rng.below(3)),
        1..=4 => model.now,
        _ => model.now + rng.below(5),
    };
    let live: Vec<u64> = model.flying.values().map(|f| f.lease).collect();
    let lease = if live.is_empty() || rng.below(4) == 0 {
        rng.below(model.leases + 2)
    } else {
        live[usize::try_from(rng.below(live.len() as u64)).expect("index")]
    };
    Draw {
        key: rng.below(6),
        now,
        lease,
        policy: [rng.below(6), rng.below(6), rng.below(8)],
        age: rng.below(4),
        regressed: now < model.now,
    }
}

fn step(rng: &mut Rng, refresher: &mut Core, model: &mut Model) {
    let d = draw(rng, model);
    match rng.below(20) {
        0..=7 => {
            let lead_for = rng.below(7);
            let expect = model.decide(d.key, d.now, lead_for);
            let found = refresher.decide(d.key, t(d.now), t(lead_for));
            let found = observe(found, refresher, model, d.key);
            assert_eq!(found, expect, "decide({}, {}, {lead_for})", d.key, d.now);
        }
        8..=10 => store(refresher, model, &d),
        11..=12 => fail(rng, refresher, model, &d),
        13 => {
            let expect = model
                .settle(d.lease, Outcome::Abandoned)
                .map(|_| ())
                .ok_or(Refusal::Stale);
            assert_eq!(refresher.abandon(Lease(d.lease)), expect);
        }
        14 => reap(refresher, model, &d),
        15 => put(refresher, model, &d),
        16 => {
            let expected = match model.held.remove(&d.key) {
                Some(_) => Forgetting::Forgotten,
                None => Forgetting::NotHeld,
            };
            assert_eq!(refresher.forget(d.key), expected);
        }
        17 => sweep(rng, refresher, model, &d),
        18 => cancel(rng, refresher, model),
        _ => drain(rng, refresher, model),
    }
}

fn store(refresher: &mut Core, model: &mut Model, d: &Draw) {
    let Draw {
        now,
        lease,
        policy,
        age,
        regressed,
        ..
    } = *d;
    let expect = if regressed {
        Err(Refusal::TimeRegressed)
    } else {
        match model.flying.iter().find(|(_, f)| f.lease == lease) {
            None => Err(Refusal::Stale),
            Some((key, _)) if !model.room(*key) => Err(Refusal::EntriesFull),
            Some(_) => Ok(()),
        }
    };
    if expect.is_ok() {
        let key = model.settle(lease, Outcome::Completed).expect("live lease");
        model.held.insert(
            key,
            Held {
                stored_at: now,
                age,
                policy,
                retry_at: 0,
            },
        );
        model.now = now;
    }
    assert_eq!(
        refresher.store(Lease(lease), t(now), t(age), rules(policy)),
        expect
    );
}

fn fail(rng: &mut Rng, refresher: &mut Core, model: &mut Model, d: &Draw) {
    let Draw {
        now,
        lease,
        regressed,
        ..
    } = *d;
    let retry_after = rng.below(6);
    let live = model.flying.values().any(|f| f.lease == lease);
    let expect = if regressed {
        Err(Refusal::TimeRegressed)
    } else if live {
        let key = model.settle(lease, Outcome::Abandoned).expect("live lease");
        model.now = now;
        Ok(model.held.get_mut(&key).and_then(|h| {
            h.retry_at = now + retry_after;
            h.sie(now)
                .then(|| u64::try_from(h.age_at(now)).expect("small ages"))
        }))
    } else {
        Err(Refusal::Stale)
    };
    assert_eq!(
        refresher.fail(Lease(lease), t(now), t(retry_after)),
        expect.map(|age| age.map(t))
    );
}

fn reap(refresher: &mut Core, model: &mut Model, d: &Draw) {
    let Draw { now, regressed, .. } = *d;
    let expect = if regressed {
        Err(Refusal::TimeRegressed)
    } else {
        let overdue: Vec<u64> = model
            .flying
            .values()
            .filter(|f| now >= f.deadline)
            .map(|f| f.lease)
            .collect();
        // Settlement order across keys is the core's slot order, which
        // the model does not track; compare the wake-ups as a set below.
        model.now = now;
        Ok(overdue)
    };
    let found = refresher.reap(t(now));
    assert_eq!(found, expect.as_ref().map(Vec::len).map_err(|r| *r));
    if let Ok(overdue) = expect {
        let before = model.ready.len();
        for lease in overdue {
            model.settle(lease, Outcome::Abandoned);
        }
        reorder(refresher, model, before);
    }
}

fn put(refresher: &mut Core, model: &mut Model, d: &Draw) {
    let Draw {
        key,
        now,
        policy,
        age,
        regressed,
        ..
    } = *d;
    let expect = if regressed {
        Err(Refusal::TimeRegressed)
    } else if model.room(key) {
        model.held.insert(
            key,
            Held {
                stored_at: now,
                age,
                policy,
                retry_at: 0,
            },
        );
        model.now = now;
        Ok(())
    } else {
        Err(Refusal::EntriesFull)
    };
    assert_eq!(refresher.put(key, t(now), t(age), rules(policy)), expect);
}

fn sweep(rng: &mut Rng, refresher: &mut Core, model: &mut Model, d: &Draw) {
    let Draw { now, regressed, .. } = *d;
    let mut out = [u64::MAX; 2];
    let room = usize::try_from(rng.below(3)).expect("index");
    let found = refresher.sweep(t(now), &mut out[..room]);
    if regressed {
        assert_eq!(found, Err(Refusal::TimeRegressed));
        return;
    }
    let gone = |h: &Held| !h.fresh(now) && !h.swr(now) && !h.sie(now);
    let eligible: Vec<u64> = model
        .held
        .iter()
        .filter(|(key, h)| gone(h) && !model.flying.contains_key(key))
        .map(|(key, _)| *key)
        .collect();
    let written = eligible.len().min(room);
    assert_eq!(found, Ok(written));
    assert!(out[written..].iter().all(|key| *key == u64::MAX));
    for key in &out[..written] {
        assert!(eligible.contains(key), "swept a live key {key}");
        assert!(model.held.remove(key).is_some(), "swept {key} twice");
    }
    model.now = now;
}

fn cancel(rng: &mut Rng, refresher: &mut Core, model: &mut Model) {
    let known = model.seen.len() as u64;
    let waiter = if known == 0 || rng.below(5) == 0 {
        WaiterId::from_raw(rng.next())
    } else {
        model.seen[usize::try_from(rng.below(known)).expect("index")]
    };
    let queued = model.ready.iter().position(|(w, _, _)| *w == waiter);
    let expect = if let Some(index) = queued {
        model.ready.remove(index);
        Ok(())
    } else {
        model
            .flying
            .values_mut()
            .find_map(|f| {
                let index = f.followers.iter().position(|w| *w == waiter)?;
                f.followers.remove(index);
                Some(())
            })
            .ok_or(Refusal::Stale)
    };
    assert_eq!(refresher.cancel(waiter), expect);
}

fn drain(rng: &mut Rng, refresher: &mut Core, model: &mut Model) {
    let mut out = [Notification::default(); 2];
    let room = usize::try_from(rng.below(3)).expect("index");
    let written = refresher.drain(&mut out[..room]);
    assert_eq!(written, model.ready.len().min(room));
    assert!(
        out[written..]
            .iter()
            .all(|note| *note == Notification::default())
    );
    for note in out.iter().take(written) {
        let expect = model.ready.pop_front().expect("model has it queued");
        assert_eq!((note.waiter, note.key, note.outcome), expect);
        assert_eq!(model.told.get(&note.waiter), Some(&note.flight));
    }
}

/// After a reap, bring the model's newly queued wake-ups into the core's order.
///
/// Within one key the order is join order in both; only the order of the keys
/// reaped together is unspecified, so peek at it through a clone.
fn reorder(refresher: &Core, model: &mut Model, before: usize) {
    let mut peek = refresher.clone();
    let mut actual = Vec::new();
    while let Some(note) = peek.poll() {
        actual.push((note.waiter, note.key, note.outcome));
    }
    let mut fresh: Vec<_> = model.ready.drain(before..).collect();
    let mut sorted = actual[before.min(actual.len())..].to_vec();
    fresh.sort_by_key(|(waiter, ..)| *waiter);
    sorted.sort_by_key(|(waiter, ..)| *waiter);
    assert_eq!(fresh, sorted, "reap woke a different set of waiters");
    model.ready.extend(actual.into_iter().skip(before));
}

#[test]
fn random_operations_agree_with_a_naive_model() {
    for seed in 1..=40_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut refresher = Refresher::<KEYS, FLIGHTS, WAITERS>::new();
        let mut model = Model::default();
        for _ in 0..3_000 {
            step(&mut rng, &mut refresher, &mut model);
            let probe = model.now + rng.below(4);
            agree(&refresher, &model, probe);
        }
    }
}
