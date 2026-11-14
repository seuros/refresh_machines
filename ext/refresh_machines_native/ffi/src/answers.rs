//! The gem's `Data` classes, resolved once at init and pinned, and the values
//! built from what the core answered.
//!
//! `lib/refresh_machines.rb` defines every class before this extension loads.
//! An answer is built natively (allocate, fill the members in order, freeze),
//! which is all `Data#initialize` does past the check, and the core's own
//! answers need no check, so no Ruby code runs while one is built. Mutating
//! calls build on a copy of the refresher kept only once the answer exists.

use magnus::{Error, IntoValue, RClass, RModule, RStruct, Ruby, TryConvert, Value, prelude::*};
use matryona_machines::ruby::{Class, Registry, SymbolNames, Symbols, nanoseconds};
use refresh_machines::{Decision, Notification, Outcome, Revalidation};

use crate::refusal;

/// Every `Data` class of the gem, as resolved at init.
pub struct Classes {
    pub policy: Class,
    pub fresh: Class,
    pub stale: Class,
    pub fetch: Class,
    pub wait: Class,
    pub lead: Class,
    pub backoff: Class,
    pub unavailable: Class,
    pub notification: Class,
}

static CLASSES: Registry<Classes> = Registry::new("refresh_machines");

pub fn resolve(ruby: &Ruby, module: RModule) -> Result<(), Error> {
    let class = |name| Class::resolve(ruby, module, name);
    let classes = Classes {
        policy: class("Policy")?,
        fresh: class("Fresh")?,
        stale: class("Stale")?,
        fetch: class("Fetch")?,
        wait: class("Wait")?,
        lead: class("Lead")?,
        backoff: class("Backoff")?,
        unavailable: class("Unavailable")?,
        notification: class("Notification")?,
    };
    CLASSES.set(ruby, classes).map(drop)
}

pub fn classes(ruby: &Ruby) -> Result<&'static Classes, Error> {
    CLASSES.get(ruby)
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
        let outcome = Outcomes::symbol(ruby, note.outcome());
        let members = [
            note.waiter().raw().into_value_with(ruby),
            note.flight().raw().into_value_with(ruby),
            note.key().into_value_with(ruby),
            outcome.as_value(),
        ];
        build(self.notification.get(ruby), members)
    }

    pub fn decision(&self, ruby: &Ruby, decision: Decision) -> Result<Value, Error> {
        let id = |raw: u64| raw.into_value_with(ruby);
        let span = |duration| nanoseconds(ruby, duration).as_value();
        match decision {
            Decision::Fresh { age } => build(self.fresh.get(ruby), [span(age)]),
            Decision::Stale { age, revalidation } => {
                let revalidation = match revalidation {
                    Revalidation::Lead(lease) => build(self.lead.get(ruby), [id(lease.raw())])?,
                    Revalidation::InFlight => {
                        Revalidations::symbol(ruby, &Revalidation::InFlight).as_value()
                    }
                    Revalidation::Backoff { until } => {
                        build(self.backoff.get(ruby), [span(until)])?
                    }
                    Revalidation::Unavailable(refused) => {
                        build(self.unavailable.get(ruby), [refusal(ruby, refused)])?
                    }
                };
                build(self.stale.get(ruby), [span(age), revalidation])
            }
            Decision::Fetch { lease } => build(self.fetch.get(ruby), [id(lease.raw())]),
            Decision::Wait { waiter, flight } => {
                build(self.wait.get(ruby), [id(waiter.raw()), id(flight.raw())])
            }
        }
    }
}

matryona_machines::symbols! {
    /// A `Stale#revalidation`'s kind. `:in_flight` is the one unit variant,
    /// the only one that crosses as a Symbol; the others are `Data` values.
    pub Revalidations for Revalidation as "revalidation" => names_only {
        Lead(..) => "lead",
        InFlight => "in_flight",
        Backoff { .. } => "backoff",
        Unavailable(..) => "unavailable",
    }
}

matryona_machines::symbols! {
    /// How a flight settled, as the Symbol a `Notification#outcome` holds.
    pub Outcomes for Outcome as "outcome" {
        Completed => "completed",
        Abandoned => "abandoned",
    }
}
