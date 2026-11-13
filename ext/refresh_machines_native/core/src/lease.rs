use periscope_machines::scope_of;

/// The right to refresh one key, and the token that reports how it went.
///
/// Issued by [`crate::Refresher::decide`]. Never reissued: once the refresh is
/// reported, abandoned, reaped or taken over, the lease is
/// [`crate::Refusal::Stale`] forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lease(pub(crate) u64);

impl Lease {
    /// Rebuild a lease from [`Self::raw`], e.g. after it crossed an FFI or queue boundary.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The lease as a plain integer. Never zero for an issued lease.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The scope of the refresher that issued this lease, see [`crate::Refresher::with_scope`].
    #[must_use]
    pub const fn scope(self) -> u16 {
        scope_of(self.0)
    }
}
