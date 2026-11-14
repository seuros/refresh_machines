//! Thin binding to the bounded freshness and refresh-lease policy.
mod answers;
mod init;
mod values;

use std::cell::RefCell;

use magnus::{
    Error, Integer, RArray, Ruby, Symbol, TryConvert, Value, prelude::*, typed_data::Obj,
};
use matryona_machines::ruby::{
    Symbols, duration, exact_fields, integer, keep, nanoseconds, symbol,
};
use periscope_machines::scope_of;
use refresh_machines::{
    FlightId, Forgetting, Lease, Notification, Policy, Refresher as Core, Refusal, State,
};
use values::{lease_id, waiter_id};

/// `RefreshMachines.scope_of(id)`: the scope a lease, waiter or flight id was issued under.
fn scope_of_id(id: Value) -> Result<u16, Error> {
    integer(id, "id").map(scope_of)
}

const KEY_CAPACITY: usize = 256;
const FLIGHT_CAPACITY: usize = 64;
const WAITER_CAPACITY: usize = 256;

/// How much of a `capacity`-long buffer a `max` asks for: past the capacity
/// there is never more to take, so it is the whole buffer.
fn room(max: u64, capacity: usize) -> usize {
    usize::try_from(max).map_or(capacity, |max| max.min(capacity))
}

/// A `RefreshMachines::Policy`: exactly that class, its windows read from
/// the struct (no reader runs) and converted again, since `Marshal.load`
/// skips `initialize`.
#[derive(Clone, Copy)]
struct Rules(Policy);

impl TryConvert for Rules {
    fn try_convert(value: Value) -> Result<Self, Error> {
        let ruby = Ruby::get_with(value);
        let class = answers::classes(&ruby)?.policy.get(&ruby);
        let [fresh, revalidate, if_error] =
            exact_fields(&ruby, value, class, "expected a RefreshMachines::Policy")?;
        Ok(Self(Policy::new(
            duration(fresh, "fresh_for")?,
            duration(revalidate, "stale_while_revalidate")?,
            duration(if_error, "stale_if_error")?,
        )))
    }
}

matryona_machines::symbols! {
    /// A refusal as its Symbol: the snake case of the core variant.
    Refusals for Refusal as "refusal" {
        TimeRegressed => "time_regressed",
        ZeroLead => "zero_lead",
        EntriesFull => "entries_full",
        FlightsFull => "flights_full",
        WaitersFull => "waiters_full",
        Exhausted => "exhausted",
        Stale => "stale",
    }
}

fn refusal(ruby: &Ruby, refused: Refusal) -> Value {
    Refusals::symbol(ruby, refused).as_value()
}

fn done(ruby: &Ruby, outcome: Result<(), Refusal>, name: &str) -> Value {
    match outcome {
        Ok(()) => symbol(ruby, name).as_value(),
        Err(refused) => refusal(ruby, refused),
    }
}

fn state_name(ruby: &Ruby, state: State) -> Symbol {
    symbol(
        ruby,
        match state {
            State::Fresh => "fresh",
            State::Stale => "stale",
            State::Expired => "expired",
        },
    )
}

fn policy_state(ruby: &Ruby, this: Value, age: Value) -> Result<Symbol, Error> {
    let age = duration(age, "age")?;
    Ok(state_name(ruby, Rules::try_convert(this)?.0.state(age)))
}

fn policy_while_revalidate(this: Value, age: Value) -> Result<bool, Error> {
    let age = duration(age, "age")?;
    Ok(Rules::try_convert(this)?.0.while_revalidate(age))
}

