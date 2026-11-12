#![no_std]
#![doc = include_str!("../README.md")]

use core::fmt;

use coalesce_machines::{Coalescer, Joined};
pub use coalesce_machines::{FlightId, Notification, Outcome, WaiterId};

mod policy;

pub use policy::{Policy, State};

/// The right to refresh one key, and the token that reports how it went.
///
/// Issued by [`Refresher::decide`]. Never reissued: once the refresh is
/// reported, abandoned, reaped or taken over, the lease is
/// [`Refused::Stale`] forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lease(u64);

impl Lease {
    /// Rebuild a lease from [`Self::raw`], e.g. after it crossed an FFI or queue boundary.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The lease as a plain integer. Never zero for an issued lease.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Why a call changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refused {
    /// `now` is earlier than a time this refresher already accepted.
    TimeRegressed,
    /// A zero lead duration: the lease would be overdue before it was issued.
    Rejected,
    /// Every metadata slot holds another key.
    EntriesFull,
    /// Every refresh slot holds an unsettled refresh; no new key can be led.
    FlightsFull,
    /// Every waiter slot holds a waiting caller or an undrained notification.
    WaitersFull,
    /// The id space is spent; this refresher can register nothing more.
    Exhausted,
    /// The lease or waiter names nothing live.
    Stale,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TimeRegressed => "time moved backwards",
            Self::Rejected => "lead duration must be positive",
            Self::EntriesFull => "every metadata slot is in use",
            Self::FlightsFull => "every refresh slot is in use",
            Self::WaitersFull => "every waiter slot is in use",
            Self::Exhausted => "id space exhausted",
            Self::Stale => "id does not name a live lease or waiter",
        })
    }
}

impl core::error::Error for Refused {}

impl From<coalesce_machines::Refused> for Refused {
    fn from(refused: coalesce_machines::Refused) -> Self {
        match refused {
            coalesce_machines::Refused::FlightsFull => Self::FlightsFull,
            coalesce_machines::Refused::WaitersFull => Self::WaitersFull,
            coalesce_machines::Refused::Exhausted => Self::Exhausted,
            _ => Self::Stale,
        }
    }
}

/// What is happening about refreshing a stale value that is being served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revalidation {
    /// Nobody was refreshing this key. The caller now is: serve the stale
    /// value, refresh in the background, report with the lease.
    Lead(Lease),
    /// Another caller holds a live lease for this key. Just serve.
    InFlight,
    /// The last refresh failed and asked for quiet until this time. Just serve.
    Backoff {
        /// When the next caller will be asked to refresh again.
        until: u64,
    },
    /// A refresh is due but could not be registered. Just serve; the next
    /// caller tries again.
    Unavailable(Refused),
}

/// The answer to "may I serve what I have for this key?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Serve the cached value.
    Fresh {
        /// Its age at `now`.
        age: u64,
    },
    /// Serve the cached value, stale, and mind the revalidation.
    Stale {
        /// Its age at `now`.
        age: u64,
        /// Who refreshes it, if anyone.
        revalidation: Revalidation,
    },
    /// Nothing servable. The caller fetches and reports with the lease.
    Fetch {
        /// The right to refresh this key.
        lease: Lease,
    },
    /// Nothing servable and another caller is already fetching. Wait for a
    /// [`Notification`] carrying this id, then decide again.
    Wait {
        /// The caller's notification id.
        waiter: WaiterId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    key: u64,
    stored_at: u64,
    age: u64,
    policy: Policy,
    /// Failed refreshes stay quiet while `now < retry_at`; zero means none.
    retry_at: u64,
}

