# frozen_string_literal: true
require 'minitest/autorun'
require 'refresh_machines'

class RefresherTest < Minitest::Test
  R = RefreshMachines
  RULES = R::Policy.new(10, 5, 20)
  MAX = 2**64 - 1
  S = 1_000_000_000

  def setup
    @refresher = R::Refresher.new
  end

  def test_policy_classifies_explicit_ages
    assert_equal %i[fresh fresh stale stale expired], [0, 9, 10, 29, 30].map { RULES.state(it) }
    assert_equal [false, true, true, false], [9, 10, 14, 15].map { RULES.while_revalidate?(it) }
    assert_equal [false, true, true, false], [9, 10, 29, 30].map { RULES.if_error?(it) }
    assert_equal :expired, R::Policy.new(10, 0, 0).state(10)
    assert_equal :stale, R::Policy.new(MAX, MAX, MAX).state(MAX)
    assert_equal R::Policy.new(fresh_for: 10, stale_while_revalidate: 5, stale_if_error: 20), RULES
  end

  def test_time_is_integer_nanoseconds_in_and_out
    now = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
    assert_equal :stored, @refresher.put(1, now, 0, R::Policy.new(60 * S, 30 * S, 300 * S))
    assert_equal R::Fresh.new((60 * S) - 1), @refresher.decide(1, now + (60 * S) - 1, 10 * S)
    lead = @refresher.decide(1, now + (60 * S), 10 * S)
    lease = lead.revalidation.lease
    assert_equal R::Stale.new(60 * S, R::Lead.new(lease)), lead
    assert_equal (60 * S) + 1, @refresher.fail(lease, now + (60 * S) + 1, 5 * S)
    quiet = @refresher.decide(1, now + (61 * S), 10 * S)
    assert_equal R::Stale.new(61 * S, R::Backoff.new(now + (65 * S) + 1)), quiet
    assert_equal now + (61 * S), @refresher.now
    assert_equal 61 * S, @refresher.stale_if_error(1, now + (61 * S))
    assert_equal [61 * S, :stale], [@refresher.age(1, now + (61 * S)), @refresher.state(1, now + (61 * S))]
    [@refresher.now, quiet.age, quiet.revalidation.until].each { assert_kind_of Integer, it }
  end

  def test_sums_past_the_argument_range_are_exact_and_never_wrap
    @refresher.put(1, 0, MAX, R::Policy.new(MAX, MAX, MAX))
    assert_equal [MAX, :stale], [@refresher.age(1, 0), @refresher.state(1, 0)]
    assert_equal [2 * MAX, :expired], [@refresher.age(1, MAX), @refresher.state(1, MAX)]
    @refresher.put(2, MAX, 1, R::Policy.new(1, MAX, MAX))
    lead = @refresher.decide(2, MAX, MAX)
    assert_equal 1, @refresher.fail(lead.revalidation.lease, MAX, MAX)
    assert_equal R::Stale.new(1, R::Backoff.new(2 * MAX)), @refresher.decide(2, MAX, 1)
    assert_equal MAX, @refresher.now
  end

  def test_miss_is_fetched_once_and_waiters_are_woken
    fetch = @refresher.decide(42, 0, 100)
    assert_equal R::Fetch.new(fetch.lease), fetch
    assert_equal fetch.lease, @refresher.leading(42)
    a = @refresher.decide(42, 1, 100)
    b = @refresher.decide(42, 2, 100)
    assert_instance_of R::Wait, a
    refute_equal a.waiter, b.waiter
    assert_equal R::Wait.new(a.waiter, @refresher.flight(fetch.lease)), a
    assert_equal a.flight, b.flight
    assert_equal [1, 2, 0, 0], [@refresher.in_flight, @refresher.waiting, @refresher.pending, @refresher.size]
    assert_empty @refresher.drain(10)
    flight = @refresher.flight(fetch.lease)
    assert_kind_of Integer, flight

    assert_equal :stored, @refresher.store(fetch.lease, 3, 0, RULES)
    assert_equal :stale, @refresher.store(fetch.lease, 3, 0, RULES)
    assert_equal [0, 0, 2, 1], [@refresher.in_flight, @refresher.waiting, @refresher.pending, @refresher.size]
    assert_nil @refresher.leading(42)
    assert_nil @refresher.flight(fetch.lease)
    assert_equal [R::Notification.new(a.waiter, flight, 42, :completed)], @refresher.drain(1)
    assert_equal [R::Notification.new(b.waiter, flight, 42, :completed)], @refresher.drain(10)
    assert_equal R::Fresh.new(1), @refresher.decide(42, 4, 100)
    assert_equal 4, @refresher.now
  end

  def test_stale_while_revalidate_serves_and_elects_one_leader
    assert_equal :stored, @refresher.put(7, 0, 0, RULES)
    assert_equal R::Fresh.new(9), @refresher.decide(7, 9, 100)
    lead = @refresher.decide(7, 10, 100)
    lease = lead.revalidation.lease
    assert_equal R::Stale.new(10, R::Lead.new(lease)), lead
    assert_kind_of Integer, lease
    assert_equal R::Stale.new(12, :in_flight), @refresher.decide(7, 12, 100)
    assert_equal 0, @refresher.waiting
    assert_equal :stored, @refresher.store(lease, 13, 2, RULES)
    assert_equal R::Fresh.new(2), @refresher.decide(7, 13, 100)
    assert_equal 9, @refresher.age(7, 20)
    assert_equal %i[fresh stale expired], [20, 21, 41].map { @refresher.state(7, it) }
    assert_nil @refresher.age(8, 20)
    assert_nil @refresher.state(8, 20)
  end

  def test_stale_if_error_needs_a_failed_refresh_and_backs_off
    @refresher.put(7, 0, 0, RULES)
    assert_equal 15, @refresher.stale_if_error(7, 15)
    fetch = @refresher.decide(7, 15, 100)
    assert_instance_of R::Fetch, fetch
    waiter = @refresher.decide(7, 16, 100)
    flight = @refresher.flight(fetch.lease)
    assert_equal 17, @refresher.fail(fetch.lease, 17, 5)
    assert_equal [R::Notification.new(waiter.waiter, flight, 7, :abandoned)], @refresher.drain(10)
    assert_equal R::Stale.new(21, R::Backoff.new(22)), @refresher.decide(7, 21, 100)
    assert_equal 0, @refresher.in_flight

    again = @refresher.decide(7, 22, 100)
    assert_instance_of R::Fetch, again
    assert_nil @refresher.fail(again.lease, 30, 5)
    assert_equal :stale, @refresher.fail(again.lease, 30, 5)
    assert_nil @refresher.stale_if_error(7, 30)
    assert_instance_of R::Fetch, @refresher.decide(7, 31, 100)
  end

  def test_abandon_gives_the_lease_back_without_a_backoff
    @refresher.put(7, 0, 0, RULES)
    lead = @refresher.decide(7, 10, 100)
    assert_equal :abandoned, @refresher.abandon(lead.revalidation.lease)
    assert_equal :stale, @refresher.abandon(lead.revalidation.lease)
    assert_instance_of R::Lead, @refresher.decide(7, 10, 100).revalidation
  end

  def test_overdue_lease_is_taken_over_or_reaped
    dead = @refresher.decide(5, 0, 10)
    waiter = @refresher.decide(5, 9, 10)
    heir = @refresher.decide(5, 10, 10)
    assert_instance_of R::Fetch, heir
    refute_equal dead.lease, heir.lease
    assert_equal :stale, @refresher.store(dead.lease, 11, 0, RULES)
    assert_equal [1, 1], [@refresher.in_flight, @refresher.waiting]
    flight = @refresher.flight(heir.lease)

    assert_equal 0, @refresher.reap(19)
    assert_equal 1, @refresher.reap(20)
    assert_equal :time_regressed, @refresher.reap(19)
    assert_equal :stale, @refresher.store(heir.lease, 20, 0, RULES)
    assert_equal [R::Notification.new(waiter.waiter, flight, 5, :abandoned)], @refresher.drain(10)
  end

  def test_data_values_have_the_fields_of_the_core_types_in_order
    members = { R::Policy => %i[fresh_for stale_while_revalidate stale_if_error], R::Fresh => %i[age],
                R::Stale => %i[age revalidation], R::Fetch => %i[lease], R::Wait => %i[waiter flight],
                R::Lead => %i[lease], R::Backoff => %i[until], R::Unavailable => %i[refusal],
                R::Notification => %i[waiter flight key outcome] }
    members.each { |klass, fields| assert_equal fields, klass.members, klass.name }
    note = R::Notification.new(waiter: 2, flight: 1, key: 42, outcome: :completed)
    assert_equal [2, 1, 42, :completed], note.to_h.values
  end

  def test_flight_ties_a_notification_back_to_its_lease
    fetch = @refresher.decide(42, 0, 10)
    other = @refresher.decide(43, 0, 10)
    flight = @refresher.flight(fetch.lease)
    refute_equal flight, @refresher.flight(other.lease)
    waiter = @refresher.decide(42, 9, 10)
    assert_equal flight, waiter.flight
    assert_equal @refresher.flight(other.lease), @refresher.decide(43, 9, 10).flight

    # A takeover issues a new lease for the same flight.
    heir = @refresher.decide(42, 10, 10)
    assert_nil @refresher.flight(fetch.lease)
    assert_equal flight, @refresher.flight(heir.lease)
    late = @refresher.decide(42, 10, 10)
    assert_equal flight, late.flight
    assert_equal :cancelled, @refresher.cancel(late.waiter)

    assert_equal :stored, @refresher.store(heir.lease, 11, 0, RULES)
    note = @refresher.drain(10).first
    assert_equal [waiter.waiter, flight, 42, :completed], note.to_h.values
    assert_nil @refresher.flight(heir.lease)
    assert_kind_of Integer, @refresher.flight(other.lease)
  end

  def test_flight_of_a_stale_or_foreign_lease_is_nil
    near = R::Refresher.with_scope(1)
    far = R::Refresher.with_scope(2)
    near_fetch = near.decide(7, 0, 100)
    far_fetch = far.decide(7, 0, 100)
    assert_equal 1, near.flight(near_fetch.lease) >> 48
    assert_nil near.flight(far_fetch.lease)
    assert_nil far.flight(near_fetch.lease)
    assert_nil @refresher.flight(0)
    assert_nil @refresher.flight(MAX)
    assert_equal :abandoned, near.abandon(near_fetch.lease)
    assert_nil near.flight(near_fetch.lease)
    assert_raises(TypeError) { @refresher.flight(nil) }
    assert_raises(TypeError) { @refresher.flight(1.0) }
    assert_raises(RangeError) { @refresher.flight(-1) }
    assert_raises(RangeError) { @refresher.flight(2**64) }
  end

  def state(refresher) = [refresher.in_flight, refresher.waiting, refresher.pending]

  # Regression: identical calls on two refreshers issue different ids, and
  # each refuses the other's through every method that takes one.
  def test_an_id_from_another_refresher_names_nothing_here
    near = R::Refresher.with_scope(1)
    far = R::Refresher.with_scope(2)
    assert_equal [0, 1, 2], [@refresher.scope, near.scope, far.scope]
    near_ids = [near.decide(7, 0, 100).lease, near.decide(7, 1, 100).waiter]
    far_ids = [far.decide(7, 0, 100).lease, far.decide(7, 1, 100).waiter]
    # One sequence: the lease, its flight, then the waiter.
    assert_equal [(1 << 48) | 1, (1 << 48) | 3], near_ids
    assert_equal [(2 << 48) | 1, (2 << 48) | 3], far_ids
    assert_equal 1, @refresher.decide(7, 0, 100).lease # scope zero is a plain counter

    [[near, near_ids, far_ids], [far, far_ids, near_ids]].each do |here, own, (lease, waiter)|
      assert_equal :stale, here.store(lease, 2, 0, RULES)
      assert_equal :stale, here.fail(lease, 2, 5)
      assert_equal :stale, here.abandon(lease)
      assert_equal :stale, here.cancel(waiter)
      assert_nil here.flight(lease)
      assert_equal [own.first, 1, [1, 1, 0]], [here.leading(7), here.now, state(here)]
    end
    assert_equal :stored, near.store(near_ids.first, 2, 0, RULES)
    assert_equal [near_ids.last], near.drain(4).map(&:waiter)
    assert_equal :cancelled, far.cancel(far_ids.last)
  end

  def test_refreshers_left_in_the_same_scope_still_alias
    one = R::Refresher.with_scope(5)
    other = R::Refresher.with_scope(5)
    lease = one.decide(7, 0, 100).lease
    assert_equal lease, other.decide(8, 0, 100).lease
    waiter = one.decide(7, 0, 100).waiter
    assert_equal waiter, other.decide(8, 0, 100).waiter
    # Nothing tells them apart: keeping scopes distinct is the caller's job.
    assert_equal :cancelled, other.cancel(waiter)
    assert_equal :abandoned, other.abandon(lease)
    assert_equal [1, 1, 0], state(one)
  end

  def test_scope_of_routes_an_id_to_its_refresher
    assert_equal [0, 0, 3, 65_535], [0, 5, (3 << 48) | 5, MAX].map { R.scope_of(it) }
    pool = [4, 9].to_h { [it, R::Refresher.with_scope(it)] }
    fetch = pool[9].decide(1, 0, 100)
    waiter = pool[9].decide(1, 0, 100).waiter
    ids = [fetch.lease, waiter, pool[9].flight(fetch.lease)]
    assert_equal [9, 9, 9], ids.map { R.scope_of(it) }
    assert_equal :stored, pool.fetch(R.scope_of(fetch.lease)).store(fetch.lease, 1, 0, RULES)
    assert_raises(RangeError) { R.scope_of(-1) }
    assert_raises(RangeError) { R.scope_of(2**64) }
    assert_raises(TypeError) { R.scope_of(1.0) }
    assert_raises(TypeError) { R.scope_of('1') }
    assert_raises(TypeError) { R.scope_of(nil) }
    # The rest of the layout lives in the periscope-machines crate, not here.
    refute_respond_to R, :scoped
    refute R.const_defined?(:SEQUENCE_MAX)
  end

  def test_repeats_on_a_settled_lease_stay_stale
    stored = @refresher.decide(7, 0, 100).lease
    gone = @refresher.decide(7, 0, 100).waiter
    assert_equal :cancelled, @refresher.cancel(gone)
    assert_equal :stored, @refresher.store(stored, 1, 0, RULES)
    dropped = @refresher.decide(8, 1, 100).lease
    assert_equal :abandoned, @refresher.abandon(dropped)
    [stored, dropped].each do |lease|
      assert_equal :stale, @refresher.store(lease, 1, 0, RULES)
      assert_equal :stale, @refresher.fail(lease, 1, 5)
      assert_equal :stale, @refresher.abandon(lease)
    end
    assert_equal :stale, @refresher.cancel(gone)
    assert_equal [1, [0, 0, 0]], [@refresher.size, state(@refresher)]
  end

  def test_scope_must_be_a_sixteen_bit_integer
    assert_equal [0, 65_535], [R::Refresher.with_scope(0).scope, R::Refresher.with_scope(65_535).scope]
    assert_raises(RangeError) { R::Refresher.with_scope(65_536) }
    assert_raises(RangeError) { R::Refresher.with_scope(-1) }
    assert_raises(TypeError) { R::Refresher.with_scope(1.0) }
    assert_raises(TypeError) { R::Refresher.with_scope(nil) }
    assert_raises(ArgumentError) { R::Refresher.with_scope }
    assert_raises(ArgumentError) { R::Refresher.new(1) }
    assert_raises(ArgumentError) { R::Refresher.new(scope: 1) }
  end

  def test_waiters_cancel
    fetch = @refresher.decide(1, 0, 100)
    waiter = @refresher.decide(1, 0, 100)
    assert_equal :cancelled, @refresher.cancel(waiter.waiter)
    assert_equal :stale, @refresher.cancel(waiter.waiter)
    assert_equal :abandoned, @refresher.abandon(fetch.lease)
    assert_empty @refresher.drain(10)
  end

  def test_forget_and_sweep
    3.times { @refresher.put(it, 0, 0, R::Policy.new(1, 0, 0)) }
    @refresher.put(3, 0, 0, RULES)
    refute_empty @refresher
    assert_equal :forgotten, @refresher.forget(0)
    assert_equal :not_held, @refresher.forget(0)
    assert_equal :not_held, @refresher.forget(999)
    assert_equal 3, @refresher.size
    assert_empty @refresher.sweep(5, 0)
    assert_equal [1], @refresher.sweep(5, 1)
    assert_equal [2], @refresher.sweep(5, MAX)
    assert_empty @refresher.sweep(5, 10)
    assert_equal 1, @refresher.size
    assert_equal :time_regressed, @refresher.sweep(4, 10)
    assert_equal :forgotten, @refresher.forget(3)
    assert_empty @refresher
    assert_predicate R::Refresher.new, :empty?
  end

  def test_poll_takes_the_oldest_notification
    assert_nil @refresher.poll
    fetch = @refresher.decide(1, 0, 100)
    first = @refresher.decide(1, 0, 100)
    second = @refresher.decide(1, 0, 100)
    assert_nil @refresher.poll
    assert_equal :abandoned, @refresher.abandon(fetch.lease)
    assert_equal R::Notification.new(first.waiter, first.flight, 1, :abandoned), @refresher.poll
    assert_equal 1, @refresher.pending
    assert_equal [second.waiter], @refresher.drain(10).map(&:waiter)
    assert_nil @refresher.poll
  end

  def test_refusals_are_symbols_and_change_nothing
    assert_equal :rejected, @refresher.decide(1, 5, 0)
    assert_equal 0, @refresher.now
    fetch = @refresher.decide(1, 5, 100)
    assert_equal :time_regressed, @refresher.decide(1, 4, 100)
    assert_equal :time_regressed, @refresher.store(fetch.lease, 4, 0, RULES)
    assert_equal :time_regressed, @refresher.fail(fetch.lease, 4, 0)
    assert_equal :time_regressed, @refresher.put(2, 4, 0, RULES)
    assert_equal :stale, @refresher.store(0, 5, 0, RULES)
    assert_equal :stale, @refresher.store(MAX, 5, 0, RULES)
    assert_equal fetch.lease, @refresher.leading(1)
  end

  def test_capacities_are_compiled_constants_and_enforced
    assert_equal [256, 64, 256], [R::Refresher::CAPACITY, R::Refresher::FLIGHT_CAPACITY, R::Refresher::WAITER_CAPACITY]
    assert_equal [256, 64, 256],
                 [@refresher.capacity, @refresher.flight_capacity, @refresher.waiter_capacity]

    256.times { assert_equal :stored, @refresher.put(it, 0, 0, RULES) }
    assert_equal :entries_full, @refresher.put(999, 0, 0, RULES)
    assert_equal :stored, @refresher.put(3, 0, 1, RULES)

    leases = 64.times.map { @refresher.decide(1000 + it, 12, 100).lease }
    assert_equal :flights_full, @refresher.decide(2000, 12, 100)
    assert_equal R::Stale.new(12, R::Unavailable.new(:flights_full)), @refresher.decide(0, 12, 100)
    assert_equal :entries_full, @refresher.store(leases.first, 12, 0, RULES)
    assert_equal :forgotten, @refresher.forget(255)
    assert_equal :stored, @refresher.store(leases.first, 12, 0, RULES)

    256.times { assert_instance_of R::Wait, @refresher.decide(1001, 12, 100) }
    assert_equal :waiters_full, @refresher.decide(1001, 12, 100)
    assert_equal 256, @refresher.waiting
  end

  def test_malformed_input_raises
    assert_raises(TypeError) { @refresher.decide('1', 0, 1) }
    assert_raises(TypeError) { @refresher.decide(1, 0.5, 1) }
    assert_raises(TypeError) { @refresher.decide(1, 0, nil) }
    assert_raises(RangeError) { @refresher.decide(-1, 0, 1) }
    assert_raises(RangeError) { @refresher.decide(1, 2**64, 1) }
    assert_raises(RangeError) { @refresher.decide(1, -1, 1) }
    assert_raises(RangeError) { @refresher.decide(1, 0, 2**64) }
    assert_raises(TypeError) { @refresher.decide(1, 0, 1.0) }
    assert_raises(TypeError) { @refresher.store(1, 0, 0.5, RULES) }
    assert_raises(TypeError) { @refresher.fail(1, 0, 1r) }
    assert_raises(TypeError) { @refresher.reap(1.5) }
    assert_raises(RangeError) { @refresher.reap(-1) }
    assert_raises(TypeError) { @refresher.sweep(nil, 1) }
    assert_raises(TypeError) { @refresher.age(1, 1.0) }
    assert_raises(RangeError) { @refresher.state(1, 2**64) }
    assert_raises(TypeError) { @refresher.stale_if_error(1, '1') }
    assert_raises(TypeError) { R::Policy.new(10.0, 5, 20).state(1) }
    assert_raises(TypeError) { @refresher.put(1, 0, 0, [10, 5, 20]) }
    assert_raises(TypeError) { @refresher.put(1, 0, 0, R::Policy.new(10, nil, 20)) }
    assert_raises(RangeError) { @refresher.put(1, 0, 0, R::Policy.new(-1, 5, 20)) }
    assert_raises(TypeError) { R::Policy.new(10, 5, '20').state(1) }
    assert_raises(TypeError) { RULES.if_error?(1.0) }
    assert_raises(RangeError) { RULES.while_revalidate?(-1) }
    assert_raises(TypeError) { @refresher.drain(nil) }
    assert_raises(TypeError) { R::Refresher.allocate }
    assert_equal 0, @refresher.size
  end

  def test_frozen_refresher_refuses_to_change
    @refresher.put(1, 0, 0, RULES)
    @refresher.freeze
    assert_raises(FrozenError) { @refresher.decide(1, 0, 1) }
    assert_raises(FrozenError) { @refresher.put(1, 0, 0, RULES) }
    assert_raises(FrozenError) { @refresher.store(1, 0, 0, RULES) }
    assert_raises(FrozenError) { @refresher.fail(1, 0, 0) }
    assert_raises(FrozenError) { @refresher.abandon(1) }
    assert_raises(FrozenError) { @refresher.reap(0) }
    assert_raises(FrozenError) { @refresher.forget(1) }
    assert_raises(FrozenError) { @refresher.sweep(0, 1) }
    assert_raises(FrozenError) { @refresher.cancel(1) }
    assert_raises(FrozenError) { @refresher.drain(1) }
    assert_raises(FrozenError) { @refresher.poll }
    refute_empty @refresher
    assert_equal :fresh, @refresher.state(1, 0)
    assert_nil @refresher.flight(1)
  end
end
