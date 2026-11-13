# frozen_string_literal: true
require 'minitest/autorun'
require 'refresh_machines'

class RefresherTest < Minitest::Test
  R = RefreshMachines
  RULES = R::Policy.new(10, 5, 20)
  MAX = 2**64 - 1

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

  def test_miss_is_fetched_once_and_waiters_are_woken
    fetch = @refresher.decide(42, 0, 100)
    assert_equal R::Fetch.new(fetch.lease_id, 42), fetch
    assert_equal fetch.lease_id, @refresher.leading(42)
    a = @refresher.decide(42, 1, 100)
    b = @refresher.decide(42, 2, 100)
    assert_instance_of R::Wait, a
    assert_equal 42, a.key
    refute_equal a.waiter_id, b.waiter_id
    assert_equal [1, 2, 0, 0], [@refresher.in_flight, @refresher.waiting, @refresher.pending, @refresher.size]
    assert_empty @refresher.drain(10)

    assert_equal :stored, @refresher.store(fetch.lease_id, 3, 0, RULES)
    assert_equal :stale, @refresher.store(fetch.lease_id, 3, 0, RULES)
    assert_equal [0, 0, 2, 1], [@refresher.in_flight, @refresher.waiting, @refresher.pending, @refresher.size]
    assert_nil @refresher.leading(42)
    assert_equal [R::Notification.new(a.waiter_id, 42, :completed)], @refresher.drain(1)
    assert_equal [R::Notification.new(b.waiter_id, 42, :completed)], @refresher.drain(10)
    assert_equal R::Fresh.new(1), @refresher.decide(42, 4, 100)
    assert_equal 4, @refresher.now
  end

  def test_stale_while_revalidate_serves_and_elects_one_leader
    assert_equal :stored, @refresher.put(7, 0, 0, RULES)
    assert_equal R::Fresh.new(9), @refresher.decide(7, 9, 100)
    lead = @refresher.decide(7, 10, 100)
    assert_equal R::Stale.new(10, :lead, lead.lease_id, nil), lead
    assert_kind_of Integer, lead.lease_id
    assert_equal R::Stale.new(12, :in_flight, nil, nil), @refresher.decide(7, 12, 100)
    assert_equal 0, @refresher.waiting
    assert_equal :stored, @refresher.store(lead.lease_id, 13, 2, RULES)
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
    assert_equal 17, @refresher.fail(fetch.lease_id, 17, 5)
    assert_equal [R::Notification.new(waiter.waiter_id, 7, :abandoned)], @refresher.drain(10)
    assert_equal R::Stale.new(21, :backoff, nil, 22), @refresher.decide(7, 21, 100)
    assert_equal 0, @refresher.in_flight

    again = @refresher.decide(7, 22, 100)
    assert_instance_of R::Fetch, again
    assert_nil @refresher.fail(again.lease_id, 30, 5)
    assert_equal :stale, @refresher.fail(again.lease_id, 30, 5)
    assert_nil @refresher.stale_if_error(7, 30)
    assert_instance_of R::Fetch, @refresher.decide(7, 31, 100)
  end

  def test_abandon_gives_the_lease_back_without_a_backoff
    @refresher.put(7, 0, 0, RULES)
    lead = @refresher.decide(7, 10, 100)
    assert_equal :abandoned, @refresher.abandon(lead.lease_id)
    assert_equal :stale, @refresher.abandon(lead.lease_id)
    assert_equal :lead, @refresher.decide(7, 10, 100).revalidation
  end

  def test_overdue_lease_is_taken_over_or_reaped
    dead = @refresher.decide(5, 0, 10)
    waiter = @refresher.decide(5, 9, 10)
    heir = @refresher.decide(5, 10, 10)
    assert_instance_of R::Fetch, heir
    refute_equal dead.lease_id, heir.lease_id
    assert_equal :stale, @refresher.store(dead.lease_id, 11, 0, RULES)
    assert_equal [1, 1], [@refresher.in_flight, @refresher.waiting]

    assert_equal 0, @refresher.reap(19)
    assert_equal 1, @refresher.reap(20)
    assert_equal :time_regressed, @refresher.reap(19)
    assert_equal :stale, @refresher.store(heir.lease_id, 20, 0, RULES)
    assert_equal [R::Notification.new(waiter.waiter_id, 5, :abandoned)], @refresher.drain(10)
  end

  def test_scopes_keep_the_ids_of_separate_refreshers_apart
    near = R::Refresher.new(scope: 1)
    far = R::Refresher.new(scope: 2)
    assert_equal [0, 1, 2], [@refresher.scope, near.scope, far.scope]
    near_fetch = near.decide(7, 0, 100)
    far_fetch = far.decide(7, 0, 100)
    near_wait = near.decide(7, 1, 100)
    far_wait = far.decide(7, 1, 100)
    assert_equal (1 << 48) | 1, near_fetch.lease_id
    assert_equal (2 << 48) | 2, far_wait.waiter_id
    assert_equal 1, @refresher.decide(7, 0, 100).lease_id # scope zero keeps plain counters

    # A foreign lease or waiter is stale here, and the refusal changes nothing.
    assert_equal :stale, near.store(far_fetch.lease_id, 2, 0, RULES)
    assert_equal :stale, near.fail(far_fetch.lease_id, 2, 5)
    assert_equal :stale, near.abandon(far_fetch.lease_id)
    assert_equal :stale, near.cancel(far_wait.waiter_id)
    assert_equal [near_fetch.lease_id, 1, 1, 0], [near.leading(7), near.in_flight, near.waiting, near.pending]
    assert_equal :stored, near.store(near_fetch.lease_id, 2, 0, RULES)
    assert_equal [near_wait.waiter_id], near.drain(4).map(&:waiter_id)
    assert_equal :cancelled, far.cancel(far_wait.waiter_id)
  end

  def test_scope_must_be_a_sixteen_bit_integer
    assert_equal 65_535, R::Refresher.new(scope: 65_535).scope
    assert_raises(RangeError) { R::Refresher.new(scope: 65_536) }
    assert_raises(RangeError) { R::Refresher.new(scope: -1) }
    assert_raises(TypeError) { R::Refresher.new(scope: 1.0) }
    assert_raises(TypeError) { R::Refresher.new(scope: nil) }
    assert_raises(ArgumentError) { R::Refresher.new(1) }
    assert_raises(ArgumentError) { R::Refresher.new(scopes: 1) }
  end

  def test_waiters_cancel
    fetch = @refresher.decide(1, 0, 100)
    waiter = @refresher.decide(1, 0, 100)
    assert_equal :cancelled, @refresher.cancel(waiter.waiter_id)
    assert_equal :stale, @refresher.cancel(waiter.waiter_id)
    assert_equal :abandoned, @refresher.abandon(fetch.lease_id)
    assert_empty @refresher.drain(10)
  end

  def test_forget_and_sweep
    3.times { @refresher.put(it, 0, 0, R::Policy.new(1, 0, 0)) }
    @refresher.put(3, 0, 0, RULES)
    assert @refresher.forget(0)
    refute @refresher.forget(0)
    assert_empty @refresher.sweep(5, 0)
    assert_equal [1], @refresher.sweep(5, 1)
    assert_equal [2], @refresher.sweep(5, MAX)
    assert_empty @refresher.sweep(5, 10)
    assert_equal 1, @refresher.size
    assert_equal :time_regressed, @refresher.sweep(4, 10)
  end

  def test_refusals_are_symbols_and_change_nothing
    assert_equal :rejected, @refresher.decide(1, 5, 0)
    assert_equal 0, @refresher.now
    fetch = @refresher.decide(1, 5, 100)
    assert_equal :time_regressed, @refresher.decide(1, 4, 100)
    assert_equal :time_regressed, @refresher.store(fetch.lease_id, 4, 0, RULES)
    assert_equal :time_regressed, @refresher.fail(fetch.lease_id, 4, 0)
    assert_equal :time_regressed, @refresher.put(2, 4, 0, RULES)
    assert_equal :stale, @refresher.store(0, 5, 0, RULES)
    assert_equal :stale, @refresher.store(MAX, 5, 0, RULES)
    assert_equal fetch.lease_id, @refresher.leading(1)
  end

  def test_capacities_are_compiled_constants_and_enforced
    assert_equal [256, 64, 256], [R::Refresher::CAPACITY, R::Refresher::FLIGHT_CAPACITY, R::Refresher::WAITER_CAPACITY]
    assert_equal [256, 64, 256],
                 [@refresher.capacity, @refresher.flight_capacity, @refresher.waiter_capacity]

    256.times { assert_equal :stored, @refresher.put(it, 0, 0, RULES) }
    assert_equal :entries_full, @refresher.put(999, 0, 0, RULES)
    assert_equal :stored, @refresher.put(3, 0, 1, RULES)

    leases = 64.times.map { @refresher.decide(1000 + it, 12, 100).lease_id }
    assert_equal :flights_full, @refresher.decide(2000, 12, 100)
    assert_equal R::Stale.new(12, :flights_full, nil, nil), @refresher.decide(0, 12, 100)
    assert_equal :entries_full, @refresher.store(leases.first, 12, 0, RULES)
    assert @refresher.forget(255)
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
    assert_equal :fresh, @refresher.state(1, 0)
  end
end
