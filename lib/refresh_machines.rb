# frozen_string_literal: true

require_relative 'refresh_machines/version'

# Every Data value below is held to what the core accepts, whether the gem
# returned it or it was built by hand: TypeError for the wrong type,
# RangeError for a value outside the core's domain.
module RefreshMachines
  # Freshness rules for one value, as three spans in Integer nanoseconds.
  # Both stale windows open when `fresh_for` ends. `state`,
  # `while_revalidate?` and `if_error?` are native and take an age.
  #
  # Every instant and span in this gem is Integer nanoseconds: `now` as read
  # from `Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)`, and
  # every age, window, `lead_for`, `retry_after` and `Backoff#until`.
  # Arguments, and the Policy windows, are in 0..2**64-1; an age or instant the
  # core answers (Fresh, Stale, Backoff) may be larger, up to the core's
  # `Duration::MAX`, because sums saturate instead of wrapping.
  Policy = Data.define(:fresh_for, :stale_while_revalidate, :stale_if_error) do
    def initialize(fresh_for:, stale_while_revalidate:, stale_if_error:)
      RefreshMachines.__send__(:__policy, fresh_for, stale_while_revalidate, stale_if_error)
      super
    end
  end
  # Serve the cached value.
  Fresh = Data.define(:age) do
    def initialize(age:)
      RefreshMachines.__send__(:__age, age)
      super
    end
  end
  # Serve the cached value, stale. `revalidation` says who refreshes it: a
  # Lead (you do), :in_flight (someone else is), a Backoff (nobody until then)
  # or an Unavailable (a refresh is due but could not be registered).
  Stale = Data.define(:age, :revalidation) do
    def initialize(age:, revalidation:)
      RefreshMachines.__send__(:__age, age)
      RefreshMachines.__send__(:__revalidation, revalidation)
      super
    end
  end
  # Nothing servable: fetch, then report with `lease`, a lease id (its
  # sequence, the low 48 bits, is never zero).
  Fetch = Data.define(:lease) do
    def initialize(lease:)
      RefreshMachines.__send__(:__lease, lease)
      super
    end
  end
  # Nothing servable and someone is already fetching: park under the `waiter`
  # id (issued after `flight`, in its scope). `flight` names the refresh being shared: what `Refresher#flight`
  # answers for the key's live lease, and the `flight` of the notification to come.
  Wait = Data.define(:waiter, :flight) do
    def initialize(waiter:, flight:)
      RefreshMachines.__send__(:__wait, waiter, flight)
      super
    end
  end
  # Stale#revalidation: refresh in the background and report with `lease`.
  Lead = Data.define(:lease) do
    def initialize(lease:)
      RefreshMachines.__send__(:__lease, lease)
      super
    end
  end
  # Stale#revalidation: the last refresh failed and asked for quiet `until` then.
  Backoff = Data.define(:until) do
    def initialize(until:)
      RefreshMachines.__send__(:__until, binding.local_variable_get(:until))
      super
    end
  end
  # Stale#revalidation: a refresh is due but was not registered; `refusal` is
  # the reason, a refusal symbol. The decision is still honoured: serve the
  # stale value.
  Unavailable = Data.define(:refusal) do
    def initialize(refusal:)
      RefreshMachines.__send__(:__refusal, refusal)
      super
    end
  end
  # One waiter to wake. `flight` names the refresh that settled, see
  # `Refresher#flight`; `waiter` is of its scope and issued after it.
  # `outcome` is :completed or :abandoned.
  Notification = Data.define(:waiter, :flight, :key, :outcome) do
    def initialize(waiter:, flight:, key:, outcome:)
      RefreshMachines.__send__(:__notification, waiter, flight, key, outcome)
      super
    end
  end
end

# Native is required: no algorithm fallback. It validates and builds the Data
# values above and adds the Policy predicates, so they must exist first.
require 'refresh_machines_native/refresh_machines_native'

module RefreshMachines
  private_class_method :__policy, :__age, :__until, :__lease, :__wait, :__notification, :__refusal, :__revalidation

  # The values are sealed. `initialize` is where each validates, and a
  # subclass could override it, or `allocate` build an instance that never ran
  # it: subclassing raises TypeError and `allocate` is gone. Sealing stops
  # accidents, not reflection (a named subclass's constant is bound before
  # `inherited` raises; `Marshal.load` skips `initialize`), so the native side
  # is the authority: a value it reads (a Policy, a Stale's revalidation) must
  # be exactly its class, and its fields are checked again.
  [Policy, Fresh, Stale, Fetch, Wait, Lead, Backoff, Unavailable, Notification].each do |value|
    value.singleton_class.undef_method(:allocate)
    value.define_singleton_method(:inherited) { |_| raise TypeError, "#{value} cannot be subclassed" }
  end
end
