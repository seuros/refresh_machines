# refresh-machines

> **AMS Nottingham**, NCC-4230, *Nottingham*-class resupply tender.

Engine-room flight manual: decide whether the stores already aboard may be
issued, and which single crew goes out for the resupply.

Allocation-free `no_std` freshness metadata and refresh leases, after RFC 5861.
A `Policy` holds three durations: `fresh_for`, `stale_while_revalidate` and
`stale_if_error`. Given an explicit age it answers `Fresh`, `Stale` or
`Expired`. A `Refresher` keeps that metadata per key and turns a lookup into a
`Decision`: serve fresh, serve stale (and maybe lead the revalidation), fetch
under a lease, or wait for whoever is already fetching.

The refresher stores no values, fetches nothing and evicts nothing. The caller
holds the cache, runs the fetch, and reports `store`, `fail` or `abandon` with
the lease. Which keys stay in the cache is `cache-machines`' question; this
crate only says whether a value that is still there may be served.

```rust
use refresh_machines::{Decision, Policy, Refresher, Revalidation};

// Fresh for 60, then 30 more while revalidating, 300 more if the origin errors.
let rules = Policy::new(60, 30, 300);
let mut refresher = Refresher::<8, 4, 16>::new();

// Nothing cached: this caller fetches, with 10 ticks to report back.
let Ok(Decision::Fetch { lease }) = refresher.decide(42, 0, 10) else { panic!("miss") };
assert!(matches!(refresher.decide(42, 1, 10), Ok(Decision::Wait { .. })));
assert_eq!(refresher.store(lease, 2, 0, rules), Ok(()));
assert_eq!(refresher.decide(42, 50, 10), Ok(Decision::Fresh { age: 48 }));

// Stale but inside the revalidate window: serve it, one caller refreshes.
let Ok(Decision::Stale { age: 68, revalidation: Revalidation::Lead(lease) }) =
    refresher.decide(42, 70, 10) else { panic!("first stale caller leads") };
assert_eq!(
    refresher.decide(42, 71, 10),
    Ok(Decision::Stale { age: 69, revalidation: Revalidation::InFlight })
);

// The origin is down. The stale-if-error window is open, so the old value
// covers, and nobody is asked to refetch for the next 20 ticks.
assert_eq!(refresher.fail(lease, 72, 20), Ok(Some(70)));
assert_eq!(
    refresher.decide(42, 85, 10),
    Ok(Decision::Stale { age: 83, revalidation: Revalidation::Backoff { until: 92 } })
);
```

## Contract

- **Time and ages.** Every mutating call takes `now` in the caller's unit and
  refuses an earlier one with `Refused::TimeRegressed`. `store` and `put` take
  the value's age on arrival, so a response that was already old upstream
  starts old here. Ages, deadlines and backoffs saturate at `u64::MAX`.
- **Windows.** Fresh while `age < fresh_for`. Both stale windows open when
  freshness ends and are half-open: `stale_while_revalidate = 30` covers ages
  `fresh_for .. fresh_for + 30`. A zero window disables its rule. Past both
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
  reaped, reported or abandoned lease is `Refused::Stale` forever.
- **Waiters.** `Wait` returns a `WaiterId`. When the refresh settles each waiter
  gets one `Notification` (`Completed` after `store`, `Abandoned` after `fail`,
  `abandon` or `reap`) through `poll` or `drain`, then decides again. A takeover
  keeps the waiters. `cancel` withdraws one.
- **Capacities.** `Refresher<KEYS, FLIGHTS, WAITERS>`. A full table answers
  `EntriesFull`, `FlightsFull` or `WaitersFull`; nothing is evicted. While a
  stale value is servable a full flight table does not refuse the caller: it
  answers `Stale` with `Revalidation::Unavailable(reason)`.
- **Refusals change nothing.** Every `Err` leaves the refresher as it was.
- **Housekeeping.** `forget(key)` drops metadata when the cache evicts the
  value. `sweep(now, out)` drops expired metadata into a caller-sized buffer and
  resumes on the next call.

## Not Included

No clock, values, payloads, hashing, I/O, threads or locks. No eviction or
admission. No negative caching: a miss that fails is forgotten and the next
caller fetches. No HTTP parsing: map `max-age`, `stale-while-revalidate` and
`stale-if-error` to the three durations yourself. No retry schedule: the caller
supplies each `retry_after`. Keys are caller-supplied `u64`s. State is in memory
and scoped to one refresher. Lookups are linear scans over the fixed tables,
which suits small capacities.

Initial, unstable API.
