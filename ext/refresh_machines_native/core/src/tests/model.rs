//! Randomized comparison against a naive, allocating model.
use super::std::collections::{BTreeMap, VecDeque};
use super::std::vec::Vec;

use core::time::Duration;

use crate::*;

/// The model counts whole ticks; the core is handed the same instants and
/// spans as `Duration`s. A tick is not a whole second, so sums carry.
pub(super) const fn t(ticks: u64) -> Duration {
    Duration::from_millis(ticks * 400)
}

/// The core policy for a model policy of `[fresh_for, swr, sie]` ticks.
pub(super) const fn rules(policy: [u64; 3]) -> Policy {
    Policy::new(t(policy[0]), t(policy[1]), t(policy[2]))
}

pub(super) struct Rng(pub(super) u64);

impl Rng {
    /// A stream for `seed`, never the all-zero state xorshift is stuck in.
    pub(super) const fn seeded(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub(super) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(super) fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// An index into something `len` long (`len > 0`).
    pub(super) fn index(&mut self, len: usize) -> usize {
        usize::try_from(self.below(len as u64)).expect("index")
    }

    /// One of `items` (non-empty).
    pub(super) fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.index(items.len())]
    }
}

/// The scope a seeded run uses: zero, a middling one or the last, by turns.
pub(super) fn scope_for(seed: u64, middle: u16) -> u16 {
    [0, middle, u16::MAX][usize::try_from(seed % 3).expect("index")]
}

#[derive(Clone, Copy)]
pub(super) struct Held {
    pub(super) stored_at: u64,
    pub(super) age: u64,
    pub(super) policy: [u64; 3],
    pub(super) retry_at: u64,
}

impl Held {
    /// Wide arithmetic instead of the core's saturating/subtracting forms.
    pub(super) fn age_at(&self, now: u64) -> u128 {
        u128::from(self.age) + u128::from(now - self.stored_at)
    }

    /// [`Self::age_at`] in ticks; the streams keep ages small.
    pub(super) fn ticks_at(&self, now: u64) -> u64 {
        u64::try_from(self.age_at(now)).expect("small ages")
    }

    pub(super) fn fresh(&self, now: u64) -> bool {
        self.age_at(now) < u128::from(self.policy[0])
    }

    pub(super) fn within(&self, now: u64, window: u64) -> bool {
        let end = u128::from(self.policy[0]) + u128::from(window);
        !self.fresh(now) && self.age_at(now) < end
    }

    pub(super) fn swr(&self, now: u64) -> bool {
        self.within(now, self.policy[1])
    }

    pub(super) fn sie(&self, now: u64) -> bool {
        self.within(now, self.policy[2])
    }
}

pub(super) struct Flying {
    pub(super) lease: u64,
    pub(super) deadline: u64,
    pub(super) followers: Vec<WaiterId>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Expect {
    Fresh(Duration),
    Stale(Duration, Revalidation),
    Fetch(u64),
    Wait,
    Refusal(Refusal),
}

#[derive(Default)]
pub(super) struct Model {
    pub(super) held: BTreeMap<u64, Held>,
    pub(super) flying: BTreeMap<u64, Flying>,
    pub(super) ready: VecDeque<(WaiterId, u64, Outcome)>,
    pub(super) seen: Vec<WaiterId>,
    /// The flight each waiter was told it shares, checked when it is woken.
    pub(super) told: BTreeMap<u64, FlightId>,
    /// Ids issued so far, of every kind: one sequence for the refresher.
    pub(super) issued: u64,
    pub(super) scope: u16,
    pub(super) now: u64,
}

pub(super) type Core = Refresher<KEYS, FLIGHTS, WAITERS>;

pub(super) const KEYS: usize = 3;
pub(super) const FLIGHTS: usize = 2;
pub(super) const WAITERS: usize = 3;

impl Model {
    /// The id of a sequence number, spelled out here rather than borrowed
    /// from the crate under test: scope in the top 16 bits, sequence below.
    pub(super) fn id(&self, sequence: u64) -> u64 {
        (u64::from(self.scope) << 48) | sequence
    }

    pub(super) fn waiters(&self) -> usize {
        self.flying
            .values()
            .map(|f| f.followers.len())
            .sum::<usize>()
            + self.ready.len()
    }

    pub(super) fn lead(&mut self, key: u64, now: u64, lead_for: u64) -> Result<u64, Refusal> {
        if !self.flying.contains_key(&key) && self.flying.len() == FLIGHTS {
            return Err(Refusal::FlightsFull);
        }
        // A new refresh takes a lease and then a flight id; a takeover, the
        // flight already flying, takes only the lease.
        let lease = self.id(self.issued + 1);
        self.issued += if self.flying.contains_key(&key) { 1 } else { 2 };
        let flying = self.flying.entry(key).or_insert_with(|| Flying {
            lease: 0,
            deadline: 0,
            followers: Vec::new(),
        });
        flying.lease = lease;
        flying.deadline = now + lead_for;
        Ok(lease)
    }

