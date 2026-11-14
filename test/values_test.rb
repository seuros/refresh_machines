# frozen_string_literal: true
require 'minitest/autorun'
require 'refresh_machines'

# Values are valid or do not exist, and the binding never loses work.
class ValuesTest < Minitest::Test
  R = RefreshMachines
  RULES = R::Policy.new(10, 5, 20)
  MAX = 2**64 - 1
  # The core's Duration::MAX in nanoseconds.
  DURATION_MAX = (MAX * 1_000_000_000) + 999_999_999
  ID = (3 << 48) | 1
  REFUSALS = %i[time_regressed zero_lead entries_full flights_full waiters_full exhausted stale].freeze

  def setup
    @refresher = R::Refresher.new
  end

  def state(refresher)
    [refresher.in_flight, refresher.waiting, refresher.pending, refresher.size, refresher.now, refresher.issued]
  end

  def test_policy_windows_are_integer_nanoseconds_in_the_argument_range
    assert_equal [0, MAX, 1], R::Policy.new(0, MAX, 1).to_h.values
    assert_raises(RangeError) { R::Policy.new(-1, 0, 0) }
    assert_raises(RangeError) { R::Policy.new(0, 2**64, 0) }
    assert_raises(TypeError) { R::Policy.new(0, 0, 1.5) }
    assert_raises(TypeError) { R::Policy.new(fresh_for: nil, stale_while_revalidate: 0, stale_if_error: 0) }
  end

  def test_answered_ages_and_instants_reach_the_core_duration_max
    assert_equal DURATION_MAX, R::Fresh.new(DURATION_MAX).age
    assert_equal DURATION_MAX, R::Backoff.new(DURATION_MAX).until
    assert_raises(RangeError) { R::Fresh.new(DURATION_MAX + 1) }
    assert_raises(RangeError) { R::Backoff.new(-1) }
    assert_raises(TypeError) { R::Fresh.new('1') }
    assert_raises(RangeError) { R::Stale.new(-1, :in_flight) }
  end

  def test_stale_holds_only_a_revalidation
    [R::Lead.new(ID), :in_flight, R::Backoff.new(5), R::Unavailable.new(:exhausted)].each do |revalidation|
      assert_equal revalidation, R::Stale.new(1, revalidation).revalidation
    end
    assert_raises(RangeError) { R::Stale.new(1, :nope) }
    assert_raises(TypeError) { R::Stale.new(1, ID) }
    assert_raises(TypeError) { R::Stale.new(1, R::Fetch.new(ID)) }
  end

  def test_ids_never_have_a_zero_sequence
    assert_equal ID, R::Fetch.new(ID).lease
    assert_equal ID, R::Lead.new(lease: ID).lease
    assert_equal [ID + 1, ID], R::Wait.new(ID + 1, ID).to_h.values
    [0, 3 << 48].each do |zero|
      assert_raises(RangeError) { R::Fetch.new(zero) }
      assert_raises(RangeError) { R::Lead.new(zero) }
      assert_raises(RangeError) { R::Wait.new(zero, ID) }
      assert_raises(RangeError) { R::Wait.new(ID + 1, zero) }
      assert_raises(RangeError) { R::Notification.new(zero, ID, 1, :completed) }
      assert_raises(RangeError) { R::Notification.new(ID + 1, zero, 1, :completed) }
    end
    assert_raises(RangeError) { R::Fetch.new(-1) }
    assert_raises(RangeError) { R::Fetch.new(2**64) }
    assert_raises(TypeError) { R::Fetch.new('1') }
    assert_raises(TypeError) { R::Wait.new(ID, 1.0) }
  end

  # A waiter is issued after the flight it waits on, in the flight's scope.
  def test_a_wait_and_a_notification_pair_ids_one_refresher_issues_together
    assert_raises(RangeError) { R::Wait.new(ID, ID) }
    assert_raises(RangeError) { R::Wait.new(ID, ID + 1) }
    assert_raises(RangeError) { R::Wait.new(ID + 1 + (1 << 48), ID) }
    assert_raises(RangeError) { R::Notification.new(ID, ID + 1, 1, :completed) }
    assert_raises(RangeError) { R::Notification.new(ID + 1 + (1 << 48), ID, 1, :completed) }
  end

  def test_unavailable_holds_a_refusal_symbol
    REFUSALS.each { |refusal| assert_equal refusal, R::Unavailable.new(refusal).refusal }
    assert_raises(RangeError) { R::Unavailable.new(:rejected) }
    assert_raises(TypeError) { R::Unavailable.new('exhausted') }
  end

  def test_notification_holds_a_key_and_an_outcome
    assert_equal [ID + 1, ID, MAX, :abandoned], R::Notification.new(ID + 1, ID, MAX, :abandoned).to_h.values
    assert_raises(RangeError) { R::Notification.new(ID + 1, ID, -1, :completed) }
    assert_raises(TypeError) { R::Notification.new(ID + 1, ID, nil, :completed) }
    assert_raises(RangeError) { R::Notification.new(ID + 1, ID, 1, :settled) }
    assert_raises(TypeError) { R::Notification.new(ID + 1, ID, 1, 'completed') }
    # A wrong type anywhere is TypeError, even beside a value out of range.
    assert_raises(TypeError) { R::Notification.new(0, ID, 1, 'completed') }
  end

  def test_the_validation_hooks_are_private
    %i[__nanos __duration __lease __wait __notification __refusal].each do |hook|
      assert_raises(NoMethodError) { R.public_send(hook, 1) }
    end
  end

  def test_a_zero_id_raises_before_the_core_is_asked
    lease = @refresher.decide(1, 0, 100).lease
    waiter = @refresher.decide(1, 0, 100).waiter
    before = state(@refresher)
    assert_raises(RangeError) { @refresher.store(1 << 48, 1, 0, RULES) }
    assert_raises(RangeError) { @refresher.fail(0, 1, 0) }
    assert_raises(RangeError) { @refresher.abandon(0) }
    assert_raises(RangeError) { @refresher.cancel(0) }
    assert_equal before, state(@refresher)
    assert_equal [:abandoned, :cancelled], [@refresher.abandon(lease), @refresher.cancel(waiter)]
  end

  def test_a_bad_argument_after_a_good_lease_loses_nothing
    lease = @refresher.decide(1, 0, 100).lease
    before = state(@refresher)
    assert_raises(TypeError) { @refresher.store(lease, 1, 0, Struct.new(:fresh_for).new(1)) }
    assert_raises(TypeError) { @refresher.store(lease, 1, nil, RULES) }
    assert_raises(TypeError) { @refresher.fail(lease, 1, 1.0) }
    assert_equal before, state(@refresher)
    assert_equal lease, @refresher.leading(1)
  end

  # The classes are resolved once, at init: removing a constant later changes
  # nothing the binding builds or reads.
  def test_the_classes_are_resolved_once_at_init
    without(:Fetch) do
      assert_equal 'RefreshMachines::Fetch', @refresher.decide(9, 5, 100).class.name
    end
  end

  # Sealing stops accidents, not reflection: Ruby binds a named subclass's
  # constant before `inherited` raises, so the class leaks. Both forms raise.
  def test_values_are_sealed_and_a_named_subclass_leaks_its_constant
    [R::Policy, R::Fresh, R::Stale, R::Fetch, R::Wait, R::Lead, R::Backoff, R::Unavailable,
     R::Notification].each do |value|
      assert_raises(TypeError) { Class.new(value) }
      assert_raises(NoMethodError) { value.allocate }
    end
    assert_raises(TypeError) { self.class.module_eval('class LeakedLead < R::Lead; end', __FILE__, __LINE__) }
    assert self.class.const_defined?(:LeakedLead, false), 'the constant is bound before inherited raises'
  end

  # A forged value never reaches the core: a leaked subclass's instance is a
  # TypeError, a Marshal frame edited past the check a RangeError.
  def test_a_forged_value_is_refused_at_the_boundary
    assert_raises(TypeError) { self.class.module_eval('class ForgedPolicy < R::Policy; end', __FILE__, __LINE__) }
    assert_raises(TypeError) { self.class.module_eval('class ForgedLead < R::Lead; end', __FILE__, __LINE__) }
    [ForgedPolicy, ForgedLead].each do |forger|
      forger.class_eval { def initialize(**fields) = Data.instance_method(:initialize).bind_call(self, **fields) }
    end
    lease = @refresher.decide(1, 0, 100).lease
    before = state(@refresher)
    forged = ForgedPolicy.new(fresh_for: -1, stale_while_revalidate: 0, stale_if_error: 0)
    assert_raises(TypeError) { @refresher.store(lease, 1, 0, forged) }
    assert_raises(TypeError) { forged.state(0) }
    assert_raises(TypeError) { R::Stale.new(1, ForgedLead.new(lease: 0)) }
    edited = Marshal.load(Marshal.dump(R::Policy.new(5, 0, 0)).sub("i\x0A".b, "i\xFA".b))
    assert_equal(-1, edited.fresh_for)
    assert_raises(RangeError) { @refresher.store(lease, 1, 0, edited) }
    frame = Marshal.dump(R::Lead.new(lease))
    frame[(frame.index(":\nlease".b) + 7)..] = "i\x00".b
    zero = Marshal.load(frame)
    assert_equal 0, zero.lease
    assert_raises(RangeError) { R::Stale.new(1, zero) }
    assert_equal before, state(@refresher)
  end

  # The classes are pinned at init, so a compaction moves none of them: every
  # path that builds or reads a value still works after it.
  def test_every_value_path_survives_gc_compact
    skip 'GC.compact is not available' unless GC.respond_to?(:compact)
    GC.compact
    lease = @refresher.decide(1, 0, 100).lease
    waiter = @refresher.decide(1, 0, 100).waiter
    assert_equal :stored, @refresher.store(lease, 1, 0, RULES)
    assert_equal :fresh, RULES.state(0)
    assert_instance_of R::Fresh, @refresher.decide(1, 1, 100)
    assert_instance_of R::Lead, @refresher.decide(1, 11, 100).revalidation
    assert_equal [waiter], @refresher.drain(5).map(&:waiter)
    assert_nil @refresher.poll
    assert_equal R::Stale.new(1, :in_flight), R::Stale.new(1, :in_flight)
  end

  # A frozen refresher raises FrozenError from every mutating call.
  def test_a_frozen_refresher_refuses_every_mutating_call
    lease = @refresher.decide(1, 0, 100).lease
    @refresher.freeze
    [-> { @refresher.decide(2, 0, 100) }, -> { @refresher.store(lease, 1, 0, RULES) },
     -> { @refresher.fail(lease, 1, 1) }, -> { @refresher.abandon(lease) }, -> { @refresher.reap(1) },
     -> { @refresher.put(3, 1, 0, RULES) }, -> { @refresher.forget(1) }, -> { @refresher.sweep(1, 5) },
     -> { @refresher.cancel(lease) }, -> { @refresher.poll }, -> { @refresher.drain(5) }].each do |call|
      assert_raises(FrozenError) { call.call }
    end
  end

  # Answers are built natively from what the core returned: no Ruby
  # `initialize` runs, so one that raises cannot lose a lease or a wake-up.
  def test_answers_after_the_core_mutated_run_no_ruby_code
    poisoned(%i[Fresh Stale Fetch Wait Lead Backoff Unavailable Notification]) do
      lease = @refresher.decide(1, 0, 100).lease
      waiter = @refresher.decide(1, 0, 100).waiter
      assert_equal :stored, @refresher.store(lease, 1, 0, RULES)
      assert_equal [waiter], @refresher.drain(5).map(&:waiter)
      assert_instance_of R::Fresh, @refresher.decide(1, 1, 100)
      assert_instance_of R::Lead, @refresher.decide(1, 11, 100).revalidation
      assert_predicate @refresher.decide(1, 11, 100), :frozen?
      assert_raises(RuntimeError) { R::Fetch.new(lease) }
    end
  end

  private

  def poisoned(names)
    saved = names.to_h { [it, R.const_get(it).instance_method(:initialize)] }
    saved.each_key { R.const_get(it).define_method(:initialize) { |*, **| raise 'Ruby code ran' } }
    yield
  ensure
    saved.each { |name, method| R.const_get(name).define_method(:initialize, method) }
  end

  def without(name)
    saved = R.send(:remove_const, name)
    yield
  ensure
    R.const_set(name, saved)
  end
end
