//! Private hooks that hold every `RefreshMachines` Data value to what the
//! core accepts, whether the gem built it or the caller did.

use std::time::Duration;

use magnus::{Error, Ruby, Symbol, TryConvert, Value};
use refresh_machines::{Decision, FlightId, Lease, Notification, Outcome, Refusal, WaiterId};

use crate::answers::{classes, members_of};
use crate::{NANOS_TYPE, Nanos, Number, integer, refusal_name};

/// `__nanos`: Integer nanoseconds in `0..2**64-1`.
pub fn nanos(value: Value) -> Result<Value, Error> {
    Nanos::try_convert(value).map(|_| value)
}

/// `__duration`: Integer nanoseconds a core `Duration` holds, up to
/// `Duration::MAX`: the range of an age or instant the core answers, which
/// may pass the argument range because sums saturate instead of wrapping.
pub fn duration(ruby: &Ruby, value: Value) -> Result<Value, Error> {
    integer(value, NANOS_TYPE)?;
    match u128::try_convert(value) {
        Ok(nanos) if nanos <= Duration::MAX.as_nanos() => Ok(value),
        Ok(_) | Err(_) => Err(Error::new(
            ruby.exception_range_error(),
            "nanoseconds must be in 0..=Duration::MAX (about 1.8e28)",
        )),
    }
}

/// `__lease`: what `Lease::from_raw` accepts.
pub fn lease(ruby: &Ruby, value: Value) -> Result<Value, Error> {
    lease_id(ruby, Number::try_convert(value)?).map(|_| value)
}

/// `__wait`: what `Decision::wait` accepts.
pub fn wait(ruby: &Ruby, waiter: Number, flight: Number) -> Result<(), Error> {
    let (waiter, flight) = (waiter_id(ruby, waiter)?, flight_id(ruby, flight)?);
    Decision::wait(waiter, flight)
        .map(|_| ())
        .ok_or_else(|| not_together(ruby))
}

/// `__notification`: what `Notification::new` accepts.
pub fn notification(
    ruby: &Ruby,
    waiter: Number,
    flight: Number,
    key: Number,
    outcome: Value,
) -> Result<(), Error> {
    let name = symbol_name(ruby, outcome, "outcome")?;
    let (waiter, flight) = (waiter_id(ruby, waiter)?, flight_id(ruby, flight)?);
    let outcome = match name.as_str() {
        "completed" => Outcome::Completed,
        "abandoned" => Outcome::Abandoned,
        _ => {
            return Err(Error::new(
                ruby.exception_range_error(),
                format!("outcome must be :completed or :abandoned, not :{name}"),
            ));
        }
    };
    Notification::new(waiter, flight, key.0, outcome)
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
    let name = symbol_name(ruby, value, "refusal")?;
    refusal_from(&name).map(|_| value).ok_or_else(|| {
        Error::new(
            ruby.exception_range_error(),
            format!("unknown refusal :{name}"),
        )
    })
}

/// `__revalidation`: what a `Stale#revalidation` may hold, read natively
/// because `case`/`===` takes a subclass: `:in_flight`, or exactly a `Lead`,
/// `Backoff` or `Unavailable` whose fields are checked again (`Marshal.load`
/// skips `initialize`). `RangeError` for another Symbol, `TypeError` for
/// anything else.
pub fn revalidation(ruby: &Ruby, value: Value) -> Result<Value, Error> {
    if let Some(symbol) = Symbol::from_value(value) {
        let name = symbol.name()?;
        if name == "in_flight" {
            return Ok(value);
        }
        return Err(Error::new(
            ruby.exception_range_error(),
            format!("unknown revalidation :{name}"),
        ));
    }
    let classes = classes(ruby)?;
    if let Some([lease]) = members_of(value, classes.lead)? {
        self::lease(ruby, lease)?;
    } else if let Some([until]) = members_of(value, classes.backoff)? {
        duration(ruby, until)?;
    } else if let Some([refused]) = members_of(value, classes.unavailable)? {
        refusal(ruby, refused)?;
    } else {
        return Err(Error::new(
            ruby.exception_type_error(),
            "revalidation must be a Lead, :in_flight, a Backoff or an Unavailable",
        ));
    }
    Ok(value)
}

/// The lease an Integer names, `RangeError` for a sequence of zero.
pub fn lease_id(ruby: &Ruby, raw: Number) -> Result<Lease, Error> {
    Lease::from_raw(raw.0).ok_or_else(|| zero_sequence(ruby, "lease"))
}

/// The waiter id an Integer names, `RangeError` for a sequence of zero.
pub fn waiter_id(ruby: &Ruby, raw: Number) -> Result<WaiterId, Error> {
    WaiterId::from_raw(raw.0).ok_or_else(|| zero_sequence(ruby, "waiter"))
}

fn flight_id(ruby: &Ruby, raw: Number) -> Result<FlightId, Error> {
    FlightId::from_raw(raw.0).ok_or_else(|| zero_sequence(ruby, "flight"))
}

fn zero_sequence(ruby: &Ruby, kind: &str) -> Error {
    Error::new(
        ruby.exception_range_error(),
        format!("a {kind} id's sequence (the low 48 bits) must not be zero"),
    )
}

fn symbol_name(ruby: &Ruby, value: Value, what: &str) -> Result<String, Error> {
    let symbol = Symbol::from_value(value).ok_or_else(|| {
        Error::new(
            ruby.exception_type_error(),
            format!("{what} must be a Symbol"),
        )
    })?;
    Ok(symbol.name()?.into_owned())
}

/// The refusal a symbol names: the inverse of [`refusal_name`].
///
/// Walks every variant through [`after`], an exhaustive match, so a new
/// variant does not compile until it is placed in the walk.
fn refusal_from(name: &str) -> Option<Refusal> {
    let mut candidate = Some(Refusal::TimeRegressed);
    while let Some(refused) = candidate {
        if refusal_name(refused) == name {
            return Some(refused);
        }
        candidate = after(refused);
    }
    None
}

/// The variant after `refused` in declaration order, `None` after the last.
const fn after(refused: Refusal) -> Option<Refusal> {
    match refused {
        Refusal::TimeRegressed => Some(Refusal::ZeroLead),
        Refusal::ZeroLead => Some(Refusal::EntriesFull),
        Refusal::EntriesFull => Some(Refusal::FlightsFull),
        Refusal::FlightsFull => Some(Refusal::WaitersFull),
        Refusal::WaitersFull => Some(Refusal::Exhausted),
        Refusal::Exhausted => Some(Refusal::Stale),
        Refusal::Stale => None,
    }
}