    pub(super) fn decide(&mut self, key: u64, now: u64, lead_for: u64) -> Expect {
        if now < self.now {
            return Expect::Refusal(Refusal::TimeRegressed);
        }
        if lead_for == 0 {
            return Expect::Refusal(Refusal::ZeroLead);
        }
        let live = self.flying.get(&key).is_some_and(|f| now < f.deadline);
        let held = self.held.get(&key).copied();
        let age = held.map(|h| h.ticks_at(now));
        let quiet = held.is_some_and(|h| now < h.retry_at);
        let expect = match (held, age) {
            (Some(h), Some(age)) if h.fresh(now) => Expect::Fresh(t(age)),
            (Some(h), Some(age)) if h.swr(now) || (h.sie(now) && quiet) => {
                let revalidation = if live {
                    Revalidation::InFlight
                } else if quiet {
                    Revalidation::Backoff {
                        until: t(h.retry_at),
                    }
                } else {
                    match self.lead(key, now, lead_for) {
                        Ok(lease) => Revalidation::Lead(Lease(lease)),
                        Err(refused) => Revalidation::Unavailable(refused),
                    }
                };
                Expect::Stale(t(age), revalidation)
            }
            _ if live => {
                if self.waiters() == WAITERS {
                    return Expect::Refusal(Refusal::WaitersFull);
                }
                Expect::Wait
            }
            _ => match self.lead(key, now, lead_for) {
                Ok(lease) => Expect::Fetch(lease),
                Err(refused) => return Expect::Refusal(refused),
            },
        };
        self.now = now;
        expect
    }

    pub(super) fn settle(&mut self, lease: u64, outcome: Outcome) -> Option<u64> {
        let key = *self.flying.iter().find(|(_, f)| f.lease == lease)?.0;
        let flying = self.flying.remove(&key)?;
        self.ready
            .extend(flying.followers.into_iter().map(|w| (w, key, outcome)));
        Some(key)
    }

    /// Metadata stored at `now` without backoff, by `store` or `put`; both
    /// are honoured calls, so the clock advances.
    pub(super) fn hold(&mut self, key: u64, now: u64, age: u64, policy: [u64; 3]) {
        let held = Held {
            stored_at: now,
            age,
            policy,
            retry_at: 0,
        };
        self.held.insert(key, held);
        self.now = now;
    }

    pub(super) fn room(&self, key: u64) -> bool {
        self.held.contains_key(&key) || self.held.len() < KEYS
    }
}

pub(super) fn observe(
    found: Result<Decision, Refusal>,
    refresher: &Core,
    model: &mut Model,
    key: u64,
) -> Expect {
    match found {
        Ok(Decision::Fresh { age }) => Expect::Fresh(age),
        Ok(Decision::Stale { age, revalidation }) => Expect::Stale(age, revalidation),
        Ok(Decision::Fetch { lease }) => Expect::Fetch(lease.raw()),
        Ok(Decision::Wait { waiter, flight }) => {
            assert!(!model.seen.contains(&waiter), "waiter id reissued");
            model.issued += 1;
            assert_eq!(waiter.raw(), model.id(model.issued), "the next id");
            let lease = refresher.leading(key).expect("a wait has a leader");
            assert_eq!(
                refresher.flight(lease),
                Some(flight),
                "wait names the flight"
            );
            model.seen.push(waiter);
            model.told.insert(waiter.raw(), flight);
            if let Some(flying) = model.flying.get_mut(&key) {
                flying.followers.push(waiter);
            }
            Expect::Wait
        }
        Err(refused) => Expect::Refusal(refused),
    }
}

pub(super) fn agree(refresher: &Core, model: &Model, probe: u64) {
    assert_eq!(refresher.now(), t(model.now));
    assert_eq!(refresher.issued(), model.issued);
    assert_eq!(refresher.len(), model.held.len());
    assert_eq!(refresher.in_flight(), model.flying.len());
    assert_eq!(refresher.pending(), model.ready.len());
    assert_eq!(
        refresher.waiting() + refresher.pending(),
        model.waiters(),
        "waiter accounting"
    );
    for key in 0..6 {
        let held = model.held.get(&key);
        let age = held.map(|h| h.ticks_at(probe));
        assert_eq!(refresher.age(key, t(probe)), age.map(t));
        let state = held.map(|h| {
            if h.fresh(probe) {
                State::Fresh
            } else if h.swr(probe) || h.sie(probe) {
                State::Stale
            } else {
                State::Expired
            }
        });
        assert_eq!(refresher.state(key, t(probe)), state);
        let fallback = held.filter(|h| h.sie(probe)).and(age);
        assert_eq!(refresher.stale_if_error(key, t(probe)), fallback.map(t));
        let leading = model.flying.get(&key).map(|f| Lease(f.lease));
        assert_eq!(refresher.leading(key), leading);
    }
}
