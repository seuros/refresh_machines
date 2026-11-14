//! Thin binding to the bounded freshness and refresh-lease policy.
mod answers;
mod init;
mod values;

use std::{cell::RefCell, time::Duration};

use magnus::{
    Error, Integer, RArray, Ruby, Symbol, TryConvert, Value, prelude::*, typed_data::Obj,
};
use periscope_machines::scope_of;
use refresh_machines::{
    FlightId, Forgetting, Lease, Notification, Policy, Refresher as Core, Refusal, State,
};
use values::{lease_id, waiter_id};

/// `RefreshMachines.scope_of(id)`: the scope a lease, waiter or flight id was issued under.
const fn scope_of_id(id: Number) -> u16 {
    scope_of(id.0)
}

const KEY_CAPACITY: usize = 256;
const FLIGHT_CAPACITY: usize = 64;
const WAITER_CAPACITY: usize = 256;

#[derive(Clone, Copy)]
struct Number(u64);

/// `TypeError` unless `value` is an Integer; its range is the caller's to check.
fn integer(value: Value, message: &'static str) -> Result<(), Error> {
    if Integer::from_value(value).is_some() {
        return Ok(());
    }
    Err(Error::new(
        Ruby::get_with(value).exception_type_error(),
        message,
    ))
}

impl TryConvert for Number {
    fn try_convert(value: Value) -> Result<Self, Error> {
        integer(value, "expected an Integer (unsigned 64-bit)")?;
        u64::try_convert(value).map(Self)
    }
}

/// The `TypeError` message for an instant or span that is not an Integer.
const NANOS_TYPE: &str = "expected Integer nanoseconds";

/// An instant or a span: Integer nanoseconds in `0..2**64-1`.
#[derive(Clone, Copy)]
struct Nanos(Duration);

impl TryConvert for Nanos {
    fn try_convert(value: Value) -> Result<Self, Error> {
        integer(value, NANOS_TYPE)?;
        u64::try_convert(value).map(|nanos| Self(Duration::from_nanos(nanos)))
    }
}

/// How much of a `capacity`-long buffer a `max` asks for: past the capacity
/// there is never more to take, so it is the whole buffer.
fn room(max: Number, capacity: usize) -> usize {
    usize::try_from(max.0).map_or(capacity, |max| max.min(capacity))
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
        integer(value, "scope must be an Integer")?;
        u16::try_convert(value).map(Self).map_err(|_| {
            let ruby = Ruby::get_with(value);
            Error::new(ruby.exception_range_error(), "scope must be in 0..=65535")
        })
    }
}

/// A `RefreshMachines::Policy`: exactly that class (see
/// [`answers::members_of`]), each window converted again, since
/// `Marshal.load` skips `initialize`.
#[derive(Clone, Copy)]
struct Rules(Policy);

impl TryConvert for Rules {
    fn try_convert(value: Value) -> Result<Self, Error> {
        let ruby = Ruby::get_with(value);
        let Some([fresh, revalidate, if_error]) =
            answers::members_of(value, answers::classes(&ruby)?.policy)?
        else {
            return Err(Error::new(
                ruby.exception_type_error(),
                "expected a RefreshMachines::Policy",
            ));
        };
        let window = |raw: Value| Nanos::try_convert(raw).map(|nanos| nanos.0);
        Ok(Self(Policy::new(
            window(fresh)?,
            window(revalidate)?,
            window(if_error)?,
        )))
    }
}

