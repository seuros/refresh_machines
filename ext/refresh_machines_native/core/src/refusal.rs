use core::fmt;

/// Why a call changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `now` is earlier than a time this refresher already accepted.
    TimeRegressed,
    /// A zero lead duration: the lease would be overdue before it was issued.
    ZeroLead,
    /// Every metadata slot holds another key.
    EntriesFull,
    /// Every refresh slot holds an unsettled refresh; no new key can be led.
    FlightsFull,
    /// Every waiter slot holds a waiting caller or an undrained notification.
    WaitersFull,
    /// The id space is spent; this refresher can register nothing more.
    Exhausted,
    /// The lease or waiter names nothing live.
    Stale,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TimeRegressed => "time moved backwards",
            Self::ZeroLead => "lead duration must be positive",
            Self::EntriesFull => "every metadata slot is in use",
            Self::FlightsFull => "every refresh slot is in use",
            Self::WaitersFull => "every waiter slot is in use",
            Self::Exhausted => "id space exhausted",
            Self::Stale => "id does not name a live lease or waiter",
        })
    }
}

impl core::error::Error for Refusal {}

impl From<coalesce_machines::Refusal> for Refusal {
    fn from(refusal: coalesce_machines::Refusal) -> Self {
        match refusal {
            coalesce_machines::Refusal::FlightsFull => Self::FlightsFull,
            coalesce_machines::Refusal::WaitersFull => Self::WaitersFull,
            coalesce_machines::Refusal::Exhausted => Self::Exhausted,
            coalesce_machines::Refusal::Stale => Self::Stale,
        }
    }
}
