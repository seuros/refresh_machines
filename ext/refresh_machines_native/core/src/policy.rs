//! Freshness rules over explicit ages; no table, no clock.

use core::time::Duration;

/// Where a cached value stands against its [`Policy`] at a given age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Younger than `fresh_for`: serve it, no questions asked.
    Fresh,
    /// Past `fresh_for` but inside at least one stale window: servable only
    /// under that window's condition.
    Stale,
    /// Past every window: not servable under any condition.
    Expired,
}

/// Freshness rules for one stored value.
///
/// Both stale windows open the moment the value stops being fresh, as in
/// RFC 5861. All three are spans; zero disables the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The value is fresh while its age is below this.
    pub fresh_for: Duration,
    /// After freshness ends, the value may be served for this long while a
    /// revalidation runs in the background.
    pub stale_while_revalidate: Duration,
    /// After freshness ends, the value may be served for this long when a
    /// refresh fails.
    pub stale_if_error: Duration,
}

impl Policy {
    /// Build a policy from its three durations.
    #[must_use]
    pub const fn new(
        fresh_for: Duration,
        stale_while_revalidate: Duration,
        stale_if_error: Duration,
    ) -> Self {
        Self {
            fresh_for,
            stale_while_revalidate,
            stale_if_error,
        }
    }

    /// Classify a value of the given age.
    #[must_use]
    pub const fn state(&self, age: Duration) -> State {
        if age.as_nanos() < self.fresh_for.as_nanos() {
            State::Fresh
        } else if self.while_revalidate(age) || self.if_error(age) {
            State::Stale
        } else {
            State::Expired
        }
    }

    /// Whether a value of this age may be served while it is revalidated.
    #[must_use]
    pub const fn while_revalidate(&self, age: Duration) -> bool {
        self.within(age, self.stale_while_revalidate)
    }

    /// Whether a value of this age may be served because a refresh failed.
    #[must_use]
    pub const fn if_error(&self, age: Duration) -> bool {
        self.within(age, self.stale_if_error)
    }

    /// Whether `age` is past freshness by less than `window`.
    const fn within(&self, age: Duration, window: Duration) -> bool {
        match age.checked_sub(self.fresh_for) {
            Some(past) => past.as_nanos() < window.as_nanos(),
            None => false,
        }
    }
}
