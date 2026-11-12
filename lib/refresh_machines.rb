# frozen_string_literal: true

require_relative 'refresh_machines/version'

module RefreshMachines
  # Freshness rules for one value, as three durations in the caller's time
  # unit. Both stale windows open when `fresh_for` ends. `state`,
  # `while_revalidate?` and `if_error?` are native and take an age.
  Policy = Data.define(:fresh_for, :stale_while_revalidate, :stale_if_error)
  # Serve the cached value.
  Fresh = Data.define(:age)
  # Serve the cached value, stale. `revalidation` is :lead (refresh in the
  # background and report with `lease_id`), :in_flight, :backoff (quiet until
  # `retry_at`), or the reason no refresh could be registered.
  Stale = Data.define(:age, :revalidation, :lease_id, :retry_at)
  # Nothing servable: fetch, then report with `lease_id`.
  Fetch = Data.define(:lease_id, :key)
  # Nothing servable and someone is already fetching: park under `waiter_id`.
  Wait = Data.define(:waiter_id, :key)
  # One waiter to wake. `outcome` is :completed or :abandoned.
  Notification = Data.define(:waiter_id, :key, :outcome)
end

# Native is required: no algorithm fallback. It builds the Data values above
# and adds the Policy predicates, so they must exist first.
require 'refresh_machines_native/refresh_machines_native'
