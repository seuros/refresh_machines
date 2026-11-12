//! Thin binding to the bounded freshness and refresh-lease policy.
mod init;

use std::cell::RefCell;

use magnus::{
    Error, Integer, RArray, RClass, Ruby, Symbol, TryConvert, Value, prelude::*, typed_data::Obj,
};
use refresh_machines::{
    Decision, Lease, Notification, Outcome, Policy, Refresher as Core, Refused, Revalidation,
    State, WaiterId,
};

const KEY_CAPACITY: usize = 256;
const FLIGHT_CAPACITY: usize = 64;
const WAITER_CAPACITY: usize = 256;

#[derive(Clone, Copy)]
struct Number(u64);

impl TryConvert for Number {
    fn try_convert(value: Value) -> Result<Self, Error> {
        if Integer::from_value(value).is_none() {
            return Err(Error::new(
                Ruby::get_with(value).exception_type_error(),
                "expected an Integer (unsigned 64-bit)",
            ));
        }
        u64::try_convert(value).map(Self)
    }
}

/// A `RefreshMachines::Policy`, read field by field so every one is validated.
#[derive(Clone, Copy)]
struct Rules(Policy);

impl TryConvert for Rules {
    fn try_convert(value: Value) -> Result<Self, Error> {
        let ruby = Ruby::get_with(value);
        let class: RClass = ruby.define_module("RefreshMachines")?.const_get("Policy")?;
        if !value.is_kind_of(class) {
            return Err(Error::new(
                ruby.exception_type_error(),
                "expected a RefreshMachines::Policy",
            ));
        }
        let field = |name: &str| -> Result<u64, Error> {
            let raw: Value = value.funcall(name, ())?;
            Number::try_convert(raw).map(|number| number.0)
        };
        Ok(Self(Policy::new(
            field("fresh_for")?,
            field("stale_while_revalidate")?,
            field("stale_if_error")?,
        )))
    }
}

const fn refusal_name(refused: Refused) -> &'static str {
    match refused {
        Refused::TimeRegressed => "time_regressed",
        Refused::Rejected => "rejected",
        Refused::EntriesFull => "entries_full",
        Refused::FlightsFull => "flights_full",
        Refused::WaitersFull => "waiters_full",
        Refused::Exhausted => "exhausted",
        _ => "stale",
    }
}

fn refusal(ruby: &Ruby, refused: Refused) -> Value {
    ruby.to_symbol(refusal_name(refused)).as_value()
}

fn done(ruby: &Ruby, outcome: Result<(), Refused>, name: &str) -> Value {
    match outcome {
        Ok(()) => ruby.to_symbol(name).as_value(),
        Err(refused) => refusal(ruby, refused),
    }
}

fn state_name(ruby: &Ruby, state: State) -> Symbol {
    ruby.to_symbol(match state {
        State::Fresh => "fresh",
        State::Stale => "stale",
        State::Expired => "expired",
    })
}

fn data(ruby: &Ruby, name: &str) -> Result<Value, Error> {
    ruby.define_module("RefreshMachines")?.const_get(name)
}

fn policy_state(ruby: &Ruby, this: Value, age: Number) -> Result<Symbol, Error> {
    Ok(state_name(ruby, Rules::try_convert(this)?.0.state(age.0)))
}

fn policy_while_revalidate(this: Value, age: Number) -> Result<bool, Error> {
    Ok(Rules::try_convert(this)?.0.while_revalidate(age.0))
}

fn policy_if_error(this: Value, age: Number) -> Result<bool, Error> {
    Ok(Rules::try_convert(this)?.0.if_error(age.0))
}

#[magnus::wrap(class = "RefreshMachines::Refresher", free_immediately, size)]
struct Refresher {
    core: RefCell<Core<KEY_CAPACITY, FLIGHT_CAPACITY, WAITER_CAPACITY>>,
}

impl Refresher {
    const fn new() -> Self {
        Self {
            core: RefCell::new(Core::new()),
        }
    }

    fn decide(
        ruby: &Ruby,
        this: Obj<Self>,
        key: Number,
        now: Number,
        lead_for: Number,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        // Resolve the classes before deciding, so a failure registers nothing.
        let classes = (
            data(ruby, "Fresh")?,
            data(ruby, "Stale")?,
            data(ruby, "Fetch")?,
            data(ruby, "Wait")?,
        );
        let decision = this.core.borrow_mut().decide(key.0, now.0, lead_for.0);
        match decision {
            Ok(Decision::Fresh { age }) => classes.0.funcall("new", (age,)),
            Ok(Decision::Stale { age, revalidation }) => {
                let (name, lease, retry_at) = match revalidation {
                    Revalidation::Lead(lease) => ("lead", Some(lease.raw()), None),
                    Revalidation::InFlight => ("in_flight", None, None),
                    Revalidation::Backoff { until } => ("backoff", None, Some(until)),
                    Revalidation::Unavailable(refused) => (refusal_name(refused), None, None),
                };
                classes
                    .1
                    .funcall("new", (age, ruby.to_symbol(name), lease, retry_at))
            }
            Ok(Decision::Fetch { lease }) => classes.2.funcall("new", (lease.raw(), key.0)),
            Ok(Decision::Wait { waiter }) => classes.3.funcall("new", (waiter.raw(), key.0)),
            Err(refused) => Ok(refusal(ruby, refused)),
        }
    }

