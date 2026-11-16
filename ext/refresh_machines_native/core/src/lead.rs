//! Registering and settling refreshes on the coalescer inside.

use core::time::Duration;

use coalesce_machines::{Coalescer, FlightId, Joined, Outcome, WaiterId};

use crate::{Lead, Lease, Refresher, Refusal};

impl<const KEYS: usize, const FLIGHTS: usize, const WAITERS: usize>
    Refresher<KEYS, FLIGHTS, WAITERS>
{
    /// Issue a lease for `key`: a new refresh, or a takeover of an overdue one.
    ///
    /// Every check comes before the first id is drawn, so a refused lead
    /// consumes none: a full table first, the spent sequence last, in the
    /// order the coalescer itself refuses a join.
    pub(crate) fn lead(
        &mut self,
        key: u64,
        now: Duration,
        lead_for: Duration,
    ) -> Result<Lease, Refusal> {
        let deadline = now.saturating_add(lead_for);
        let Self { leads, flights, .. } = self;
        if let Some(lead) = leads.iter_mut().flatten().find(|lead| lead.key == key) {
            // A takeover: a new lease on the flight already in the air.
            let lease = flights.reserve()?;
            lead.lease = lease;
            lead.deadline = deadline;
            return Ok(Lease(lease));
        }
        let free = leads
            .iter_mut()
            .find(|lead| lead.is_none())
            .ok_or(Refusal::FlightsFull)?;
        if flights.flight(key).is_some() {
            debug_assert!(false, "every coalesce flight has a lead");
            return Err(Refusal::Stale);
        }
        if flights.in_flight() == FLIGHTS {
            debug_assert!(false, "a free lead slot means a free flight slot");
            return Err(Refusal::FlightsFull);
        }
        // A lease and a flight: with one id left, neither is drawn.
        if flights.remaining() < 2 {
            return Err(Refusal::Exhausted);
        }
        // The key has no flight, so the join leads; the snapshot only serves
        // the arm the type demands, to leave nothing changed if it is reached.
        let before = flights.clone();
        let lease = flights.reserve()?;
        let flight = match flights.join(key) {
            Ok(Joined::Leader(flight)) => flight,
            Ok(Joined::Follower { .. })
            | Err(
                coalesce_machines::Refusal::FlightsFull
                | coalesce_machines::Refusal::WaitersFull
                | coalesce_machines::Refusal::Exhausted
                | coalesce_machines::Refusal::Stale,
            ) => {
                debug_assert!(
                    false,
                    "a key with no flight leads, with ids and a slot to spare"
                );
                *flights = before;
                return Err(Refusal::Stale);
            }
        };
        *free = Some(Lead {
            lease,
            key,
            flight,
            deadline,
        });
        Ok(Lease(lease))
    }

    /// Wait on the live lead's flight for `key`.
    pub(crate) fn follow(&mut self, key: u64) -> Result<(WaiterId, FlightId), Refusal> {
        if self.flights.flight(key).is_none() {
            debug_assert!(false, "every lead's flight is in the air");
            return Err(Refusal::Stale);
        }
        // The key is in flight, so the join follows or refuses; the snapshot
        // only serves the arm the type demands, to leave nothing changed.
        let before = self.flights.clone();
        match self.flights.join(key)? {
            Joined::Follower { waiter, flight } => Ok((waiter, flight)),
            Joined::Leader(_) => {
                debug_assert!(false, "a key in flight is never led again");
                self.flights = before;
                Err(Refusal::Stale)
            }
        }
    }

    /// Settle the flight of the lead held in `slot` and free the slot.
    ///
    /// Every lead's flight is in the air, so the coalescer never refuses; if
    /// it did, it changed nothing and neither does this: the slot is freed
    /// only once the flight has settled.
    pub(crate) fn settle(
        flights: &mut Coalescer<FLIGHTS, WAITERS>,
        slot: &mut Option<Lead>,
        flight: FlightId,
        outcome: Outcome,
    ) -> Result<(), Refusal> {
        let settled = match outcome {
            Outcome::Completed => flights.complete(flight),
            Outcome::Abandoned => flights.abandon(flight),
        };
        match settled {
            Ok(_) => {
                *slot = None;
                Ok(())
            }
            Err(refusal) => {
                debug_assert!(false, "every lead's flight is in the air");
                Err(refusal.into())
            }
        }
    }
}