fn policy_if_error(this: Value, age: Value) -> Result<bool, Error> {
    let age = duration(age, "age")?;
    Ok(Rules::try_convert(this)?.0.if_error(age))
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

    fn with_scope(scope: Value) -> Result<Self, Error> {
        Ok(Self {
            core: RefCell::new(Core::with_scope(integer(scope, "scope")?)),
        })
    }

    /// Raises `RangeError` when `issued` is beyond the sequence (the core answers `None`).
    fn resumed(ruby: &Ruby, scope: Value, issued: Value) -> Result<Self, Error> {
        let (scope, issued) = (integer(scope, "scope")?, integer(issued, "issued")?);
        Core::resumed(scope, issued)
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
        key: Value,
        now: Value,
        lead_for: Value,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let (key, now) = (integer(key, "key")?, duration(now, "now")?);
        let lead_for = duration(lead_for, "lead_for")?;
        let classes = answers::classes(ruby)?;
        keep(
            ruby,
            this,
            &this.core,
            |core| core.decide(key, now, lead_for),
            |decided| match decided {
                Ok(decision) => classes.decision(ruby, decision),
                Err(refused) => Ok(refusal(ruby, refused)),
            },
        )
    }

    fn store(
        ruby: &Ruby,
        this: Obj<Self>,
        lease: Value,
        now: Value,
        age: Value,
        policy: Value,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let (lease, now) = (integer(lease, "lease")?, duration(now, "now")?);
        let (age, policy) = (duration(age, "age")?, Rules::try_convert(policy)?.0);
        let lease = lease_id(ruby, lease)?;
        let outcome = this.core.borrow_mut().store(lease, now, age, policy);
        Ok(done(ruby, outcome, "stored"))
    }

    fn fail(
        ruby: &Ruby,
        this: Obj<Self>,
        lease: Value,
        now: Value,
        retry_after: Value,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let (lease, now) = (integer(lease, "lease")?, duration(now, "now")?);
        let retry_after = duration(retry_after, "retry_after")?;
        let lease = lease_id(ruby, lease)?;
        let outcome = this.core.borrow_mut().fail(lease, now, retry_after);
        Ok(match outcome {
            Ok(Some(age)) => nanoseconds(ruby, age).as_value(),
            Ok(None) => ruby.qnil().as_value(),
            Err(refused) => refusal(ruby, refused),
        })
    }

    fn abandon(ruby: &Ruby, this: Obj<Self>, lease: Value) -> Result<Value, Error> {
        this.check_frozen()?;
        let lease = integer(lease, "lease")?;
        let lease = lease_id(ruby, lease)?;
        let outcome = this.core.borrow_mut().abandon(lease);
        Ok(done(ruby, outcome, "abandoned"))
    }

    fn reap(ruby: &Ruby, this: Obj<Self>, now: Value) -> Result<Value, Error> {
        this.check_frozen()?;
        let now = duration(now, "now")?;
        let outcome = this.core.borrow_mut().reap(now);
        Ok(match outcome {
            Ok(revoked) => ruby.integer_from_u64(revoked as u64).as_value(),
            Err(refused) => refusal(ruby, refused),
        })
    }

    fn put(
        ruby: &Ruby,
        this: Obj<Self>,
        key: Value,
        now: Value,
        age: Value,
        policy: Value,
    ) -> Result<Value, Error> {
        this.check_frozen()?;
        let (key, now) = (integer(key, "key")?, duration(now, "now")?);
        let (age, policy) = (duration(age, "age")?, Rules::try_convert(policy)?.0);
        let outcome = this.core.borrow_mut().put(key, now, age, policy);
        Ok(done(ruby, outcome, "stored"))
    }

    fn forget(ruby: &Ruby, this: Obj<Self>, key: Value) -> Result<Symbol, Error> {
        this.check_frozen()?;
        let key = integer(key, "key")?;
        let forgetting = this.core.borrow_mut().forget(key);
        Ok(symbol(
            ruby,
            match forgetting {
                Forgetting::Forgotten => "forgotten",
                Forgetting::NotHeld => "not_held",
            },
        ))
    }

    fn sweep(ruby: &Ruby, this: Obj<Self>, now: Value, max: Value) -> Result<Value, Error> {
        this.check_frozen()?;
        let (now, max) = (duration(now, "now")?, integer(max, "max")?);
        let room = room(max, KEY_CAPACITY);
        keep(
            ruby,
            this,
            &this.core,
            |core| {
                let mut buffer = [0_u64; KEY_CAPACITY];
                core.sweep(now, &mut buffer[..room])
                    .map(|written| (buffer, written))
            },
            |swept| {
                let (buffer, written) = match swept {
                    Ok(swept) => swept,
                    Err(refused) => return Ok(refusal(ruby, refused)),
                };
                let keys = ruby.ary_new_capa(written);
                for key in buffer.iter().take(written) {
                    keys.push(*key)?;
                }
                Ok(keys.as_value())
            },
        )
    }

    fn cancel(ruby: &Ruby, this: Obj<Self>, waiter: Value) -> Result<Value, Error> {
        this.check_frozen()?;
        let waiter = integer(waiter, "waiter")?;
        let waiter = waiter_id(ruby, waiter)?;
        let outcome = this.core.borrow_mut().cancel(waiter);
        Ok(done(ruby, outcome, "cancelled"))
    }

    fn poll(ruby: &Ruby, this: Obj<Self>) -> Result<Option<Value>, Error> {
        this.check_frozen()?;
        let classes = answers::classes(ruby)?;
        keep(ruby, this, &this.core, Held::poll, |note| {
            note.map(|note| classes.notification(ruby, note))
                .transpose()
        })
    }

    /// Read into a buffer on the copy, then built, inside one [`keep`].
    fn drain(ruby: &Ruby, this: Obj<Self>, max: Value) -> Result<RArray, Error> {
        this.check_frozen()?;
        let max = integer(max, "max")?;
        let classes = answers::classes(ruby)?;
        let room = room(max, WAITER_CAPACITY);
        keep(
            ruby,
            this,
            &this.core,
            |core| {
                let mut buffer = [Notification::default(); WAITER_CAPACITY];
                let written = core.drain(&mut buffer[..room]);
                (buffer, written)
            },
            |(buffer, written)| {
                let notifications = ruby.ary_new_capa(written);
                for note in buffer.iter().take(written) {
                    notifications.push(classes.notification(ruby, *note)?)?;
                }
                Ok(notifications)
            },
        )
    }

    fn age(ruby: &Ruby, this: &Self, key: Value, now: Value) -> Result<Option<Integer>, Error> {
        let (key, now) = (integer(key, "key")?, duration(now, "now")?);
        let age = this.core.borrow().age(key, now);
        Ok(age.map(|age| nanoseconds(ruby, age)))
    }

    fn state(ruby: &Ruby, this: &Self, key: Value, now: Value) -> Result<Option<Symbol>, Error> {
        let (key, now) = (integer(key, "key")?, duration(now, "now")?);
        let state = this.core.borrow().state(key, now);
        Ok(state.map(|state| state_name(ruby, state)))
    }

    fn stale_if_error(
        ruby: &Ruby,
        this: &Self,
        key: Value,
        now: Value,
    ) -> Result<Option<Integer>, Error> {
        let (key, now) = (integer(key, "key")?, duration(now, "now")?);
        let age = this.core.borrow().stale_if_error(key, now);
        Ok(age.map(|age| nanoseconds(ruby, age)))
    }

    fn leading(&self, key: Value) -> Result<Option<u64>, Error> {
        let key = integer(key, "key")?;
        Ok(self.core.borrow().leading(key).map(Lease::raw))
    }

    fn flight(ruby: &Ruby, this: &Self, lease: Value) -> Result<Option<u64>, Error> {
        let lease = lease_id(ruby, integer(lease, "lease")?)?;
        Ok(this.core.borrow().flight(lease).map(FlightId::raw))
    }

    fn now(ruby: &Ruby, this: &Self) -> Integer {
        nanoseconds(ruby, this.core.borrow().now())
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
