use core::time::Duration;

use coalesce_machines::{FlightId, Joined, WaiterId};

use crate::{Lease, Refusal};

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
        until: Duration,
    },
    /// A refresh is due but could not be registered, for the reason given.
    ///
    /// This is not a refused call: the stale value is still served, so
    /// [`crate::Refresher::decide`] is honoured and answers
    /// `Ok(Decision::Stale { .. })`. The failed registration changed nothing
    /// (no lease, no flight, no waiter); only the clock advances, as for any
    /// honoured decision. Just serve; the next caller tries again.
    Unavailable(Refusal),
}

/// The answer to "may I serve what I have for this key?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Serve the cached value.
    Fresh {
        /// Its age at `now`.
        age: Duration,
    },
    /// Serve the cached value, stale, and mind the revalidation.
    Stale {
        /// Its age at `now`.
        age: Duration,
        /// Who refreshes it, if anyone.
        revalidation: Revalidation,
    },
    /// Nothing servable. The caller fetches and reports with the lease.
    Fetch {
        /// The right to refresh this key.
        lease: Lease,
    },
    /// Nothing servable and another caller is already fetching. Wait for a
    /// [`crate::Notification`] carrying this id, then decide again.
    Wait {
        /// The caller's notification id.
        waiter: WaiterId,
        /// The refresh being shared: what [`crate::Refresher::flight`] answers for
        /// the key's live lease, and the [`crate::Notification::flight`] to come.
        flight: FlightId,
    },
}

impl Decision {
    /// A [`Decision::Wait`], rebuilt from its two ids.
    ///
    /// `None` unless they are ids one refresher could have answered together,
    /// as [`Joined::follower`] judges: the same scope, the waiter issued after
    /// the flight it waits on. The ids are not looked up.
    #[must_use]
    pub const fn wait(waiter: WaiterId, flight: FlightId) -> Option<Self> {
        match Joined::follower(waiter, flight) {
            Some(_) => Some(Self::Wait { waiter, flight }),
            None => None,
        }
    }
}

/// What [`crate::Refresher::forget`] did. Both are honoured; neither is a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forgetting {
    /// The key had metadata and it was dropped; its slot is free.
    Forgotten,
    /// The key had no metadata: an idempotent repeat that changed nothing.
    NotHeld,
}
