# refresh-machines

> **AMS Nottingham**, NCC-4230, *Nottingham*-class resupply tender.

Engine-room flight manual: decide whether the stores already aboard may be
issued, and which single crew goes out for the resupply.

Allocation-free `no_std` freshness metadata and refresh leases, after RFC 5861.
A `Policy` holds three `Duration`s: `fresh_for`, `stale_while_revalidate` and
`stale_if_error`. Given an explicit age it answers `Fresh`, `Stale` or
`Expired`. A `Refresher` keeps that metadata per key and turns a lookup into a
`Decision`: serve fresh, serve stale (and maybe lead the revalidation), fetch
under a lease, or wait for whoever is already fetching.

The refresher stores no values, fetches nothing and evicts nothing. The caller
holds the cache, runs the fetch, and reports `store`, `fail` or `abandon` with
the lease. Which keys stay in the cache is `cache-machines`' question; this
crate only says whether a value that is still there may be served.

```rust
use core::time::Duration;

use refresh_machines::{Decision, Policy, Refresher, Revalidation};

// Instants are time since an epoch the caller picks; here, whole seconds.
let s = Duration::from_secs;
// Fresh for 60s, then 30s more while revalidating, 300s more if the origin errors.
let rules = Policy::new(s(60), s(30), s(300));
let mut refresher = Refresher::<8, 4, 16>::new();

// Nothing cached: this caller fetches, with 10s to report back.
let Ok(Decision::Fetch { lease }) = refresher.decide(42, s(0), s(10)) else { panic!("miss") };
// A second caller waits, and is told which flight it shares.
let Ok(Decision::Wait { flight, .. }) = refresher.decide(42, s(1), s(10)) else { panic!("waits") };
assert_eq!(refresher.flight(lease), Some(flight));
assert_eq!(refresher.store(lease, s(2), Duration::ZERO, rules), Ok(()));
assert_eq!(refresher.decide(42, s(50), s(10)), Ok(Decision::Fresh { age: s(48) }));

// Stale but inside the revalidate window: serve it, one caller refreshes.
let Ok(Decision::Stale { age, revalidation: Revalidation::Lead(lease) }) =
    refresher.decide(42, s(70), s(10)) else { panic!("first stale caller leads") };
assert_eq!(age, s(68));
assert_eq!(
    refresher.decide(42, s(71), s(10)),
    Ok(Decision::Stale { age: s(69), revalidation: Revalidation::InFlight })
);

// The origin is down. The stale-if-error window is open, so the old value
// covers, and nobody is asked to refetch for the next 20s.
assert_eq!(refresher.fail(lease, s(72), s(20)), Ok(Some(s(70))));
assert_eq!(
    refresher.decide(42, s(85), s(10)),
    Ok(Decision::Stale { age: s(83), revalidation: Revalidation::Backoff { until: s(92) } })
);
```

## Contract

- **Time and ages.** Every instant and every span is a `core::time::Duration`:
  `now` is monotonic time since an epoch the caller picks, and `lead_for`,
  `retry_after`, ages and the three policy windows are spans. Every mutating
  call takes `now` and refuses an earlier one with `Refusal::TimeRegressed`.
  `store` and `put` take the value's age on arrival, so a response that was
  already old upstream starts old here. Ages, deadlines and backoffs saturate
  at `Duration::MAX`.
- **Windows.** Fresh while `age < fresh_for`. Both stale windows open when
  freshness ends and are half-open: a `stale_while_revalidate` of 30s covers
  ages `fresh_for .. fresh_for + 30s`. A zero window disables its rule. Past both
  windows the value is `Expired` and is never served.
- **Decisions.** Inside the revalidate window the value is served and exactly
  one caller gets `Revalidation::Lead`. Outside it but inside the error window
  the caller must fetch (or wait); only a reported failure makes the old value
  servable, for as long as the failure's `retry_after` and the window both
  last. With nothing servable the answer is `Fetch` or `Wait`.
- **Leases.** One live lease per key, built on `coalesce-machines`. A lease is
  a capability: whoever holds it reports the refresh. It carries a deadline
  (`now + lead_for`); once that passes, the next caller needing a refresh takes
  it over, and `reap` revokes overdue leases nobody replaced. A taken-over,
  reaped, reported or abandoned lease is `Refusal::Stale` forever.
- **Waiters.** `Wait { waiter, flight }` returns a `WaiterId` and the `FlightId`
  being shared, the same pair as `coalesce_machines::Joined::Follower`. The
  flight is what `flight(lease)` answers for the key's live lease and what the
  notification will carry. When the refresh settles each waiter
  gets one `Notification` (`Completed` after `store`, `Abandoned` after `fail`,
  `abandon` or `reap`) through `poll` or `drain`, then decides again. A takeover
  keeps the waiters. `cancel` withdraws one.
