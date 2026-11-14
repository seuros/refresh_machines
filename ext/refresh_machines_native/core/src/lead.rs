//! Registering and settling refreshes on the coalescer inside.

use core::time::Duration;

use coalesce_machines::{FlightId, Joined, Outcome, WaiterId};

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
        if self.flights.flight(key).is_some() {
            debug_assert!(false, "every coalesce flight has a lead");
            return Err(Refusal::Stale);
        }
        if self.flights.in_flight() == FLIGHTS {
            debug_assert!(false, "a free lead slot means a free flight slot");
            return Err(Refusal::FlightsFull);
        }
        // A lease and a flight: with one id left, neither is drawn.
        if self.flights.remaining() < 2 {
            return Err(Refusal::Exhausted);
        }
        // The key has no flight, so the join leads; the snapshot only serves
        // the arm the type demands, to leave nothing changed if it is reached.
        let before = self.flights.clone();
        let lease = self.flights.reserve()?;
        let flight = match self.flights.join(key) {
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
                self.flights = before;
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

    /// Settle the flight of the lead in `slot` and free the slot.
    ///
    /// Every lead's flight is in the air, so the coalescer never refuses; if
    /// it did, it changed nothing and neither does this: the slot is freed
    /// only once the flight has settled.
    pub(crate) fn settle(
        &mut self,
        slot: usize,
        flight: FlightId,
        outcome: Outcome,
    ) -> Result<(), Refusal> {
        let settled = match outcome {
            Outcome::Completed => self.flights.complete(flight),
            Outcome::Abandoned => self.flights.abandon(flight),
        };
        match settled {
            Ok(_) => {
                self.leads[slot] = None;
                Ok(())
            }
            Err(refusal) => {
                debug_assert!(false, "every lead's flight is in the air");
                Err(refusal.into())
            }
        }
    }
}