    fn store(
        ruby: &Ruby,
        this: Obj<Self>,
        lease_id: Number,
        now: Number,
        age: Number,
        policy: Rules,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome =
            this.core
                .borrow_mut()
                .store(Lease::from_raw(lease_id.0), now.0, age.0, policy.0);
        Ok(done(ruby, outcome, "stored"))
    }

    fn fail(
        ruby: &Ruby,
        this: Obj<Self>,
        lease_id: Number,
        now: Number,
        retry_after: Number,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome =
            this.core
                .borrow_mut()
                .fail(Lease::from_raw(lease_id.0), now.0, retry_after.0);
        Ok(match outcome {
            Ok(Some(age)) => ruby.integer_from_u64(age).as_value(),
            Ok(None) => ruby.qnil().as_value(),
            Err(refused) => refusal(ruby, refused),
        })
    }

    fn abandon(ruby: &Ruby, this: Obj<Self>, lease_id: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome = this.core.borrow_mut().abandon(Lease::from_raw(lease_id.0));
        Ok(done(ruby, outcome, "abandoned"))
    }

    fn reap(ruby: &Ruby, this: Obj<Self>, now: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome = this.core.borrow_mut().reap(now.0);
        Ok(match outcome {
            Ok(revoked) => ruby.integer_from_u64(revoked as u64).as_value(),
            Err(refused) => refusal(ruby, refused),
        })
    }

    fn put(
        ruby: &Ruby,
        this: Obj<Self>,
        key: Number,
        now: Number,
        age: Number,
        policy: Rules,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome = this.core.borrow_mut().put(key.0, now.0, age.0, policy.0);
        Ok(done(ruby, outcome, "stored"))
    }

    fn forget(this: Obj<Self>, key: Number) -> Result<bool, Error> {
        this.check_frozen()?;
        Ok(this.core.borrow_mut().forget(key.0))
    }

    fn sweep(ruby: &Ruby, this: Obj<Self>, now: Number, max: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let room = usize::try_from(max.0).map_or(KEY_CAPACITY, |max| max.min(KEY_CAPACITY));
        let mut buffer: [Option<u64>; KEY_CAPACITY] = [None; KEY_CAPACITY];
        let outcome = this.core.borrow_mut().sweep(now.0, &mut buffer[..room]);
        let written = match outcome {
            Ok(written) => written,
            Err(refused) => return Ok(refusal(ruby, refused)),
        };
        let keys = ruby.ary_new_capa(written);
        for key in buffer.iter().take(written).flatten() {
            keys.push(*key)?;
        }
        Ok(keys.as_value())
    }

    fn cancel(ruby: &Ruby, this: Obj<Self>, waiter_id: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome = this
            .core
            .borrow_mut()
            .cancel(WaiterId::from_raw(waiter_id.0));
        Ok(done(ruby, outcome, "cancelled"))
    }

    fn drain(ruby: &Ruby, this: Obj<Self>, max: Number) -> Result<RArray, Error> {
        this.check_frozen()?;
        // Resolve the class before taking anything, so a failure loses nothing.
        let class = data(ruby, "Notification")?;
        let room = usize::try_from(max.0).map_or(WAITER_CAPACITY, |max| max.min(WAITER_CAPACITY));
        let mut buffer: [Option<Notification>; WAITER_CAPACITY] = [None; WAITER_CAPACITY];
        let written = this.core.borrow_mut().drain(&mut buffer[..room]);
        let notifications = ruby.ary_new_capa(written);
        for note in buffer.iter().take(written).flatten() {
            let outcome = ruby.to_symbol(match note.outcome {
                Outcome::Completed => "completed",
                Outcome::Abandoned => "abandoned",
            });
            let value: Value = class.funcall("new", (note.waiter.raw(), note.key, outcome))?;
            notifications.push(value)?;
        }
        Ok(notifications)
    }

    fn age(&self, key: Number, now: Number) -> Option<u64> {
        self.core.borrow().age(key.0, now.0)
    }

    fn state(ruby: &Ruby, this: &Self, key: Number, now: Number) -> Option<Symbol> {
        let state = this.core.borrow().state(key.0, now.0)?;
        Some(state_name(ruby, state))
    }

    fn stale_if_error(&self, key: Number, now: Number) -> Option<u64> {
        self.core.borrow().stale_if_error(key.0, now.0)
    }

    fn leading(&self, key: Number) -> Option<u64> {
        self.core.borrow().leading(key.0).map(Lease::raw)
    }

    fn now(&self) -> u64 {
        self.core.borrow().now()
    }

    fn size(&self) -> usize {
        self.core.borrow().len()
    }

    fn in_flight(&self) -> usize {
        self.core.borrow().in_flight()
    }

    fn waiting(&self) -> usize {
        self.core.borrow().waiting()
    }

    fn pending(&self) -> usize {
        self.core.borrow().pending()
    }

    const fn capacity() -> usize {
        KEY_CAPACITY
    }

    const fn flight_capacity() -> usize {
        FLIGHT_CAPACITY
    }

    const fn waiter_capacity() -> usize {
        WAITER_CAPACITY
    }
}
