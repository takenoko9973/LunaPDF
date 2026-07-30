use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

#[derive(Debug)]
struct Entry<V> {
    value: V,
    weight: usize,
}

/// 重み付き値を1つ挿入したときの変更内容を表す。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InsertOutcome<K, V> {
    /// 新しい値がキャッシュに保持されたかどうか。
    pub(crate) inserted: bool,
    /// キーがすでに存在していた場合に、挿入によって置き換えられた値。
    pub(crate) replaced: Option<(K, V)>,
    /// バイト予算を満たすために古い順から追い出された値。
    pub(crate) evicted: Vec<(K, V)>,
}

/// エントリのバイト重みの合計で制約されるLRUキャッシュ。
///
/// [`Self::get`]の成功時にキーへアクセス時刻を付ける。挿入とアクセスの順序は決定的で、
/// 内部順序の先頭が最古のエントリとなり、バイト予算のために追い出されたエントリもその順で返す。
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
    /// 指定された最大バイト数で空のキャッシュを作成する。
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            budget,
            current_bytes: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    // 予算と件数は診断パネル専用なので、リリース版ではアクセサも生成しない。
    /// このキャッシュが保持できる最大バイト数を返す。
    #[cfg(debug_assertions)]
    pub(crate) fn budget(&self) -> usize {
        self.budget
    }

    /// 保持中の全エントリの重みの合計を返す。
    // 追い出し後の不変条件をリリーステストでも検証するため、この値はテスト時に残す。
    #[cfg(any(debug_assertions, test))]
    pub(crate) fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    /// 保持中のエントリ数を返す。
    #[cfg(debug_assertions)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// キャッシュに保持中のエントリがないかどうかを返す。
    #[cfg(any(debug_assertions, test))]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// バイト重み付きで値を挿入し、置換と追い出しを報告する。
    ///
    /// 予算全体より重いエントリは、同じキーの以前の値を削除した後に拒否される（`inserted == false`）。
    /// これにより置換も他の挿入と同じように扱われ、キャッシュの不変条件を決して満たせない
    /// エントリを保持しない。
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
            // 順序とマップは一緒に維持されるため、このキーは存在する。
            // 将来の変更で不変条件が破られた場合にもパニックしないよう分岐を明示する。
            if let Some(entry) = self.entries.remove(&oldest_key) {
                self.current_bytes -= entry.weight;
                outcome.evicted.push((oldest_key, entry.value));
            }
        }

        // 上のループにより加算後も予算内に収まることが確定する。
        // 挿入前のchecked算術はusizeのオーバーフローも処理する。
        self.current_bytes = self
            .current_bytes
            .checked_add(weight)
            .expect("cache byte count must fit within its budget");
        self.entries.insert(key.clone(), Entry { value, weight });
        self.order.push_back(key);
        outcome.inserted = true;
        outcome
    }

    /// 保持中の値を返し、そのキーを最も最近使われたものとして記録する。
    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        if !self.touch(key) {
            return None;
        }
        self.entries.get(key).map(|entry| &entry.value)
    }

    /// 保持中のキーを最も最近使われたものとして記録し、存在するかどうかを報告する。
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

    /// キーを削除し、保持されていればその値とバイト重みを返す。
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