- **Leases and flights.** Each refresh rides one `coalesce-machines` flight,
  and `Notification::flight` names it. `flight(lease)` answers the `FlightId`
  a live lease rides on, so a notification can be tied back to the lease that
  caused it. A takeover issues a new lease for the same flight: the new lease
  answers the old one's flight and the old one answers `None`, as does any
  stale lease or one from another scope. The mapping ends when the refresh
  settles, so record it while the lease is live.
- **One sequence.** Leases, waiter ids and flight ids all come from the one
  sequence of the coalescer inside (leases through its `reserve`), so no two
  ids of a refresher are equal, whatever their kind. A lease rebuilt as a
  `WaiterId`, or a waiter or flight id rebuilt as a `Lease`, is
  `Refusal::Stale` and changes nothing. A new refresh takes two ids, its
  lease and then its flight; a takeover takes one, the new lease; a wait takes
  one.
- **Output buffers.** `drain(out: &mut [Notification])` and
  `sweep(now, out: &mut [u64])` write the first `n` entries, return `n` and
  leave the rest untouched. `[Notification::default(); N]` and `[0; N]` size
  them; the default notification is a placeholder that is never emitted (its
  ids are raw zero, never issued). Read only the `n` entries reported.
- **Scopes.** Ids are only meaningful to the refresher that issued them. Two
  refreshers in the same scope issue the same leases and waiter ids, and an id
  handed to the wrong one can name a live lease or waiter there, so give each
  refresher, and any bare coalescer beside it, its own scope:
  `Refresher::with_scope(scope)` puts a caller-chosen `u16` in the top 16 bits
  of every lease, waiter id and flight id. An id from another scope is always
  `Refusal::Stale` (`None` from `flight`), refused before any table is read.
  `new()` is scope zero and counts 1, 2, 3 across all three kinds. The
  refresher cannot choose a scope itself. The layout belongs to
  `periscope-machines`, so use its `scoped`, `scope_of` and `sequence_of` on
  raw ids. After `periscope_machines::SEQUENCE_MAX` (2^48 - 1) ids `decide`
  answers `Refusal::Exhausted` wherever it would need one, and already when a
  new refresh would need two and one is left. A full table is reported before
  a spent sequence (`FlightsFull` or `WaitersFull`, then `Exhausted`), and a
  refused `decide` consumes no id.
- **Restarts.** A new refresher counts from one again, so a lease or waiter id
  that outlived a restart could name a new refresh. Either save `issued()` and
  build the replacement with `Refresher::resumed(scope, issued)` (empty, at
  time zero, the sequence continuing; `None` when `issued` is beyond
  `SEQUENCE_MAX`), or give each incarnation a new scope.
- **Repeats are not honoured.** The refresher keeps nothing about a lease once
  it is reported, abandoned, reaped or taken over, nor about a waiter once it
  leaves, so a second `store`, `fail`, `abandon` or `cancel` is
  `Refusal::Stale` even though the id was provably issued here.
- **Capacities.** `Refresher<KEYS, FLIGHTS, WAITERS>`. A full table answers
  `EntriesFull`, `FlightsFull` or `WaitersFull`; nothing is evicted. While a
  stale value is servable a full flight table does not refuse the caller: it
  answers `Ok(Decision::Stale { .. })` with `Revalidation::Unavailable(reason)`.
  That is on purpose and not a refused call: the stale value is still served,
  so the decision is honoured, and the registration that failed changed nothing
  (no lease, no flight, no waiter). The next caller tries again.
- **Refusals change nothing.** Every `Err` leaves the refresher as it was.
- **Housekeeping.** `forget(key)` drops metadata when the cache evicts the
  value. It never refuses and answers `Forgetting::Forgotten`, or
  `Forgetting::NotHeld` when the key already had none: an idempotent repeat
  that changes nothing. `sweep(now, out)` drops expired metadata into a caller-sized buffer and
  resumes on the next call.

## Not Included

No clock, values, payloads, hashing, I/O, threads or locks. No eviction or
admission. No negative caching: a miss that fails is forgotten and the next
caller fetches. No HTTP parsing: map `max-age`, `stale-while-revalidate` and
`stale-if-error` to the three durations yourself. No wall clock: turning a
timestamp into an age is the caller's job. No retry schedule: the caller
supplies each `retry_after`. Keys are caller-supplied `u64`s. State is in memory
and scoped to one refresher. Lookups are linear scans over the fixed tables,
which suits small capacities.

Initial, unstable API.
