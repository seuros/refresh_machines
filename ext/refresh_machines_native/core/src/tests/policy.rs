//! A policy classifies explicit ages, with no table and no clock.
use super::{MAX, NEAR, RULES, t};
use crate::*;

#[test]
fn policy_windows_are_half_open_and_start_when_freshness_ends() {
    assert_eq!(RULES.state(t(0)), State::Fresh);
    assert_eq!(RULES.state(t(9)), State::Fresh);
    assert_eq!(RULES.state(t(10)), State::Stale);
    assert!(RULES.while_revalidate(t(10)) && RULES.while_revalidate(t(14)));
    assert!(!RULES.while_revalidate(t(9)) && !RULES.while_revalidate(t(15)));
    assert!(RULES.if_error(t(10)) && RULES.if_error(t(29)));
    assert!(!RULES.if_error(t(9)) && !RULES.if_error(t(30)));
    assert_eq!(RULES.state(t(29)), State::Stale);
    assert_eq!(RULES.state(t(30)), State::Expired);

    let strict = Policy::new(t(10), t(0), t(0));
    assert_eq!(strict.state(t(10)), State::Expired);
    let never_fresh = Policy::new(t(0), t(0), t(0));
    assert_eq!(never_fresh.state(t(0)), State::Expired);
    let forever = Policy::new(MAX, MAX, MAX);
    assert_eq!(forever.state(NEAR), State::Fresh);
    assert_eq!(forever.state(MAX), State::Stale);
    let wide = Policy::new(t(1), MAX, t(0));
    assert!(wide.while_revalidate(MAX) && !wide.if_error(MAX));
}
