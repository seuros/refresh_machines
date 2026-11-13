//! Thin binding to the bounded freshness and refresh-lease policy.
mod init;

use std::{cell::RefCell, time::Duration};

use magnus::{
    Error, Integer, RArray, RClass, Ruby, Symbol, TryConvert, Value, prelude::*, typed_data::Obj,
};
use refresh_machines::{
    Decision, FlightId, Forgetting, Lease, Notification, Outcome, Policy, Refresher as Core,
    Refusal, Revalidation, State, WaiterId,
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

/// An instant or a span: Integer nanoseconds in `0..2**64-1`.
#[derive(Clone, Copy)]
struct Nanos(Duration);

impl TryConvert for Nanos {
    fn try_convert(value: Value) -> Result<Self, Error> {
        if Integer::from_value(value).is_none() {
            return Err(Error::new(
                Ruby::get_with(value).exception_type_error(),
                "expected Integer nanoseconds",
            ));
        }
        u64::try_convert(value).map(|nanos| Self(Duration::from_nanos(nanos)))
    }
}

/// A duration as Integer nanoseconds, exact however large.
fn nanos(ruby: &Ruby, duration: Duration) -> Integer {
    ruby.integer_from_u128(duration.as_nanos())
}

/// A scope: an Integer in `0..=65535`.
#[derive(Clone, Copy)]
struct Scope(u16);

impl TryConvert for Scope {
    fn try_convert(value: Value) -> Result<Self, Error> {
        let ruby = Ruby::get_with(value);
        if Integer::from_value(value).is_none() {
            return Err(Error::new(
                ruby.exception_type_error(),
                "scope must be an Integer",
            ));
        }
        u16::try_convert(value)
            .map(Self)
            .map_err(|_| Error::new(ruby.exception_range_error(), "scope must be in 0..=65535"))
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
        let field = |name: &str| -> Result<Duration, Error> {
            let raw: Value = value.funcall(name, ())?;
            Nanos::try_convert(raw).map(|nanos| nanos.0)
        };
        Ok(Self(Policy::new(
            field("fresh_for")?,
            field("stale_while_revalidate")?,
            field("stale_if_error")?,
        )))
    }
}

/// Ruby symbol name of a refusal: the snake case of the core variant.
const fn refusal_name(refused: Refusal) -> &'static str {
    match refused {
        Refusal::TimeRegressed => "time_regressed",
        Refusal::Rejected => "rejected",
        Refusal::EntriesFull => "entries_full",
        Refusal::FlightsFull => "flights_full",
        Refusal::WaitersFull => "waiters_full",
        Refusal::Exhausted => "exhausted",
        Refusal::Stale => "stale",
    }
}

fn refusal(ruby: &Ruby, refused: Refusal) -> Value {
    ruby.to_symbol(refusal_name(refused)).as_value()
}

