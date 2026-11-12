//! Freshness rules over explicit ages; no table, no time.

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

/// Freshness rules for one stored value, in the caller's time unit.
///
/// Both stale windows open the moment the value stops being fresh, as in
/// RFC 5861. All three are durations; zero disables the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The value is fresh while its age is below this.
    pub fresh_for: u64,
    /// After freshness ends, the value may be served for this long while a
    /// revalidation runs in the background.
    pub stale_while_revalidate: u64,
    /// After freshness ends, the value may be served for this long when a
    /// refresh fails.
    pub stale_if_error: u64,
}

impl Policy {
    /// Build a policy from its three durations.
    #[must_use]
    pub const fn new(fresh_for: u64, stale_while_revalidate: u64, stale_if_error: u64) -> Self {
        Self {
            fresh_for,
            stale_while_revalidate,
            stale_if_error,
        }
    }

    /// Classify a value of the given age.
    #[must_use]
    pub const fn state(&self, age: u64) -> State {
        if age < self.fresh_for {
            State::Fresh
        } else if self.while_revalidate(age) || self.if_error(age) {
            State::Stale
        } else {
            State::Expired
        }
    }

    /// Whether a value of this age may be served while it is revalidated.
    #[must_use]
    pub const fn while_revalidate(&self, age: u64) -> bool {
        age >= self.fresh_for && age - self.fresh_for < self.stale_while_revalidate
    }

    /// Whether a value of this age may be served because a refresh failed.
    #[must_use]
    pub const fn if_error(&self, age: u64) -> bool {
        age >= self.fresh_for && age - self.fresh_for < self.stale_if_error
    }
}
