//! Private native entry point.
use crate::{
    FLIGHT_CAPACITY, KEY_CAPACITY, Refresher, WAITER_CAPACITY, policy_if_error, policy_state,
    policy_while_revalidate,
};
use magnus::{Error, RClass, Ruby, function, method, prelude::*};

#[magnus::init]
fn init(ruby: &Ruby) -> Result<(), Error> {
    let module = ruby.define_module("RefreshMachines")?;
    // Policy is a Data class defined in Ruby; its arithmetic lives here.
    let policy: RClass = module.const_get("Policy")?;
    policy.define_method("state", method!(policy_state, 1))?;
    policy.define_method("while_revalidate?", method!(policy_while_revalidate, 1))?;
    policy.define_method("if_error?", method!(policy_if_error, 1))?;
    let class = module.define_class("Refresher", ruby.class_object())?;
    class.undef_default_alloc_func();
    class.const_set("CAPACITY", KEY_CAPACITY)?;
    class.const_set("FLIGHT_CAPACITY", FLIGHT_CAPACITY)?;
    class.const_set("WAITER_CAPACITY", WAITER_CAPACITY)?;
    class.define_singleton_method("new", function!(Refresher::new, 0))?;
    class.define_method("decide", method!(Refresher::decide, 3))?;
    class.define_method("store", method!(Refresher::store, 4))?;
    class.define_method("fail", method!(Refresher::fail, 3))?;
    class.define_method("abandon", method!(Refresher::abandon, 1))?;
    class.define_method("reap", method!(Refresher::reap, 1))?;
    class.define_method("put", method!(Refresher::put, 4))?;
    class.define_method("forget", method!(Refresher::forget, 1))?;
    class.define_method("sweep", method!(Refresher::sweep, 2))?;
    class.define_method("cancel", method!(Refresher::cancel, 1))?;
    class.define_method("drain", method!(Refresher::drain, 1))?;
    class.define_method("age", method!(Refresher::age, 2))?;
    class.define_method("state", method!(Refresher::state, 2))?;
    class.define_method("stale_if_error", method!(Refresher::stale_if_error, 2))?;
    class.define_method("leading", method!(Refresher::leading, 1))?;
    class.define_method("now", method!(Refresher::now, 0))?;
    class.define_method("size", method!(Refresher::size, 0))?;
    class.define_method("in_flight", method!(Refresher::in_flight, 0))?;
    class.define_method("waiting", method!(Refresher::waiting, 0))?;
    class.define_method("pending", method!(Refresher::pending, 0))?;
    class.define_method("capacity", function!(Refresher::capacity, 0))?;
    class.define_method("flight_capacity", function!(Refresher::flight_capacity, 0))?;
    class.define_method("waiter_capacity", function!(Refresher::waiter_capacity, 0))?;
    Ok(())
}