fn done(ruby: &Ruby, outcome: Result<(), Refusal>, name: &str) -> Value {
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

fn notification(ruby: &Ruby, class: Value, note: Notification) -> Result<Value, Error> {
    let outcome = ruby.to_symbol(match note.outcome {
        Outcome::Completed => "completed",
        Outcome::Abandoned => "abandoned",
    });
    class.funcall(
        "new",
        (note.waiter.raw(), note.flight.raw(), note.key, outcome),
    )
}

fn policy_state(ruby: &Ruby, this: Value, age: Nanos) -> Result<Symbol, Error> {
    Ok(state_name(ruby, Rules::try_convert(this)?.0.state(age.0)))
}

fn policy_while_revalidate(this: Value, age: Nanos) -> Result<bool, Error> {
    Ok(Rules::try_convert(this)?.0.while_revalidate(age.0))
}

fn policy_if_error(this: Value, age: Nanos) -> Result<bool, Error> {
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

    const fn with_scope(scope: Scope) -> Self {
        Self {
            core: RefCell::new(Core::with_scope(scope.0)),
        }
    }

    fn scope(&self) -> u16 {
        self.core.borrow().scope()
    }

    fn decide(
        ruby: &Ruby,
        this: Obj<Self>,
        key: Number,
        now: Nanos,
        lead_for: Nanos,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        // Resolve the classes before deciding, so a failure registers nothing.
        let class = |name| data(ruby, name);
        let classes = (
            class("Fresh")?,
            class("Stale")?,
            class("Fetch")?,
            class("Wait")?,
        );
        let parts = (class("Lead")?, class("Backoff")?, class("Unavailable")?);
        let decision = this.core.borrow_mut().decide(key.0, now.0, lead_for.0);
        match decision {
            Ok(Decision::Fresh { age }) => classes.0.funcall("new", (nanos(ruby, age),)),
            Ok(Decision::Stale { age, revalidation }) => {
                let revalidation: Value = match revalidation {
                    Revalidation::Lead(lease) => parts.0.funcall("new", (lease.raw(),))?,
                    Revalidation::InFlight => ruby.to_symbol("in_flight").as_value(),
                    Revalidation::Backoff { until } => {
                        parts.1.funcall("new", (nanos(ruby, until),))?
                    }
                    Revalidation::Unavailable(refused) => {
                        parts.2.funcall("new", (refusal(ruby, refused),))?
                    }
                };
                classes.1.funcall("new", (nanos(ruby, age), revalidation))
            }
            Ok(Decision::Fetch { lease }) => classes.2.funcall("new", (lease.raw(),)),
            Ok(Decision::Wait { waiter, flight }) => {
                classes.3.funcall("new", (waiter.raw(), flight.raw()))
            }
            Err(refused) => Ok(refusal(ruby, refused)),
        }
    }

    fn store(
        ruby: &Ruby,
        this: Obj<Self>,
        lease: Number,
        now: Nanos,
        age: Nanos,
        policy: Rules,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome =
            this.core
                .borrow_mut()
                .store(Lease::from_raw(lease.0), now.0, age.0, policy.0);
        Ok(done(ruby, outcome, "stored"))
    }

    fn fail(
        ruby: &Ruby,
        this: Obj<Self>,
        lease: Number,
        now: Nanos,
        retry_after: Nanos,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome = this
            .core
            .borrow_mut()
            .fail(Lease::from_raw(lease.0), now.0, retry_after.0);
        Ok(match outcome {
            Ok(Some(age)) => nanos(ruby, age).as_value(),
            Ok(None) => ruby.qnil().as_value(),
            Err(refused) => refusal(ruby, refused),
        })
    }

    fn abandon(ruby: &Ruby, this: Obj<Self>, lease: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome = this.core.borrow_mut().abandon(Lease::from_raw(lease.0));
        Ok(done(ruby, outcome, "abandoned"))
    }

    fn reap(ruby: &Ruby, this: Obj<Self>, now: Nanos) -> Result<Value, Error> {
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
        now: Nanos,
        age: Nanos,
        policy: Rules,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome = this.core.borrow_mut().put(key.0, now.0, age.0, policy.0);
        Ok(done(ruby, outcome, "stored"))
    }

    fn forget(ruby: &Ruby, this: Obj<Self>, key: Number) -> Result<Symbol, Error> {
        this.check_frozen()?;
        let forgetting = this.core.borrow_mut().forget(key.0);
        Ok(ruby.to_symbol(match forgetting {
            Forgetting::Forgotten => "forgotten",
            Forgetting::NotHeld => "not_held",
        }))
    }

    fn sweep(ruby: &Ruby, this: Obj<Self>, now: Nanos, max: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let room = usize::try_from(max.0).map_or(KEY_CAPACITY, |max| max.min(KEY_CAPACITY));
        let mut buffer = [0_u64; KEY_CAPACITY];
        let outcome = this.core.borrow_mut().sweep(now.0, &mut buffer[..room]);
        let written = match outcome {
            Ok(written) => written,
            Err(refused) => return Ok(refusal(ruby, refused)),
        };
        let keys = ruby.ary_new_capa(written);
        for key in buffer.iter().take(written) {
            keys.push(*key)?;
        }
        Ok(keys.as_value())
    }

    fn cancel(ruby: &Ruby, this: Obj<Self>, waiter: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let outcome = this.core.borrow_mut().cancel(WaiterId::from_raw(waiter.0));
        Ok(done(ruby, outcome, "cancelled"))
    }

    fn poll(ruby: &Ruby, this: Obj<Self>) -> Result<Option<Value>, Error> {
        this.check_frozen()?;
        // Resolve the class before taking anything, so a failure loses nothing.
        let class = data(ruby, "Notification")?;
        let polled = this.core.borrow_mut().poll();
        polled
            .map(|note| notification(ruby, class, note))
            .transpose()
    }

    fn drain(ruby: &Ruby, this: Obj<Self>, max: Number) -> Result<RArray, Error> {
        this.check_frozen()?;
        // Resolve the class before taking anything, so a failure loses nothing.
        let class = data(ruby, "Notification")?;
        let room = usize::try_from(max.0).map_or(WAITER_CAPACITY, |max| max.min(WAITER_CAPACITY));
        let mut buffer = [Notification::default(); WAITER_CAPACITY];
        let written = this.core.borrow_mut().drain(&mut buffer[..room]);
        let notifications = ruby.ary_new_capa(written);
        for note in buffer.iter().take(written) {
            notifications.push(notification(ruby, class, *note)?)?;
        }
        Ok(notifications)
    }

    fn age(ruby: &Ruby, this: &Self, key: Number, now: Nanos) -> Option<Integer> {
        let age = this.core.borrow().age(key.0, now.0)?;
        Some(nanos(ruby, age))
    }

    fn state(ruby: &Ruby, this: &Self, key: Number, now: Nanos) -> Option<Symbol> {
        let state = this.core.borrow().state(key.0, now.0)?;
        Some(state_name(ruby, state))
    }

    fn stale_if_error(ruby: &Ruby, this: &Self, key: Number, now: Nanos) -> Option<Integer> {
        let age = this.core.borrow().stale_if_error(key.0, now.0)?;
        Some(nanos(ruby, age))
    }

    fn leading(&self, key: Number) -> Option<u64> {
        self.core.borrow().leading(key.0).map(Lease::raw)
    }

    fn flight(&self, lease: Number) -> Option<u64> {
        self.core
            .borrow()
            .flight(Lease::from_raw(lease.0))
            .map(FlightId::raw)
    }

    fn now(ruby: &Ruby, this: &Self) -> Integer {
        nanos(ruby, this.core.borrow().now())
    }

    fn size(&self) -> usize {
        self.core.borrow().len()
    }

    fn is_empty(&self) -> bool {
        self.core.borrow().is_empty()
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
