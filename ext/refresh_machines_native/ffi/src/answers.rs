//! The gem's `Data` classes, resolved once at init and pinned, and the values
//! built from what the core answered.
//!
//! `lib/refresh_machines.rb` defines every class before this extension loads.
//! An answer is built natively (allocate, fill the members in order, freeze),
//! which is all `Data#initialize` does past the check, and the core's own
//! answers need no check, so no Ruby code runs while one is built. Mutating
//! calls build on a copy of the refresher kept only once the answer exists.

use std::sync::OnceLock;

use magnus::rb_sys::AsRawValue;
use magnus::value::Opaque;
use magnus::{Error, IntoValue, RClass, RModule, RStruct, Ruby, TryConvert, Value, prelude::*};
use refresh_machines::{Decision, Notification, Outcome, Revalidation};

use crate::{nanos, refusal};

/// The classes, in [`Classes`] field order.
const NAMES: [&str; 9] = [
    "Policy",
    "Fresh",
    "Stale",
    "Fetch",
    "Wait",
    "Lead",
    "Backoff",
    "Unavailable",
    "Notification",
];

static CLASSES: OnceLock<[Opaque<RClass>; 9]> = OnceLock::new();

/// Every `Data` class of the gem, as resolved at init.
#[derive(Clone, Copy)]
pub struct Classes {
    pub policy: RClass,
    pub fresh: RClass,
    pub stale: RClass,
    pub fetch: RClass,
    pub wait: RClass,
    pub lead: RClass,
    pub backoff: RClass,
    pub unavailable: RClass,
    pub notification: RClass,
}

pub fn resolve(ruby: &Ruby, module: RModule) -> Result<(), Error> {
    let mut resolved = Vec::with_capacity(NAMES.len());
    for name in NAMES {
        let class: RClass = module.const_get(name)?;
        // Held in a static, so pinned: `GC.compact` must not move it.
        ruby.gc_register_mark_object(class);
        resolved.push(Opaque::from(class));
    }
    let resolved: [Opaque<RClass>; 9] = resolved
        .try_into()
        .map_err(|_| Error::new(ruby.exception_load_error(), "nine classes, resolved"))?;
    // A second load keeps the first classes: they are the same constants.
    let _ = CLASSES.get_or_init(|| resolved);
    Ok(())
}

pub fn classes(ruby: &Ruby) -> Result<Classes, Error> {
    let held = CLASSES.get().ok_or_else(|| {
        Error::new(
            ruby.exception_load_error(),
            "the refresh_machines extension is not initialised",
        )
    })?;
    let [
        policy,
        fresh,
        stale,
        fetch,
        wait,
        lead,
        backoff,
        unavailable,
        notification,
    ] = held.map(|class| ruby.get_inner(class));
    Ok(Classes {
        policy,
        fresh,
        stale,
        fetch,
        wait,
        lead,
        backoff,
        unavailable,
        notification,
    })
}

/// The members of `value` when it is an instance of exactly `class` (the
/// classes compared by identity, which no Ruby method can fake: a subclass's
/// constant leaks past the seal), read straight from the struct so no reader
/// runs. `None` for anything else. The members are not trusted: callers
/// convert each through the core's types.
pub fn members_of<const N: usize>(
    value: Value,
    class: RClass,
) -> Result<Option<[Value; N]>, Error> {
    if value.class().as_raw() != class.as_raw() {
        return Ok(None);
    }
    let fields = RStruct::try_convert(value)?;
    let mut members = [value; N];
    for (index, member) in members.iter_mut().enumerate() {
        *member = fields.aref(index)?;
    }
    Ok(Some(members))
}

/// A frozen instance of `class` holding `members`, in order.
fn build<const N: usize>(class: RClass, members: [Value; N]) -> Result<Value, Error> {
    let instance = RStruct::try_convert(class.obj_alloc()?)?;
    for (index, member) in members.into_iter().enumerate() {
        instance.aset(index, member)?;
    }
    instance.freeze();
    Ok(instance.as_value())
}

impl Classes {
    pub fn notification(&self, ruby: &Ruby, note: Notification) -> Result<Value, Error> {
        let outcome = ruby.to_symbol(match note.outcome() {
            Outcome::Completed => "completed",
            Outcome::Abandoned => "abandoned",
        });
        let members = [
            note.waiter().raw().into_value_with(ruby),
            note.flight().raw().into_value_with(ruby),
            note.key().into_value_with(ruby),
            outcome.as_value(),
        ];
        build(self.notification, members)
    }

    pub fn decision(&self, ruby: &Ruby, decision: Decision) -> Result<Value, Error> {
        let id = |raw: u64| raw.into_value_with(ruby);
        let span = |duration| nanos(ruby, duration).as_value();
        match decision {
            Decision::Fresh { age } => build(self.fresh, [span(age)]),
            Decision::Stale { age, revalidation } => {
                let revalidation = match revalidation {
                    Revalidation::Lead(lease) => build(self.lead, [id(lease.raw())])?,
                    Revalidation::InFlight => ruby.to_symbol("in_flight").as_value(),
                    Revalidation::Backoff { until } => build(self.backoff, [span(until)])?,
                    Revalidation::Unavailable(refused) => {
                        build(self.unavailable, [refusal(ruby, refused)])?
                    }
                };
                build(self.stale, [span(age), revalidation])
            }
            Decision::Fetch { lease } => build(self.fetch, [id(lease.raw())]),
            Decision::Wait { waiter, flight } => {
                build(self.wait, [id(waiter.raw()), id(flight.raw())])
            }
        }
    }
}
