use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::id::DeviceId;

/// A per-file version vector.
///
/// A timestamp cannot tell whether two hosts edited a file concurrently: clocks
/// drift, copies preserve mtimes, and coarse filesystems round them. If neither
/// side's vector dominates the other's, the changes are concurrent even when one
/// mtime happens to be larger.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VersionVector(pub BTreeMap<DeviceId, u64>);

impl VersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment(&mut self, device: DeviceId) -> u64 {
        let counter = self.0.entry(device).or_insert(0);
        *counter += 1;
        *counter
    }

    pub fn set(&mut self, device: DeviceId, counter: u64) {
        self.0.insert(device, counter);
    }

    pub fn get(&self, device: DeviceId) -> u64 {
        self.0.get(&device).copied().unwrap_or(0)
    }

    pub fn merge(&mut self, other: &Self) {
        for (device, counter) in &other.0 {
            let slot = self.0.entry(*device).or_insert(0);
            *slot = (*slot).max(*counter);
        }
    }

    /// True when every component of `self` is at least the corresponding
    /// component of `other`. Equality counts as domination.
    pub fn dominates(&self, other: &Self) -> bool {
        other
            .0
            .iter()
            .all(|(device, counter)| self.get(*device) >= *counter)
    }

    /// True when the two vectors changed different histories. A vector that
    /// dominates the other is not concurrent, even if it is newer.
    pub fn concurrent(&self, other: &Self) -> bool {
        self != other && !self.dominates(other) && !other.dominates(self)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn counters(&self) -> impl Iterator<Item = (DeviceId, u64)> + '_ {
        self.0.iter().map(|(device, counter)| (*device, *counter))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn increments_only_the_local_counter() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let mut vector = VersionVector::new();
        vector.increment(a);
        vector.increment(a);
        vector.increment(b);
        assert_eq!(vector.get(a), 2);
        assert_eq!(vector.get(b), 1);
    }

    #[test]
    fn detects_concurrency() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let mut left = VersionVector::new();
        left.increment(a);
        let mut right = VersionVector::new();
        right.increment(b);
        assert!(left.concurrent(&right));
        assert!(!left.dominates(&right));
    }

    #[test]
    fn fast_forward_is_not_a_conflict() {
        let a = DeviceId::new();
        let mut old = VersionVector::new();
        old.increment(a);
        let mut new = old.clone();
        new.increment(a);
        assert!(new.dominates(&old));
        assert!(!new.concurrent(&old));
    }

    #[test]
    fn merge_is_pointwise_max() {
        let a = DeviceId::new();
        let b = DeviceId::new();
        let mut left = VersionVector::new();
        left.set(a, 3);
        left.set(b, 1);
        let mut right = VersionVector::new();
        right.set(a, 1);
        right.set(b, 7);
        left.merge(&right);
        assert_eq!(left.get(a), 3);
        assert_eq!(left.get(b), 7);
    }
}
