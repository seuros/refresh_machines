//! Private hooks that hold every `RefreshMachines` Data value to what the
//! core accepts, whether the gem built it or the caller did.

use magnus::{Error, Ruby, Value};
use matryona_machines::ruby::{Choice, Symbols, duration, instant, integer, symbol_or_fields};
use refresh_machines::{Decision, FlightId, Lease, Notification, WaiterId};

use crate::Refusals;
use crate::answers::{Outcomes, Revalidations, classes};

/// `__policy`: the three windows `Policy::new` takes, each Integer
/// nanoseconds in `0..2**64-1`, named for its field.
pub fn policy(fresh_for: Value, revalidate: Value, if_error: Value) -> Result<(), Error> {
    duration(fresh_for, "fresh_for")?;
    duration(revalidate, "stale_while_revalidate")?;
    duration(if_error, "stale_if_error").map(drop)
}

/// `__age`: a `Fresh` or `Stale` age, Integer nanoseconds a core `Duration`
/// holds, up to `Duration::MAX`: an age the core answers may pass the
/// argument range because sums saturate instead of wrapping.
pub fn age(value: Value) -> Result<(), Error> {
    instant(value, "age").map(drop)
}

/// `__until`: a `Backoff`'s instant, over the whole `Duration` range like
/// an age.
pub fn until(value: Value) -> Result<(), Error> {
    instant(value, "until").map(drop)
}

/// `__lease`: what `Lease::from_raw` accepts.
pub fn lease(ruby: &Ruby, value: Value) -> Result<Value, Error> {
    lease_id(ruby, integer(value, "lease")?).map(|_| value)
}

/// `__wait`: what `Decision::wait` accepts.
pub fn wait(ruby: &Ruby, waiter: Value, flight: Value) -> Result<(), Error> {
    let (waiter, flight) = (integer(waiter, "waiter")?, integer(flight, "flight")?);
    let (waiter, flight) = (waiter_id(ruby, waiter)?, flight_id(ruby, flight)?);
    Decision::wait(waiter, flight)
        .map(|_| ())
        .ok_or_else(|| not_together(ruby))
}

/// `__notification`: what `Notification::new` accepts.
pub fn notification(
    ruby: &Ruby,
    waiter: Value,
    flight: Value,
    key: Value,
    outcome: Value,
) -> Result<(), Error> {
    let (waiter, flight) = (integer(waiter, "waiter")?, integer(flight, "flight")?);
    let key = integer(key, "key")?;
    let outcome = Outcomes::parse(ruby, outcome)?;
    let (waiter, flight) = (waiter_id(ruby, waiter)?, flight_id(ruby, flight)?);
    Notification::new(waiter, flight, key, outcome)
        .map(|_| ())
        .ok_or_else(|| not_together(ruby))
}

fn not_together(ruby: &Ruby) -> Error {
    Error::new(
        ruby.exception_range_error(),
        "the waiter must be of the flight's scope and issued after it",
    )
}

/// `__refusal`: the symbol of a refusal variant.
pub fn refusal(ruby: &Ruby, value: Value) -> Result<Value, Error> {
    Refusals::parse(ruby, value).map(|_| value)
}

const NOT_ONE: &str = "revalidation must be a Lead, :in_flight, a Backoff or an Unavailable";

/// `__revalidation`: what a `Stale#revalidation` may hold, read natively
/// because `case`/`===` takes a subclass: `:in_flight`, or exactly a `Lead`,
/// `Backoff` or `Unavailable` whose fields are checked again (`Marshal.load`
/// skips `initialize`). `RangeError` for another Symbol, `TypeError` for
/// anything else.
pub fn revalidation(ruby: &Ruby, value: Value) -> Result<Value, Error> {
    let classes = classes(ruby)?;
    let one_of = [
        classes.lead.get(ruby),
        classes.backoff.get(ruby),
        classes.unavailable.get(ruby),
    ];
    // Each class has one member, read from the struct.
    match symbol_or_fields(ruby, value, Revalidations, &one_of, NOT_ONE)? {
        Choice::Symbol(_) => Ok(()),
        Choice::Struct(0, [field]) => self::lease(ruby, field).map(drop),
        Choice::Struct(1, [field]) => self::until(field),
        Choice::Struct(_, [field]) => refusal(ruby, field).map(drop),
    }?;
    Ok(value)
}

/// The lease an Integer names, `RangeError` for a sequence of zero.
pub fn lease_id(ruby: &Ruby, raw: u64) -> Result<Lease, Error> {
    Lease::from_raw(raw).ok_or_else(|| zero_sequence(ruby, "lease"))
}

/// The waiter id an Integer names, `RangeError` for a sequence of zero.
pub fn waiter_id(ruby: &Ruby, raw: u64) -> Result<WaiterId, Error> {
    WaiterId::from_raw(raw).ok_or_else(|| zero_sequence(ruby, "waiter"))
}

fn flight_id(ruby: &Ruby, raw: u64) -> Result<FlightId, Error> {
    FlightId::from_raw(raw).ok_or_else(|| zero_sequence(ruby, "flight"))
}

fn zero_sequence(ruby: &Ruby, kind: &str) -> Error {
    Error::new(
        ruby.exception_range_error(),
        format!("a {kind} id's sequence (the low 48 bits) must not be zero"),
    )
}