/// Ruby symbol name of a refusal: the snake case of the core variant.
const fn refusal_name(refused: Refusal) -> &'static str {
    match refused {
        Refusal::TimeRegressed => "time_regressed",
        Refusal::ZeroLead => "zero_lead",
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

fn policy_state(ruby: &Ruby, this: Value, age: Nanos) -> Result<Symbol, Error> {
    Ok(state_name(ruby, Rules::try_convert(this)?.0.state(age.0)))
}

fn policy_while_revalidate(this: Value, age: Nanos) -> Result<bool, Error> {
    Ok(Rules::try_convert(this)?.0.while_revalidate(age.0))
}

fn policy_if_error(this: Value, age: Nanos) -> Result<bool, Error> {
    Ok(Rules::try_convert(this)?.0.if_error(age.0))
}

type Held = Core<KEY_CAPACITY, FLIGHT_CAPACITY, WAITER_CAPACITY>;

#[magnus::wrap(class = "RefreshMachines::Refresher", free_immediately, size)]
struct Refresher {
    core: RefCell<Held>,
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

    /// Raises `RangeError` when `issued` is beyond the sequence (the core answers `None`).
    fn resumed(ruby: &Ruby, scope: Scope, issued: Number) -> Result<Self, Error> {
        Core::resumed(scope.0, issued.0)
            .map(|core| Self {
                core: RefCell::new(core),
            })
            .ok_or_else(|| {
                Error::new(
                    ruby.exception_range_error(),
                    "issued is past the last sequence of a scope (2**48 - 1)",
                )
            })
    }

    fn scope(&self) -> u16 {
        self.core.borrow().scope()
    }

    fn issued(&self) -> u64 {
        self.core.borrow().issued()
    }

    fn decide(
        ruby: &Ruby,
        this: Obj<Self>,
        key: Number,
        now: Nanos,
        lead_for: Nanos,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let classes = answers::classes(ruby)?;
        Self::on_copy(ruby, this, |core| {
            match core.decide(key.0, now.0, lead_for.0) {
                Ok(decision) => classes.decision(ruby, decision),
                Err(refused) => Ok(refusal(ruby, refused)),
            }
        })
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
        let lease = lease_id(ruby, lease)?;
        let outcome = this.core.borrow_mut().store(lease, now.0, age.0, policy.0);
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
        let lease = lease_id(ruby, lease)?;
        let outcome = this.core.borrow_mut().fail(lease, now.0, retry_after.0);
        Ok(match outcome {
            Ok(Some(age)) => nanos(ruby, age).as_value(),
            Ok(None) => ruby.qnil().as_value(),
            Err(refused) => refusal(ruby, refused),
        })
    }

    fn abandon(ruby: &Ruby, this: Obj<Self>, lease: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let lease = lease_id(ruby, lease)?;
        let outcome = this.core.borrow_mut().abandon(lease);
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
        let room = room(max, KEY_CAPACITY);
        Self::on_copy(ruby, this, |core| {
            let mut buffer = [0_u64; KEY_CAPACITY];
            let written = match core.sweep(now.0, &mut buffer[..room]) {
                Ok(written) => written,
                Err(refused) => return Ok(refusal(ruby, refused)),
            };
            let keys = ruby.ary_new_capa(written);
            for key in buffer.iter().take(written) {
                keys.push(*key)?;
            }
            Ok(keys.as_value())
        })
    }

    fn cancel(ruby: &Ruby, this: Obj<Self>, waiter: Number) -> Result<Value, Error> {
        this.check_frozen()?;
        let waiter = waiter_id(ruby, waiter)?;
        let outcome = this.core.borrow_mut().cancel(waiter);
        Ok(done(ruby, outcome, "cancelled"))
    }

    fn poll(ruby: &Ruby, this: Obj<Self>) -> Result<Option<Value>, Error> {
        this.check_frozen()?;
        let classes = answers::classes(ruby)?;
        Self::on_copy(ruby, this, |core| {
            core.poll()
                .map(|note| classes.notification(ruby, note))
                .transpose()
        })
    }

    /// Read into a buffer, then built, inside one [`Self::on_copy`].
    fn drain(ruby: &Ruby, this: Obj<Self>, max: Number) -> Result<RArray, Error> {
        this.check_frozen()?;
        let classes = answers::classes(ruby)?;
        let room = room(max, WAITER_CAPACITY);
        Self::on_copy(ruby, this, |core| {
            let mut buffer = [Notification::default(); WAITER_CAPACITY];
            let written = core.drain(&mut buffer[..room]);
            let notifications = ruby.ary_new_capa(written);
            for note in buffer.iter().take(written) {
                notifications.push(classes.notification(ruby, *note)?)?;
            }
            Ok(notifications)
        })
    }

    /// Runs `act`, which builds a Ruby answer, on a copy of the refresher and
    /// keeps the copy only once the answer exists. The receiver is checked
    /// frozen up front and again right before the store: frozen meanwhile
    /// raises `FrozenError`, changed meanwhile (a call back in while the
    /// answer was built) raises `RuntimeError`, and in both cases nothing is
    /// applied. No borrow is held while `act` runs.
    fn on_copy<R>(
        ruby: &Ruby,
        this: Obj<Self>,
        act: impl FnOnce(&mut Held) -> Result<R, Error>,
    ) -> Result<R, Error> {
        this.check_frozen()?;
        let before = this.core.borrow().clone();
        let mut after = before.clone();
        let answer = act(&mut after)?;
        this.check_frozen()?;
        let mut held = this.core.borrow_mut();
        if *held != before {
            return Err(Error::new(
                ruby.exception_runtime_error(),
                "the refresher changed while the answer was built; the call was not applied",
            ));
        }
        *held = after;
        Ok(answer)
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

    fn flight(ruby: &Ruby, this: &Self, lease: Number) -> Result<Option<u64>, Error> {
        let lease = lease_id(ruby, lease)?;
        Ok(this.core.borrow().flight(lease).map(FlightId::raw))
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
