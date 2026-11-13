//! Sweeping expired metadata into a caller-sized buffer.
use super::{RULES, fetch, t};
use crate::*;

#[test]
fn sweep_drops_expired_metadata_in_bounded_resumable_batches() {
    let mut refresher = Refresher::<4, 2, 2>::new();
    for key in 0..3 {
        assert_eq!(
            refresher.put(key, t(0), t(0), Policy::new(t(1), t(0), t(0))),
            Ok(())
        );
    }
    assert_eq!(refresher.put(3, t(0), t(0), RULES), Ok(()));
    let busy = fetch(&mut refresher, 2, 5);
    let mut out = [77; 1];
    assert_eq!(refresher.sweep(t(5), &mut []), Ok(0));
    assert_eq!(refresher.sweep(t(5), &mut out), Ok(1));
    assert_eq!(out, [0]);
    // Entries past the count keep whatever the caller put there.
    let mut out = [77; 4];
    assert_eq!(refresher.sweep(t(5), &mut out), Ok(1));
    assert_eq!(out, [1, 77, 77, 77]);
    // Key 2 is expired but being refetched; key 3 is still fresh.
    assert_eq!(refresher.sweep(t(5), &mut out), Ok(0));
    assert_eq!(out, [1, 77, 77, 77]);
    assert_eq!(refresher.len(), 2);
    assert_eq!(refresher.abandon(busy), Ok(()));
    assert_eq!(refresher.sweep(t(5), &mut out), Ok(1));
    assert_eq!(out, [2, 77, 77, 77]);
    assert!(!refresher.is_empty());
}
