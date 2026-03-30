use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Causality relationship between two vector clocks
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockRelation {
    /// Self happened before Other
    Before,
    /// Self happened after Other
    After,
    /// Clocks are identical
    Equal,
    /// Clocks are concurrent (conflict)
    Concurrent,
}

/// VectorClock tracks causality between distributed nodes.
/// Map format: {instance_id: logical_version}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VectorClock(pub HashMap<String, i64>);

impl VectorClock {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Increment the clock for a given instance
    pub fn increment(&mut self, instance_id: &str) {
        let count = self.0.entry(instance_id.to_string()).or_insert(0);
        *count += 1;
    }

    /// Get the version for a given instance
    pub fn get(&self, instance_id: &str) -> i64 {
        self.0.get(instance_id).copied().unwrap_or(0)
    }

    /// Merge another clock into this one (take max of each component)
    pub fn merge(&mut self, other: &VectorClock) {
        for (k, &v) in &other.0 {
            let entry = self.0.entry(k.clone()).or_insert(0);
            if v > *entry {
                *entry = v;
            }
        }
    }

    /// Compare two vector clocks to determine causality
    pub fn compare(&self, other: &VectorClock) -> ClockRelation {
        let mut less_or_equal = true;
        let mut greater_or_equal = true;

        let all_instances: HashSet<&String> =
            self.0.keys().chain(other.0.keys()).collect();

        for instance in all_instances {
            let v1 = self.0.get(instance).copied().unwrap_or(0);
            let v2 = other.0.get(instance).copied().unwrap_or(0);

            if v1 > v2 {
                less_or_equal = false;
            }
            if v1 < v2 {
                greater_or_equal = false;
            }
        }

        if less_or_equal && greater_or_equal {
            ClockRelation::Equal
        } else if less_or_equal {
            ClockRelation::Before
        } else if greater_or_equal {
            ClockRelation::After
        } else {
            ClockRelation::Concurrent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_increment_and_get() {
        let mut vc = VectorClock::new();
        vc.increment("node_a");
        vc.increment("node_a");
        vc.increment("node_b");
        assert_eq!(vc.get("node_a"), 2);
        assert_eq!(vc.get("node_b"), 1);
        assert_eq!(vc.get("node_c"), 0);
    }

    #[test]
    fn test_compare_equal() {
        let mut a = VectorClock::new();
        a.increment("x");
        let b = a.clone();
        assert_eq!(a.compare(&b), ClockRelation::Equal);
    }

    #[test]
    fn test_compare_before_after() {
        let mut a = VectorClock::new();
        a.increment("x");
        let mut b = a.clone();
        b.increment("x");
        assert_eq!(a.compare(&b), ClockRelation::Before);
        assert_eq!(b.compare(&a), ClockRelation::After);
    }

    #[test]
    fn test_compare_concurrent() {
        let mut a = VectorClock::new();
        a.increment("node_a");
        let mut b = VectorClock::new();
        b.increment("node_b");
        assert_eq!(a.compare(&b), ClockRelation::Concurrent);
    }

    #[test]
    fn test_merge() {
        let mut a = VectorClock::new();
        a.0.insert("x".into(), 3);
        a.0.insert("y".into(), 1);
        let mut b = VectorClock::new();
        b.0.insert("x".into(), 1);
        b.0.insert("y".into(), 5);
        b.0.insert("z".into(), 2);

        a.merge(&b);
        assert_eq!(a.get("x"), 3);
        assert_eq!(a.get("y"), 5);
        assert_eq!(a.get("z"), 2);
    }
}
