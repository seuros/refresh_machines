# frozen_string_literal: true
require 'minitest/autorun'
require 'refresh_machines'

# One sequence per refresher: a lease, a waiter id and a flight id are never
# equal, and a restart resumes the sequence instead of starting it again.
class RefresherSequenceTest < Minitest::Test
  R = RefreshMachines
  RULES = R::Policy.new(10, 5, 20)
  LAST = 2**48 - 1

  def state(refresher)
    [refresher.in_flight, refresher.waiting, refresher.pending, refresher.size, refresher.now, refresher.issued]
  end

  # Every method that takes a lease, and the one that takes a waiter.
  def refused_as_lease(refresher, id)
    before = state(refresher)
    now = refresher.now
    assert_equal %i[stale stale stale],
                 [refresher.store(id, now, 0, RULES), refresher.fail(id, now, 1), refresher.abandon(id)]
    assert_nil refresher.flight(id)
    assert_equal before, state(refresher)
  end

  def refused_as_waiter(refresher, id)
    before = state(refresher)
    assert_equal :stale, refresher.cancel(id)
    assert_equal before, state(refresher)
  end

  # Regression: leases and waiter ids used to be counted separately, so from
  # Ruby, where both are bare Integers, lease 2 cancelled waiter 2 of another
  # key and waiter 2 abandoned the refresh under lease 2.
  def test_a_lease_and_a_waiter_id_never_name_each_other
    refresher = R::Refresher.with_scope(2)
    l1 = refresher.decide(1, 0, 100).lease
    waiter = refresher.decide(1, 0, 100).waiter
    l2 = refresher.decide(2, 0, 100).lease
    assert_equal [1, 3, 4].map { (2 << 48) | it }, [l1, waiter, l2]
    refute_equal l2, waiter

    before = state(refresher)
    assert_equal :stale, refresher.cancel(l2)
    assert_equal :stale, refresher.abandon(waiter)
    assert_equal before, state(refresher)
    assert_equal [l1, l2], [refresher.leading(1), refresher.leading(2)]

    flights = [l1, l2].map { refresher.flight(it) }
    refused_as_lease(refresher, waiter)
    [l1, l2].each { refused_as_waiter(refresher, it) }
    flights.each do |flight|
      refused_as_lease(refresher, flight)
      refused_as_waiter(refresher, flight)
    end

    # Key 1's waiter and key 2's refresh are both still there.
    assert_equal :abandoned, refresher.abandon(l2)
    assert_equal :cancelled, refresher.cancel(waiter)
  end

  def test_no_lease_ever_equals_a_waiter_or_flight_id
    refresher = R::Refresher.new
    random = Random.new(7)
    leases = []
    others = []
    live = []
    400.times do |now|
      key = random.rand(6)
      case (decision = refresher.decide(key, now, 1 + random.rand(4)))
      when R::Fetch
        leases << decision.lease
        live << decision.lease
        others << refresher.flight(decision.lease)
      when R::Wait
        others << decision.waiter << decision.flight
      end
      refresher.fail(live.shift, now, 0) if random.rand(3).zero? && !live.empty?
      refresher.drain(random.rand(3))
      refused_as_waiter(refresher, leases.sample(random: random)) unless leases.empty?
      refused_as_lease(refresher, others.sample(random: random)) unless others.empty?
    end
    others.uniq!
    assert_operator leases.size, :>, 40
    assert_equal leases.size, leases.uniq.size
    assert_empty leases & others
    assert_equal (1..refresher.issued).to_a, (leases + others).sort
  end

  def test_resumed_continues_the_sequence_after_a_restart
    first = R::Refresher.with_scope(6)
    lease = first.decide(7, 0, 100).lease
    waiter = first.decide(7, 0, 100).waiter
    flight = first.flight(lease)
    assert_equal 3, first.issued

    second = R::Refresher.resumed(6, first.issued)
    assert_equal [6, [0, 0, 0, 0, 0, 3]], [second.scope, state(second)]
    again = second.decide(7, 0, 100).lease
    follower = second.decide(7, 0, 100).waiter
    assert_equal [(6 << 48) | 4, (6 << 48) | 6], [again, follower]
    [lease, waiter, flight].each do |old|
      refused_as_lease(second, old)
      refused_as_waiter(second, old)
    end

    # A fresh refresher in the same scope reissues them, and honours both.
    naive = R::Refresher.with_scope(6)
    assert_equal [lease, waiter], [naive.decide(9, 0, 100).lease, naive.decide(9, 0, 100).waiter]
    assert_equal %i[cancelled abandoned], [naive.cancel(waiter), naive.abandon(lease)]
    assert_equal [0, 0], [R::Refresher.resumed(0, 0).issued, R::Refresher.new.issued]
  end

  def test_the_last_ids_are_issued_then_exhausted
    refresher = R::Refresher.resumed(65_535, LAST - 3)
    assert_equal :stored, refresher.put(1, 0, 0, RULES)
    lease = refresher.decide(2, 0, 100).lease
    assert_equal [2**64 - 3, 2**64 - 2], [lease, refresher.flight(lease)]
    assert_equal 2**64 - 1, refresher.decide(2, 0, 100).waiter
    before = state(refresher)
    assert_equal LAST, refresher.issued
    assert_equal %i[exhausted exhausted exhausted],
                 [refresher.decide(3, 0, 100), refresher.decide(2, 0, 100), refresher.decide(2, 200, 100)]
    assert_equal before, state(refresher)
    assert_equal R::Stale.new(12, R::Unavailable.new(:exhausted)), refresher.decide(1, 12, 100)
    assert_equal :stored, refresher.store(lease, 12, 0, RULES)
  end

  def test_a_new_refresh_needs_two_ids
    refresher = R::Refresher.resumed(5, LAST - 3)
    dead = refresher.decide(1, 0, 1).lease
    before = state(refresher)
    assert_equal :exhausted, refresher.decide(2, 0, 100) # one id left: no lease without a flight
    assert_equal before, state(refresher)
    heir = refresher.decide(1, 5, 100).lease # a takeover needs only the lease
    assert_equal [(5 << 48) | LAST, refresher.flight(heir)], [heir, dead + 1]
    assert_equal :exhausted, refresher.decide(1, 5, 100)
  end

  def test_resumed_validates_its_arguments
    assert_raises(RangeError) { R::Refresher.resumed(0, 2**48) }
    assert_raises(RangeError) { R::Refresher.resumed(7, 2**64 - 1) }
    assert_instance_of R::Refresher, R::Refresher.resumed(65_535, LAST)
    assert_raises(RangeError) { R::Refresher.resumed(65_536, 0) }
    assert_raises(RangeError) { R::Refresher.resumed(-1, 0) }
    assert_raises(RangeError) { R::Refresher.resumed(0, -1) }
    assert_raises(RangeError) { R::Refresher.resumed(0, 2**64) }
    assert_raises(TypeError) { R::Refresher.resumed(1.0, 0) }
    assert_raises(TypeError) { R::Refresher.resumed(0, nil) }
    assert_raises(ArgumentError) { R::Refresher.resumed(0) }
    frozen = R::Refresher.resumed(3, 9).freeze
    assert_equal [3, 9], [frozen.scope, frozen.issued]
  end
end
