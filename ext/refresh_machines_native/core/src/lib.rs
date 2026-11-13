#![no_std]
#![doc = include_str!("../README.md")]

use core::time::Duration;

use coalesce_machines::{Coalescer, Joined};
pub use coalesce_machines::{FlightId, Notification, Outcome, WaiterId};

mod decision;
mod lease;
mod policy;
mod refusal;

pub use decision::{Decision, Forgetting, Revalidation};
pub use lease::Lease;
pub use policy::{Policy, State};
pub use refusal::Refusal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    key: u64,
    stored_at: Duration,
    age: Duration,
    policy: Policy,
    /// Failed refreshes stay quiet while `now < retry_at`; zero means none.
    retry_at: Duration,
}

impl Entry {
    const fn age_at(&self, now: Duration) -> Duration {
        self.age.saturating_add(now.saturating_sub(self.stored_at))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lead {
    lease: u64,
    key: u64,
    flight: FlightId,
    deadline: Duration,
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
    /// Also the one id sequence of this refresher: leases are reserved from
    /// it, between the flight and waiter ids it issues itself.
    flights: Coalescer<FLIGHTS, WAITERS>,
    now: Duration,
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
    /// Start with no metadata, nothing in flight, at time zero, in scope zero.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_scope(0)
    }

    /// Start empty at time zero, issuing leases and waiter ids that carry `scope`.
    ///
    /// Leases, waiter ids and flight ids all come from the one sequence of
    /// the coalescer inside: the scope in the top 16 bits, a 48-bit sequence
    /// below. No two ids of a refresher are equal, whatever their kind, so a
    /// lease rebuilt as a waiter id, or the reverse, names nothing. Two
    /// refreshers in the same scope issue the same ids, and an id handed to
    /// the wrong one can name a live lease or waiter there. Give each
    /// refresher, and each coalescer whose ids could meet it, its own scope:
    /// an id from another scope is then always [`Refusal::Stale`]. Keeping
    /// scopes distinct is the caller's job.
    #[must_use]
    pub const fn with_scope(scope: u16) -> Self {
        Self::over(Coalescer::with_scope(scope))
    }

    /// Start empty at time zero, the id sequence continuing after `issued`.
    ///
    /// For a refresher that replaces an earlier one in the same `scope`, after
    /// a restart: built with [`Self::with_scope`] it would count from one
    /// again, and a lease or waiter id that outlived the restart could name a
    /// new refresh. Save [`Self::issued`] before handing an id out and resume
    /// from it, or give each incarnation a scope of its own. Every id up to
    /// `issued` is then [`Refusal::Stale`]. Metadata and time are not resumed.
    ///
    /// `None` exactly when [`periscope_machines::Issuer::resumed`] answers
    /// `None`: `issued` is beyond [`periscope_machines::SEQUENCE_MAX`].
    #[must_use]
    pub const fn resumed(scope: u16, issued: u64) -> Option<Self> {
        match Coalescer::resumed(scope, issued) {
            Some(flights) => Some(Self::over(flights)),
            None => None,
        }
    }

    /// The scope carried by every lease and waiter id this refresher issues.
    #[must_use]
    pub const fn scope(&self) -> u16 {
        self.flights.scope()
    }

    /// How many ids this refresher has issued, of every kind: leases, waiter
    /// ids and flight ids. The count to save for [`Self::resumed`].
    #[must_use]
    pub const fn issued(&self) -> u64 {
        self.flights.issued()
    }

    /// Decide what a caller asking for `key` at `now` should do.
    ///
    /// A lease issued here must be reported within `lead_for`; after that the
    /// next caller that needs one takes the refresh over and the old lease
    /// goes stale. Callers already waiting keep waiting on the new leader.
    ///
    /// # Errors
    ///
    /// [`Refusal::TimeRegressed`], then [`Refusal::Rejected`] for a zero
    /// `lead_for`, and, only when nothing is servable, a full table before a
    /// spent sequence: [`Refusal::FlightsFull`] or [`Refusal::WaitersFull`],
    /// then [`Refusal::Exhausted`]. A new refresh takes two ids, its lease and
    /// its flight, so it is `Exhausted` with one id left; a takeover and a
    /// wait take one. Nothing changes, and no id is consumed.
    pub fn decide(
        &mut self,
        key: u64,
        now: Duration,
        lead_for: Duration,
    ) -> Result<Decision, Refusal> {
        self.check(now)?;
        if lead_for.is_zero() {
            return Err(Refusal::Rejected);
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
            Some(_) | None if live => {
                let (waiter, flight) = self.follow(key)?;
                Decision::Wait { waiter, flight }
            }
            Some(_) | None => Decision::Fetch {
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
    /// [`Refusal::TimeRegressed`], [`Refusal::Stale`] when the lease is not
    /// live, [`Refusal::EntriesFull`] when the key is new and no metadata slot
    /// is free (the lease stays live: [`Self::forget`] a key and retry, or
    /// [`Self::abandon`]). Nothing changes.
    pub fn store(
        &mut self,
        lease: Lease,
        now: Duration,
        age: Duration,
        policy: Policy,
    ) -> Result<(), Refusal> {
        self.check(now)?;
        let (slot, lead) = self.held(lease).ok_or(Refusal::Stale)?;
        let home = self.room_for(lead.key).ok_or(Refusal::EntriesFull)?;
        self.flights.complete(lead.flight)?;
        self.leads[slot] = None;
        self.entries[home] = Some(Entry {
            key: lead.key,
            stored_at: now,
            age,
            policy,
            retry_at: Duration::ZERO,
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
    /// [`Refusal::TimeRegressed`], or [`Refusal::Stale`] when the lease is not
    /// live. Nothing changes.
    pub fn fail(
        &mut self,
        lease: Lease,
        now: Duration,
        retry_after: Duration,
    ) -> Result<Option<Duration>, Refusal> {
        self.check(now)?;
        let (slot, lead) = self.held(lease).ok_or(Refusal::Stale)?;
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
    /// [`Refusal::Stale`] when the lease is not live. Nothing changes.
    pub fn abandon(&mut self, lease: Lease) -> Result<(), Refusal> {
        let (slot, lead) = self.held(lease).ok_or(Refusal::Stale)?;
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
    /// [`Refusal::TimeRegressed`]. Nothing changes.
    pub fn reap(&mut self, now: Duration) -> Result<usize, Refusal> {
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
    /// [`Refusal::TimeRegressed`], or [`Refusal::EntriesFull`] when the key is
    /// new and no metadata slot is free. Nothing changes.
    pub fn put(
        &mut self,
        key: u64,
        now: Duration,
        age: Duration,
        policy: Policy,
    ) -> Result<(), Refusal> {
        self.check(now)?;
        let home = self.room_for(key).ok_or(Refusal::EntriesFull)?;
        self.entries[home] = Some(Entry {
            key,
            stored_at: now,
            age,
            policy,
            retry_at: Duration::ZERO,
        });
        self.now = now;
        Ok(())
    }

    /// Drop the metadata for `key`, e.g. because the cache evicted its value.
    ///
    /// Never refuses: [`Forgetting::Forgotten`] when there was metadata to
    /// drop, [`Forgetting::NotHeld`] when the key already had none, which
    /// changes nothing. A refresh in flight is untouched either way.
    pub fn forget(&mut self, key: u64) -> Forgetting {
        match self
            .entry_slot(key)
            .and_then(|slot| self.entries[slot].take())
        {
            Some(_) => Forgetting::Forgotten,
            None => Forgetting::NotHeld,
        }
    }

    /// Drop metadata that is [`State::Expired`] at `now`, writing the keys to `out`.
    ///
    /// Writes the first `n` entries, returns `n`, leaves the rest untouched,
    /// and stops when `out` is full: call again to continue. Only those `n`
    /// are swept keys; `[0; N]` sizes the buffer, and zero is also a valid
    /// key, so read no further than `n`. Keys with a refresh in flight are
    /// kept. The values themselves are the caller's to delete.
    ///
    /// # Errors
    ///
    /// [`Refusal::TimeRegressed`]. Nothing changes.
    pub fn sweep(&mut self, now: Duration, out: &mut [u64]) -> Result<usize, Refusal> {
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
                out[written] = entry.key;
                written += 1;
            }
        }
        Ok(written)
    }

    /// Withdraw a waiter, whether still waiting or notified but undrained.
    ///
    /// # Errors
    ///
    /// [`Refusal::Stale`] when `waiter` is not registered. Nothing changes.
    pub fn cancel(&mut self, waiter: WaiterId) -> Result<(), Refusal> {
        Ok(self.flights.cancel(waiter)?)
    }

    /// Take the oldest queued notification, freeing its waiter slot.
    pub fn poll(&mut self) -> Option<Notification> {
        self.flights.poll()
    }

    /// Move queued notifications into `out`, oldest first, and return how many.
    ///
    /// Writes the first `n` entries and leaves the rest untouched; only those
    /// `n` are notifications. `[Notification::default(); N]` sizes the buffer
    /// with a placeholder the refresher never emits. Resumable: whatever did
    /// not fit stays queued, in order.
    pub fn drain(&mut self, out: &mut [Notification]) -> usize {
        self.flights.drain(out)
    }

    /// Age of the value held for `key` at `now`, if there is metadata for it.
    #[must_use]
    pub fn age(&self, key: u64, now: Duration) -> Option<Duration> {
        Some(self.entries[self.entry_slot(key)?]?.age_at(now))
    }

    /// Fresh, stale or expired at `now`; `None` when the key has no metadata.
    #[must_use]
    pub fn state(&self, key: u64, now: Duration) -> Option<State> {
        let entry = self.entries[self.entry_slot(key)?]?;
        Some(entry.policy.state(entry.age_at(now)))
    }

    /// Age of the held value if a refresh error at `now` may be answered with it.
    #[must_use]
    pub fn stale_if_error(&self, key: u64, now: Duration) -> Option<Duration> {
        let entry = self.entries[self.entry_slot(key)?]?;
        let age = entry.age_at(now);
        entry.policy.if_error(age).then_some(age)
    }

    /// The lease currently refreshing `key`, overdue or not.
    #[must_use]
    pub fn leading(&self, key: u64) -> Option<Lease> {
        Some(Lease(self.leads[self.lead_slot(key)?]?.lease))
    }

    /// The coalesce flight a live lease rides on.
    ///
    /// This is the [`Notification::flight`] the waiters of that refresh will
    /// be woken with, so a drained notification can be tied back to a lease.
    /// A takeover issues a new lease for the same flight: the new lease
    /// answers the flight the old one did, and the old one answers `None`.
    /// Once the refresh settles the lease is stale, so record the pairing
    /// while the lease is live if it is needed after the report.
    ///
    /// `None` for a lease that is not live: never issued, reported, abandoned,
    /// reaped, taken over, or issued in another scope.
    #[must_use]
    pub fn flight(&self, lease: Lease) -> Option<FlightId> {
        Some(self.held(lease)?.1.flight)
    }

    /// The latest time this refresher accepted.
    #[must_use]
    pub const fn now(&self) -> Duration {
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

    const fn check(&self, now: Duration) -> Result<(), Refusal> {
        if now.as_nanos() < self.now.as_nanos() {
            Err(Refusal::TimeRegressed)
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

    const fn over(flights: Coalescer<FLIGHTS, WAITERS>) -> Self {
        Self {
            entries: [None; KEYS],
            leads: [None; FLIGHTS],
            flights,
            now: Duration::ZERO,
        }
    }

    /// The slot and record of a live lease. An id this refresher never
    /// issued, one from another scope included, is turned away before the
    /// table is read; an id it issued as a waiter or a flight is in no lead.
    fn held(&self, lease: Lease) -> Option<(usize, Lead)> {
        if !self.flights.owns(lease.0) {
            return None;
        }
        self.leads
            .iter()
            .enumerate()
            .find_map(|(slot, lead)| Some((slot, lead.filter(|lead| lead.lease == lease.0)?)))
    }

    /// Issue a lease for `key`: a new refresh, or a takeover of an overdue one.
    ///
    /// Every check comes before the first id is drawn, so a refused lead
    /// consumes none: a full table first, the spent sequence last, in the
    /// order the coalescer itself refuses a join.
    fn lead(&mut self, key: u64, now: Duration, lead_for: Duration) -> Result<Lease, Refusal> {
        let deadline = now.saturating_add(lead_for);
        if let Some(lead) = self
            .lead_slot(key)
            .and_then(|slot| self.leads[slot].as_mut())
        {
            // A takeover: a new lease on the flight already in the air.
            let lease = self.flights.reserve()?;
            lead.lease = lease;
            lead.deadline = deadline;
            return Ok(Lease(lease));
        }
        let slot = self
            .leads
            .iter()
            .position(Option::is_none)
            .ok_or(Refusal::FlightsFull)?;
        // A lease and a flight: with one id left, neither is drawn.
        if self.flights.remaining() < 2 {
            return Err(Refusal::Exhausted);
        }
        let lease = self.flights.reserve()?;
        let flight = match self.flights.join(key)? {
            Joined::Leader(flight) => flight,
            // Unreachable while every flight has a lead; undo and refuse.
            Joined::Follower { waiter, .. } => {
                self.flights.cancel(waiter)?;
                return Err(Refusal::Stale);
            }
        };
        self.leads[slot] = Some(Lead {
            lease,
            key,
            flight,
            deadline,
        });
        Ok(Lease(lease))
    }

    fn follow(&mut self, key: u64) -> Result<(WaiterId, FlightId), Refusal> {
        match self.flights.join(key)? {
            Joined::Follower { waiter, flight } => Ok((waiter, flight)),
            // Unreachable while every lead has a flight; undo and refuse.
            Joined::Leader(flight) => {
                self.flights.abandon(flight)?;
                Err(Refusal::Stale)
            }
        }
    }
}

#[cfg(test)]
mod tests;
