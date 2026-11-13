# frozen_string_literal: true

require_relative 'refresh_machines/version'

module RefreshMachines
  # Freshness rules for one value, as three spans in Integer nanoseconds.
  # Both stale windows open when `fresh_for` ends. `state`,
  # `while_revalidate?` and `if_error?` are native and take an age.
  #
  # Every instant and span in this gem is Integer nanoseconds: `now` as read
  # from `Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)`, and
  # every age, window, `lead_for`, `retry_after` and `Backoff#until`.
  Policy = Data.define(:fresh_for, :stale_while_revalidate, :stale_if_error)
  # Serve the cached value.
  Fresh = Data.define(:age)
  # Serve the cached value, stale. `revalidation` says who refreshes it: a
  # Lead (you do), :in_flight (someone else is), a Backoff (nobody until then)
  # or an Unavailable (a refresh is due but could not be registered).
  Stale = Data.define(:age, :revalidation)
  # Nothing servable: fetch, then report with `lease`, a lease id.
  Fetch = Data.define(:lease)
  # Nothing servable and someone is already fetching: park under the `waiter`
  # id. `flight` names the refresh being shared: what `Refresher#flight`
  # answers for the key's live lease, and the `flight` of the notification to come.
  Wait = Data.define(:waiter, :flight)
  # Stale#revalidation: refresh in the background and report with `lease`.
  Lead = Data.define(:lease)
  # Stale#revalidation: the last refresh failed and asked for quiet `until` then.
  Backoff = Data.define(:until)
  # Stale#revalidation: a refresh is due but was not registered; `refusal` is
  # the reason. The decision is still honoured: serve the stale value.
  Unavailable = Data.define(:refusal)
  # One waiter to wake. `flight` names the refresh that settled, see
  # `Refresher#flight`. `outcome` is :completed or :abandoned.
  Notification = Data.define(:waiter, :flight, :key, :outcome)
end

# Native is required: no algorithm fallback. It builds the Data values above
# and adds the Policy predicates, so they must exist first.
require 'refresh_machines_native/refresh_machines_native'