impl Entry {
    const fn age_at(&self, now: u64) -> u64 {
        self.age.saturating_add(now.saturating_sub(self.stored_at))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lead {
    lease: u64,
    key: u64,
    flight: FlightId,
    deadline: u64,
}

/// Bounded freshness metadata plus one refresh lease per key.
///
/// `KEYS` bounds the keys with stored metadata, `FLIGHTS` the refreshes in
/// progress, `WAITERS` the callers waiting on a refresh plus notifications
/// not yet drained.
///
/// The refresher stores no values, fetches nothing and evicts nothing. It
/// answers whether the value the caller holds may be served, and who refreshes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refresher<const KEYS: usize, const FLIGHTS: usize, const WAITERS: usize> {
    entries: [Option<Entry>; KEYS],
    leads: [Option<Lead>; FLIGHTS],
    flights: Coalescer<FLIGHTS, WAITERS>,
    leases: u64,
    now: u64,
}

impl<const KEYS: usize, const FLIGHTS: usize, const WAITERS: usize> Default
    for Refresher<KEYS, FLIGHTS, WAITERS>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<const KEYS: usize, const FLIGHTS: usize, const WAITERS: usize>
    Refresher<KEYS, FLIGHTS, WAITERS>
{
    /// Start with no metadata, nothing in flight, at time zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [None; KEYS],
            leads: [None; FLIGHTS],
            flights: Coalescer::new(),
            leases: 0,
            now: 0,
        }
    }

    /// Decide what a caller asking for `key` at `now` should do.
    ///
    /// A lease issued here must be reported within `lead_for`; after that the
    /// next caller that needs one takes the refresh over and the old lease
    /// goes stale. Callers already waiting keep waiting on the new leader.
    ///
    /// # Errors
    ///
    /// [`Refused::TimeRegressed`], [`Refused::Rejected`] for a zero
    /// `lead_for`, and, only when nothing is servable,
    /// [`Refused::FlightsFull`], [`Refused::WaitersFull`] or
    /// [`Refused::Exhausted`]. Nothing changes.
    pub fn decide(&mut self, key: u64, now: u64, lead_for: u64) -> Result<Decision, Refused> {
        self.check(now)?;
        if lead_for == 0 {
            return Err(Refused::Rejected);
        }
        let live = self
            .lead_slot(key)
            .and_then(|slot| self.leads[slot])
            .is_some_and(|lead| now < lead.deadline);
        let held = self
            .entry_slot(key)
            .and_then(|slot| self.entries[slot])
            .map(|entry| (entry.age_at(now), entry.policy, entry.retry_at));
        let decision = match held {
            Some((age, policy, _)) if policy.state(age) == State::Fresh => Decision::Fresh { age },
            Some((age, policy, retry_at))
                if policy.while_revalidate(age) || (policy.if_error(age) && now < retry_at) =>
            {
                let revalidation = if live {
                    Revalidation::InFlight
                } else if now < retry_at {
                    Revalidation::Backoff { until: retry_at }
                } else {
                    self.lead(key, now, lead_for)
                        .map_or_else(Revalidation::Unavailable, Revalidation::Lead)
                };
                Decision::Stale { age, revalidation }
            }
            _ if live => Decision::Wait {
                waiter: self.follow(key)?,
            },
            _ => Decision::Fetch {
                lease: self.lead(key, now, lead_for)?,
            },
        };
        self.now = now;
        Ok(decision)
    }

    /// Report a successful refresh: the value under the lease's key is new.
    ///
    /// `age` is how old the value already was when it arrived (zero when the
    /// origin just produced it). Clears any failure backoff, settles the
    /// refresh and queues a [`Outcome::Completed`] notification per waiter.
    ///
    /// # Errors
    ///
    /// [`Refused::TimeRegressed`], [`Refused::Stale`] when the lease is not
    /// live, [`Refused::EntriesFull`] when the key is new and no metadata slot
    /// is free (the lease stays live: [`Self::forget`] a key and retry, or
    /// [`Self::abandon`]). Nothing changes.
    pub fn store(
        &mut self,
        lease: Lease,
        now: u64,
        age: u64,
        policy: Policy,
    ) -> Result<(), Refused> {
        self.check(now)?;
        let (slot, lead) = self.held(lease).ok_or(Refused::Stale)?;
        let home = self.room_for(lead.key).ok_or(Refused::EntriesFull)?;
        self.flights.complete(lead.flight)?;
        self.leads[slot] = None;
        self.entries[home] = Some(Entry {
            key: lead.key,
            stored_at: now,
            age,
            policy,
            retry_at: 0,
        });
        self.now = now;
        Ok(())
    }

    /// Report a failed refresh, and learn whether the old value covers for it.
    ///
    /// Returns the age of the held value when its stale-if-error window is
    /// open at `now`, `None` when there is nothing to fall back on. While the
    /// value stays servable, callers are not asked to refresh again until
    /// `retry_after` has passed. Waiters get [`Outcome::Abandoned`] and decide again.
    ///
    /// # Errors
    ///
    /// [`Refused::TimeRegressed`], or [`Refused::Stale`] when the lease is not
    /// live. Nothing changes.
    pub fn fail(
        &mut self,
        lease: Lease,
        now: u64,
        retry_after: u64,
    ) -> Result<Option<u64>, Refused> {
        self.check(now)?;
        let (slot, lead) = self.held(lease).ok_or(Refused::Stale)?;
        self.flights.abandon(lead.flight)?;
        self.leads[slot] = None;
        self.now = now;
        let Some(entry) = self
            .entry_slot(lead.key)
            .and_then(|home| self.entries[home].as_mut())
        else {
            return Ok(None);
        };
        entry.retry_at = now.saturating_add(retry_after);
        let age = entry.age_at(now);
        Ok(entry.policy.if_error(age).then_some(age))
    }

    /// Give a lease back without an outcome, e.g. the fetch was cancelled.
    ///
    /// No failure is recorded and no backoff starts; the next caller that
    /// needs a refresh leads one. Waiters get [`Outcome::Abandoned`].
    ///
    /// # Errors
    ///
    /// [`Refused::Stale`] when the lease is not live. Nothing changes.
    pub fn abandon(&mut self, lease: Lease) -> Result<(), Refused> {
        let (slot, lead) = self.held(lease).ok_or(Refused::Stale)?;
        self.flights.abandon(lead.flight)?;
        self.leads[slot] = None;
        Ok(())
    }

    /// Revoke every lease whose deadline has passed, and return how many.
    ///
    /// [`Self::decide`] already takes an overdue refresh over when a caller
    /// needs one. This is for the keys nobody asks about again: their waiters
    /// get [`Outcome::Abandoned`] instead of waiting forever.
    ///
    /// # Errors
    ///
    /// [`Refused::TimeRegressed`]. Nothing changes.
    pub fn reap(&mut self, now: u64) -> Result<usize, Refused> {
        self.check(now)?;
        self.now = now;
        let mut revoked = 0;
        for slot in &mut self.leads {
            if let Some(lead) = slot.filter(|lead| now >= lead.deadline) {
                *slot = None;
                revoked += usize::from(self.flights.abandon(lead.flight).is_ok());
            }
        }
        Ok(revoked)
    }

    /// Record metadata for a value the caller stored without a lease.
    ///
    /// Replaces any metadata for `key` and clears its failure backoff. A
    /// refresh in flight for the key is untouched; its report lands later.
    ///
    /// # Errors
    ///
    /// [`Refused::TimeRegressed`], or [`Refused::EntriesFull`] when the key is
    /// new and no metadata slot is free. Nothing changes.
    pub fn put(&mut self, key: u64, now: u64, age: u64, policy: Policy) -> Result<(), Refused> {
        self.check(now)?;
        let home = self.room_for(key).ok_or(Refused::EntriesFull)?;
        self.entries[home] = Some(Entry {
            key,
            stored_at: now,
            age,
            policy,
            retry_at: 0,
        });
        self.now = now;
        Ok(())
    }

    /// Drop the metadata for `key`, e.g. because the cache evicted its value.
    ///
    /// Returns whether there was any. A refresh in flight is untouched.
    pub fn forget(&mut self, key: u64) -> bool {
        self.entry_slot(key)
            .is_some_and(|slot| self.entries[slot].take().is_some())
    }

    /// Drop metadata that is [`State::Expired`] at `now`, writing the keys to `out`.
    ///
    /// Writes `Some` into the first `n` entries, returns `n`, and stops when
    /// `out` is full: call again to continue. Keys with a refresh in flight
    /// are kept. The values themselves are the caller's to delete.
    ///
    /// # Errors
    ///
    /// [`Refused::TimeRegressed`]. Nothing changes.
    pub fn sweep(&mut self, now: u64, out: &mut [Option<u64>]) -> Result<usize, Refused> {
        self.check(now)?;
        self.now = now;
        let mut written = 0;
        for slot in 0..KEYS {
            if written == out.len() {
                break;
            }
            let Some(entry) = self.entries[slot] else {
                continue;
            };
            if entry.policy.state(entry.age_at(now)) == State::Expired
                && self.lead_slot(entry.key).is_none()
            {
                self.entries[slot] = None;
                out[written] = Some(entry.key);
                written += 1;
            }
        }
        Ok(written)
    }

    /// Withdraw a waiter, whether still waiting or notified but undrained.
    ///
    /// # Errors
    ///
    /// [`Refused::Stale`] when `waiter` is not registered. Nothing changes.
    pub fn cancel(&mut self, waiter: WaiterId) -> Result<(), Refused> {
        Ok(self.flights.cancel(waiter)?)
    }

    /// Take the oldest queued notification, freeing its waiter slot.
    pub fn poll(&mut self) -> Option<Notification> {
        self.flights.poll()
    }

    /// Move queued notifications into `out`, oldest first, and return how many.
    ///
    /// Resumable: whatever did not fit stays queued, in order.
    pub fn drain(&mut self, out: &mut [Option<Notification>]) -> usize {
        self.flights.drain(out)
    }

    /// Age of the value held for `key` at `now`, if there is metadata for it.
    #[must_use]
    pub fn age(&self, key: u64, now: u64) -> Option<u64> {
        Some(self.entries[self.entry_slot(key)?]?.age_at(now))
    }

    /// Fresh, stale or expired at `now`; `None` when the key has no metadata.
    #[must_use]
    pub fn state(&self, key: u64, now: u64) -> Option<State> {
        let entry = self.entries[self.entry_slot(key)?]?;
        Some(entry.policy.state(entry.age_at(now)))
    }

    /// Age of the held value if a refresh error at `now` may be answered with it.
    #[must_use]
    pub fn stale_if_error(&self, key: u64, now: u64) -> Option<u64> {
        let entry = self.entries[self.entry_slot(key)?]?;
        let age = entry.age_at(now);
        entry.policy.if_error(age).then_some(age)
    }

    /// The lease currently refreshing `key`, overdue or not.
    #[must_use]
    pub fn leading(&self, key: u64) -> Option<Lease> {
        Some(Lease(self.leads[self.lead_slot(key)?]?.lease))
    }

    /// The latest time this refresher accepted.
    #[must_use]
    pub const fn now(&self) -> u64 {
        self.now
    }

    /// Keys with metadata.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    /// Whether no key has metadata.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Refreshes not yet reported.
    #[must_use]
    pub const fn in_flight(&self) -> usize {
        self.flights.in_flight()
    }

    /// Callers waiting on a refresh.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.flights.waiting()
    }

    /// Notifications queued and not yet drained.
    #[must_use]
    pub const fn pending(&self) -> usize {
        self.flights.pending()
    }

    /// Maximum keys with metadata (`KEYS`).
    #[must_use]
    pub const fn capacity(&self) -> usize {
        KEYS
    }

    /// Maximum refreshes at once (`FLIGHTS`).
    #[must_use]
    pub const fn flight_capacity(&self) -> usize {
        FLIGHTS
    }

    /// Maximum waiting callers plus undrained notifications (`WAITERS`).
    #[must_use]
    pub const fn waiter_capacity(&self) -> usize {
        WAITERS
    }

    const fn check(&self, now: u64) -> Result<(), Refused> {
        if now < self.now {
            Err(Refused::TimeRegressed)
        } else {
            Ok(())
        }
    }

    fn entry_slot(&self, key: u64) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.key == key))
    }

    /// The slot holding `key`, else a free one.
    fn room_for(&self, key: u64) -> Option<usize> {
        self.entry_slot(key)
            .or_else(|| self.entries.iter().position(Option::is_none))
    }

    fn lead_slot(&self, key: u64) -> Option<usize> {
        self.leads
            .iter()
            .position(|lead| lead.is_some_and(|lead| lead.key == key))
    }

    fn held(&self, lease: Lease) -> Option<(usize, Lead)> {
        self.leads
            .iter()
            .enumerate()
            .find_map(|(slot, lead)| Some((slot, lead.filter(|lead| lead.lease == lease.0)?)))
    }

    /// Issue a lease for `key`: a new refresh, or a takeover of an overdue one.
    fn lead(&mut self, key: u64, now: u64, lead_for: u64) -> Result<Lease, Refused> {
        let lease = self.leases.checked_add(1).ok_or(Refused::Exhausted)?;
        let deadline = now.saturating_add(lead_for);
        if let Some(lead) = self
            .lead_slot(key)
            .and_then(|slot| self.leads[slot].as_mut())
        {
            lead.lease = lease;
            lead.deadline = deadline;
        } else {
            let slot = self
                .leads
                .iter()
                .position(Option::is_none)
                .ok_or(Refused::FlightsFull)?;
            let flight = match self.flights.join(key)? {
                Joined::Leader(flight) => flight,
                // Unreachable while every flight has a lead; undo and refuse.
                Joined::Follower { waiter, .. } => {
                    self.flights.cancel(waiter)?;
                    return Err(Refused::Stale);
                }
            };
            self.leads[slot] = Some(Lead {
                lease,
                key,
                flight,
                deadline,
            });
        }
        self.leases = lease;
        Ok(Lease(lease))
    }

    fn follow(&mut self, key: u64) -> Result<WaiterId, Refused> {
        match self.flights.join(key)? {
            Joined::Follower { waiter, .. } => Ok(waiter),
            // Unreachable while every lead has a flight; undo and refuse.
            Joined::Leader(flight) => {
                self.flights.abandon(flight)?;
                Err(Refused::Stale)
            }
        }
    }
}

#[cfg(test)]
mod tests;
