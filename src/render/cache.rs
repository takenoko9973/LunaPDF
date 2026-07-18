use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

#[derive(Debug)]
struct Entry<V> {
    value: V,
    weight: usize,
}

/// Describes the changes made by inserting one weighted value.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InsertOutcome<K, V> {
    /// Whether the new value was retained in the cache.
    pub(crate) inserted: bool,
    /// The value replaced by the insertion, if the key was already present.
    pub(crate) replaced: Option<(K, V)>,
    /// Values evicted from oldest to newest to satisfy the byte budget.
    pub(crate) evicted: Vec<(K, V)>,
}

/// A least-recently-used cache constrained by the sum of entry byte weights.
///
/// Keys are touched on successful [`Self::get`] calls. Insertion and access
/// are deterministic: the front of the internal order is the oldest entry,
/// and entries evicted for the byte budget are returned in that order.
pub(crate) struct WeightedLruCache<K, V>
where
    K: Clone + Eq + Hash,
{
    budget: usize,
    current_bytes: usize,
    entries: HashMap<K, Entry<V>>,
    order: VecDeque<K>,
}

impl<K, V> WeightedLruCache<K, V>
where
    K: Clone + Eq + Hash,
{
    /// Creates an empty cache with the supplied maximum number of bytes.
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            budget,
            current_bytes: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Returns the maximum number of bytes retained by this cache.
    pub(crate) fn budget(&self) -> usize {
        self.budget
    }

    /// Returns the sum of weights of all retained entries.
    pub(crate) fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    /// Returns the number of retained entries.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the cache has no retained entries.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Inserts a value with its byte weight and reports replacements/evictions.
    ///
    /// An entry heavier than the whole budget is rejected (`inserted == false`)
    /// after any previous value for the same key is removed. This makes a
    /// replacement behave like every other insertion and avoids retaining an
    /// entry that can never satisfy the cache's invariant.
    pub(crate) fn insert(&mut self, key: K, value: V, weight: usize) -> InsertOutcome<K, V> {
        let replaced = self
            .remove_entry(&key)
            .map(|(old_key, entry)| (old_key, entry.value));
        let mut outcome = InsertOutcome {
            inserted: false,
            replaced,
            evicted: Vec::new(),
        };

        if weight > self.budget {
            return outcome;
        }

        while self
            .current_bytes
            .checked_add(weight)
            .is_none_or(|total| total > self.budget)
        {
            let Some(oldest_key) = self.order.pop_front() else {
                break;
            };
            // The order and map are maintained together, so this key is
            // present. Keeping the branch explicit avoids panicking if that
            // invariant is ever violated during a future change.
            if let Some(entry) = self.entries.remove(&oldest_key) {
                self.current_bytes -= entry.weight;
                outcome.evicted.push((oldest_key, entry.value));
            }
        }

        // The loop above establishes that this addition fits the budget;
        // checked arithmetic also handles a usize overflow before insertion.
        self.current_bytes = self
            .current_bytes
            .checked_add(weight)
            .expect("cache byte count must fit within its budget");
        self.entries.insert(key.clone(), Entry { value, weight });
        self.order.push_back(key);
        outcome.inserted = true;
        outcome
    }

    /// Returns a retained value and marks its key as most recently used.
    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        if !self.touch(key) {
            return None;
        }
        self.entries.get(key).map(|entry| &entry.value)
    }

    /// Marks a retained key as most recently used and reports whether it exists.
    pub(crate) fn touch(&mut self, key: &K) -> bool {
        let Some(position) = self.order.iter().position(|candidate| candidate == key) else {
            return false;
        };
        let touched_key = self
            .order
            .remove(position)
            .expect("position came from the order iterator");
        self.order.push_back(touched_key);
        true
    }

    /// Removes a key and returns its value and byte weight, if retained.
    pub(crate) fn remove(&mut self, key: &K) -> Option<(V, usize)> {
        self.remove_entry(key)
            .map(|(_, entry)| (entry.value, entry.weight))
    }

    fn remove_entry(&mut self, key: &K) -> Option<(K, Entry<V>)> {
        let removed = self.entries.remove_entry(key)?;
        self.current_bytes -= removed.1.weight;
        if let Some(position) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(position);
        }
        Some(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_evicts_oldest_entries_to_stay_under_byte_budget() {
        let mut cache = WeightedLruCache::new(10);
        assert!(cache.insert("a", "A", 6).inserted);
        assert!(cache.insert("b", "B", 4).inserted);

        let outcome = cache.insert("c", "C", 5);

        assert_eq!(outcome.evicted, vec![("a", "A")]);
        assert_eq!(cache.current_bytes(), 9);
        assert_eq!(cache.get(&"b"), Some(&"B"));
        assert_eq!(cache.get(&"c"), Some(&"C"));
    }

    #[test]
    fn access_updates_lru_order() {
        let mut cache = WeightedLruCache::new(10);
        cache.insert("a", "A", 4);
        cache.insert("b", "B", 4);
        assert_eq!(cache.get(&"a"), Some(&"A"));

        let outcome = cache.insert("c", "C", 4);

        assert_eq!(outcome.evicted, vec![("b", "B")]);
        assert!(cache.get(&"a").is_some());
        assert!(cache.get(&"c").is_some());
    }

    #[test]
    fn replacement_subtracts_old_weight_before_admitting_new_value() {
        let mut cache = WeightedLruCache::new(10);
        cache.insert("a", "old", 8);

        let outcome = cache.insert("a", "new", 3);

        assert_eq!(outcome.replaced, Some(("a", "old")));
        assert!(outcome.evicted.is_empty());
        assert_eq!(cache.current_bytes(), 3);
        assert_eq!(cache.get(&"a"), Some(&"new"));
    }

    #[test]
    fn oversized_entry_is_reported_and_not_retained() {
        let mut cache = WeightedLruCache::new(4);
        let outcome = cache.insert("a", "A", 5);

        assert!(!outcome.inserted);
        assert!(outcome.replaced.is_none());
        assert!(outcome.evicted.is_empty());
        assert!(cache.is_empty());
        assert_eq!(cache.current_bytes(), 0);
    }

    #[test]
    fn byte_count_overflow_is_treated_as_budget_excess() {
        let mut cache = WeightedLruCache::new(usize::MAX);
        cache.insert("old", "O", usize::MAX);

        let outcome = cache.insert("new", "N", 1);

        assert_eq!(outcome.evicted, vec![("old", "O")]);
        assert_eq!(cache.current_bytes(), 1);
        assert_eq!(cache.get(&"new"), Some(&"N"));
    }
}
