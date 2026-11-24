use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ELEMENT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct Entry<T> {
    id: u64,
    value: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Front,
    Back,
}

/// A contiguous structural subtree resolved from the Deque's reverse-preorder
/// sequence and parallel span boundaries. The range contains no parent pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuralRange {
    pub start: usize,
    pub count: usize,
    pub end: End,
}

/// A stable-identity, double-ended segment of the canonical UI hierarchy.
/// Structural operations are intentionally expressed in terms of its two ends.
#[derive(Debug)]
pub struct Deque<T> {
    entries: VecDeque<Entry<T>>,
    subtree_spans: VecDeque<usize>,
    entry_indices: HashMap<u64, usize>,
    parent_indices: Vec<Option<usize>>,
    identity_hash: u64,
    ghost: bool,
}

impl<T> Default for Deque<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Deque<T> {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let mut value = Self {
            entries: VecDeque::with_capacity(capacity),
            subtree_spans: VecDeque::with_capacity(capacity),
            entry_indices: HashMap::with_capacity(capacity),
            parent_indices: Vec::with_capacity(capacity),
            identity_hash: 0,
            ghost: true,
        };
        value.refresh_hash();
        value
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.entries.capacity()
    }
    pub fn is_ghost(&self) -> bool {
        self.ghost
    }
    pub fn identity_hash(&self) -> u64 {
        self.identity_hash
    }

    fn fresh_id() -> u64 {
        NEXT_ELEMENT_ID.fetch_add(1, Ordering::Relaxed)
    }

    /// Rebuilds lookup indexes from the canonical reverse-preorder sequence.
    /// `parent_indices` is only a derived acceleration cache: subtree spans
    /// remain the sole source of structural truth.
    fn refresh_hash(&mut self) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.ghost.hash(&mut h);
        self.entries.len().hash(&mut h);

        self.entry_indices.clear();
        self.entry_indices.reserve(self.entries.len());
        self.parent_indices.clear();
        self.parent_indices.resize(self.entries.len(), None);

        // The stack contains roots of completed, disjoint subtrees. When an
        // ancestor arrives, its span identifies exactly the suffix roots it
        // contains; those roots acquire this entry as their direct parent.
        let mut roots = Vec::<usize>::new();
        for (index, (entry, span)) in self.entries.iter().zip(&self.subtree_spans).enumerate() {
            entry.id.hash(&mut h);
            span.hash(&mut h);
            self.entry_indices.insert(entry.id, index);

            let start = index + 1 - (*span).min(index + 1);
            while roots.last().is_some_and(|root| *root >= start) {
                let child_root = roots.pop().expect("checked stack top");
                self.parent_indices[child_root] = Some(index);
            }
            roots.push(index);
        }
        self.identity_hash = h.finish();
    }

    /// Inserts a new last child in the root element's logical child order.
    /// Physical Front is logical Back because Deque storage is reverse preorder.
    pub fn push_front(&mut self, value: T) -> u64 {
        let id = Self::fresh_id();
        if let Some(root_span) = self.subtree_spans.back_mut() {
            *root_span += 1;
        }
        self.entries.push_front(Entry { id, value });
        self.subtree_spans.push_front(1);
        self.ghost = false;
        self.refresh_hash();
        id
    }

    /// Appends a new ancestor around the Deque's existing subtree at physical Back.
    pub fn push_back(&mut self, value: T) -> u64 {
        let id = Self::fresh_id();
        let span = self.entries.len() + 1;
        self.entries.push_back(Entry { id, value });
        self.subtree_spans.push_back(span);
        self.ghost = false;
        self.refresh_hash();
        id
    }
    /// Builds a Deque from an already ordered reverse-preorder snapshot in one pass.
    /// Indexes and the identity hash are rebuilt once after all entries are appended.
    pub(crate) fn from_ordered_spans(
        items: impl IntoIterator<Item = (T, usize)>,
    ) -> Option<(Self, Vec<u64>)> {
        let items = items.into_iter();
        let (lower_bound, upper_bound) = items.size_hint();
        let capacity = upper_bound.unwrap_or(lower_bound);
        let mut deque = Self::with_capacity(capacity);
        let mut ids = Vec::with_capacity(capacity);

        for (value, span) in items {
            if span == 0 || span > deque.entries.len() + 1 {
                return None;
            }
            let id = Self::fresh_id();
            deque.entries.push_back(Entry { id, value });
            deque.subtree_spans.push_back(span);
            ids.push(id);
        }

        deque.ghost = deque.entries.is_empty();
        deque.refresh_hash();
        Some((deque, ids))
    }

    /// Adds one entry for focused structural unit tests.
    #[cfg(test)]
    pub(crate) fn push_back_with_span(&mut self, value: T, span: usize) -> Option<u64> {
        if span == 0 || span > self.entries.len() + 1 {
            return None;
        }
        let id = Self::fresh_id();
        self.entries.push_back(Entry { id, value });
        self.subtree_spans.push_back(span);
        self.ghost = false;
        self.refresh_hash();
        Some(id)
    }

    pub fn subtree_spans(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        self.subtree_spans.iter().copied()
    }
    /// Checks that subtree intervals are nested or disjoint, never crossing.
    /// A range-minimum tree detects crossings even when an earlier interval
    /// would no longer be present on a simple nesting stack.
    pub fn validate_structure(&self) -> bool {
        let len = self.entries.len();
        if len != self.subtree_spans.len() {
            return false;
        }
        if len == 0 {
            return true;
        }

        let Some(size) = len.checked_next_power_of_two() else {
            return false;
        };
        let Some(tree_len) = size.checked_mul(2) else {
            return false;
        };
        let mut minimum_starts = vec![usize::MAX; tree_len];

        for (index, span) in self.subtree_spans.iter().copied().enumerate() {
            if span == 0 || span > index + 1 {
                return false;
            }
            let start = index + 1 - span;

            // Every earlier interval ending at or after `start` must begin
            // at or after `start`; otherwise the two intervals cross.
            let mut left = start + size;
            let mut right = index + size;
            let mut previous_minimum = usize::MAX;
            while left < right {
                if left & 1 == 1 {
                    previous_minimum = previous_minimum.min(minimum_starts[left]);
                    left += 1;
                }
                if right & 1 == 1 {
                    right -= 1;
                    previous_minimum = previous_minimum.min(minimum_starts[right]);
                }
                left /= 2;
                right /= 2;
            }
            if previous_minimum < start {
                return false;
            }

            let mut position = index + size;
            minimum_starts[position] = start;
            while position > 1 {
                position /= 2;
                minimum_starts[position] =
                    minimum_starts[position * 2].min(minimum_starts[position * 2 + 1]);
            }
        }

        self.subtree_spans.back().copied() == Some(len)
    }

    pub fn logical_parent(&self, id: u64) -> Option<u64> {
        let index = self.index_of(id)?;
        let parent_index = self.parent_indices.get(index).copied().flatten()?;
        self.entries.get(parent_index).map(|entry| entry.id)
    }

    pub fn logical_children(&self, id: u64) -> Vec<u64> {
        let Some(parent_index) = self.index_of(id) else {
            return Vec::new();
        };
        let span = self.subtree_spans[parent_index];
        if span == 0 || span > parent_index + 1 {
            return Vec::new();
        }
        let start = parent_index + 1 - span;
        let mut children = Vec::new();
        let mut cursor = parent_index;

        // Reverse-preorder stores each direct child's complete subtree before
        // the child root. Walking backward from the parent therefore jumps
        // from one direct child root to the previous subtree boundary, without
        // scanning any grandchildren or unrelated descendants.
        while cursor > start {
            let child_index = cursor - 1;
            let child_span = self.subtree_spans[child_index];
            if child_span == 0 || child_span > child_index + 1 {
                return Vec::new();
            }
            let child_start = child_index + 1 - child_span;
            if child_start < start {
                return Vec::new();
            }
            children.push(self.entries[child_index].id);
            cursor = child_start;
        }
        children
    }

    pub fn are_siblings(&self, left: u64, right: u64) -> bool {
        if left == right {
            return false;
        }
        let Some(parent) = self.logical_parent(left) else {
            return false;
        };
        self.logical_parent(right) == Some(parent)
    }

    pub fn front(&self) -> Option<&T> {
        self.entries.front().map(|e| &e.value)
    }
    pub fn back(&self) -> Option<&T> {
        self.entries.back().map(|e| &e.value)
    }
    pub fn front_mut(&mut self) -> Option<&mut T> {
        self.entries.front_mut().map(|e| &mut e.value)
    }
    pub fn back_mut(&mut self) -> Option<&mut T> {
        self.entries.back_mut().map(|e| &mut e.value)
    }
    pub fn get(&self, index: usize) -> Option<&T> {
        self.entries.get(index).map(|e| &e.value)
    }

    pub(crate) fn entry_id_at(&self, index: usize) -> Option<u64> {
        self.entries.get(index).map(|entry| entry.id)
    }
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.entries.get_mut(index).map(|e| &mut e.value)
    }

    pub fn get_by_id(&self, id: u64) -> Option<&T> {
        self.entries
            .get(*self.entry_indices.get(&id)?)
            .map(|entry| &entry.value)
    }
    pub fn get_mut_by_id(&mut self, id: u64) -> Option<&mut T> {
        let index = *self.entry_indices.get(&id)?;
        self.entries.get_mut(index).map(|entry| &mut entry.value)
    }
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.entry_indices.get(&id).copied()
    }
    /// Resolves a subtree from the parallel boundary array. Storage order is
    /// reverse preorder: descendants precede their ancestor.
    pub fn structural_range(&self, anchor: u64) -> Option<StructuralRange> {
        let index = self.index_of(anchor)?;
        let count = *self.subtree_spans.get(index)?;
        if count == 0 || count > index + 1 {
            return None;
        }
        let start = index + 1 - count;
        let suffix = self.entries.len() - (start + count);
        let end = if start == 0 && suffix == 0 {
            End::Back
        } else if start == 0 {
            End::Front
        } else if suffix == 0 || suffix < start {
            End::Back
        } else {
            End::Front
        };
        Some(StructuralRange { start, count, end })
    }

    fn extract_subtree_entries(&mut self, range: StructuralRange) -> (Vec<Entry<T>>, Vec<usize>) {
        let range_end = range.start + range.count;
        for (index, span) in self.subtree_spans.iter_mut().enumerate().skip(range_end) {
            let old_start = index + 1 - *span;
            if old_start <= range.start {
                *span -= range.count;
            }
        }

        let (removed_entries, removed_spans) = if range.end == End::Front {
            let prefix_entries: Vec<_> = self.entries.drain(..range.start).collect();
            let prefix_spans: Vec<_> = self.subtree_spans.drain(..range.start).collect();
            let removed_entries: Vec<_> = self.entries.drain(..range.count).collect();
            let removed_spans: Vec<_> = self.subtree_spans.drain(..range.count).collect();
            for entry in prefix_entries.into_iter().rev() {
                self.entries.push_front(entry);
            }
            for span in prefix_spans.into_iter().rev() {
                self.subtree_spans.push_front(span);
            }
            (removed_entries, removed_spans)
        } else {
            let suffix_entries: Vec<_> = self.entries.drain(range_end..).collect();
            let suffix_spans: Vec<_> = self.subtree_spans.drain(range_end..).collect();
            let removed_entries: Vec<_> = self
                .entries
                .drain(range.start..range.start + range.count)
                .collect();
            let removed_spans: Vec<_> = self
                .subtree_spans
                .drain(range.start..range.start + range.count)
                .collect();
            self.entries.extend(suffix_entries);
            self.subtree_spans.extend(suffix_spans);
            (removed_entries, removed_spans)
        };
        self.refresh_hash();
        (removed_entries, removed_spans)
    }

    pub fn drain_subtree(&mut self, anchor: u64) -> Option<Vec<(u64, T)>> {
        let range = self.structural_range(anchor)?;
        let (entries, _) = self.extract_subtree_entries(range);
        Some(
            entries
                .into_iter()
                .map(|entry| (entry.id, entry.value))
                .collect(),
        )
    }

    pub fn transfer_subtree_to(
        &mut self,
        target: &mut Self,
        anchor: u64,
        target_end: End,
    ) -> Option<usize> {
        if std::ptr::eq(self, target) {
            return None;
        }
        let range = self.structural_range(anchor)?;
        let (moved_entries, moved_spans) = self.extract_subtree_entries(range);
        let moved_count = moved_entries.len();
        let target_old_len = target.entries.len();
        match target_end {
            End::Front => {
                if let Some(root_span) = target.subtree_spans.back_mut() {
                    *root_span += moved_count;
                }
                for (entry, span) in moved_entries.into_iter().zip(moved_spans).rev() {
                    target.entries.push_front(entry);
                    target.subtree_spans.push_front(span);
                }
            }
            End::Back => {
                for (entry, span) in moved_entries.into_iter().zip(moved_spans) {
                    target.entries.push_back(entry);
                    target
                        .subtree_spans
                        .push_back(if target_old_len > 0 && span == moved_count {
                            span + target_old_len
                        } else {
                            span
                        });
                }
            }
        }
        if moved_count > 0 {
            target.ghost = false;
        }
        target.refresh_hash();
        Some(moved_count)
    }

    pub fn ids(&self) -> impl DoubleEndedIterator<Item = u64> + '_ {
        self.entries.iter().map(|e| e.id)
    }
    pub(crate) fn ids_in_range(
        &self,
        start: usize,
        count: usize,
    ) -> impl Iterator<Item = u64> + '_ {
        self.entries
            .range(start..start.saturating_add(count))
            .map(|entry| entry.id)
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.entries.iter().map(|e| &e.value)
    }
    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut T> + ExactSizeIterator {
        self.entries.iter_mut().map(|e| &mut e.value)
    }

    /// Replaces a value without changing the entry's identity or structural hash.
    pub fn replace_value(&mut self, id: u64, value: T) -> Option<T> {
        let index = *self.entry_indices.get(&id)?;
        let entry = self.entries.get_mut(index)?;
        Some(std::mem::replace(&mut entry.value, value))
    }

    /// Drops every element while retaining allocation until sibling reconciliation.
    pub(crate) fn clear_for_reconcile(&mut self) -> Vec<u64> {
        if self.ghost {
            return Vec::new();
        }
        let ids = self.entries.drain(..).map(|entry| entry.id).collect();
        self.subtree_spans.clear();
        self.refresh_hash();
        ids
    }

    pub(crate) fn mark_ghost(&mut self) {
        debug_assert!(self.is_empty());
        self.entries = VecDeque::new();
        self.subtree_spans = VecDeque::new();
        self.ghost = true;
        self.refresh_hash();
    }

    pub(crate) fn transfer_empty_storage_to(&mut self, recipient: &mut Self) -> bool {
        if !self.is_empty() || recipient.is_empty() {
            return false;
        }
        let mut recipient_entries = std::mem::take(&mut recipient.entries);
        let mut recipient_spans = std::mem::take(&mut recipient.subtree_spans);
        recipient.entries = std::mem::take(&mut self.entries);
        recipient.entries.append(&mut recipient_entries);
        recipient.subtree_spans = std::mem::take(&mut self.subtree_spans);
        recipient.subtree_spans.append(&mut recipient_spans);
        self.ghost = true;
        recipient.ghost = false;
        self.refresh_hash();
        recipient.refresh_hash();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{Deque, End};

    #[test]
    fn structural_transfer_covers_front_and_back_source_and_target_ends() {
        for (anchor_is_root, target_end) in [
            (false, End::Front),
            (false, End::Back),
            (true, End::Front),
            (true, End::Back),
        ] {
            let mut source = Deque::new();
            let root = source.push_back("root");
            let first_child = source.push_front("first-child");
            let second_child = source.push_front("second-child");
            let anchor = if anchor_is_root { root } else { second_child };
            let mut target = Deque::new();
            target.push_back("existing");
            let moved_count = source
                .transfer_subtree_to(&mut target, anchor, target_end)
                .unwrap();
            assert_eq!(moved_count, if anchor_is_root { 3 } else { 1 });
            assert!(source.validate_structure());
            assert!(target.validate_structure());
            if anchor_is_root {
                assert!(source.is_empty());
                assert_eq!(
                    target.iter().copied().collect::<Vec<_>>(),
                    match target_end {
                        End::Front => vec!["second-child", "first-child", "root", "existing"],
                        End::Back => vec!["existing", "second-child", "first-child", "root"],
                    }
                );
            } else {
                assert_eq!(
                    source.iter().copied().collect::<Vec<_>>(),
                    vec!["first-child", "root"]
                );
                assert_eq!(
                    target.iter().copied().collect::<Vec<_>>(),
                    match target_end {
                        End::Front => vec!["second-child", "existing"],
                        End::Back => vec!["existing", "second-child"],
                    }
                );
            }
            assert!(
                source.index_of(first_child).is_some() || target.index_of(first_child).is_some()
            );
            assert!(
                source.index_of(second_child).is_some() || target.index_of(second_child).is_some()
            );
            assert!(source.index_of(root).is_some() || target.index_of(root).is_some());
        }
    }

    #[test]
    fn local_hash_tracks_order_but_not_in_place_values() {
        let mut d = Deque::new();
        let a = d.push_back(String::from("a"));
        d.push_back(String::from("b"));
        let hash = d.identity_hash();
        *d.get_mut_by_id(a).unwrap() = String::from("changed");
        assert_eq!(hash, d.identity_hash());
        d.push_front(String::from("child"));
        assert_ne!(hash, d.identity_hash());
    }

    #[test]
    fn structural_range_is_inferred_from_order_without_a_count() {
        let mut deque = Deque::new();
        let leaf = deque.push_front("leaf");
        let parent = deque.push_back("parent");
        let root_range = deque.structural_range(parent).unwrap();
        assert_eq!(
            root_range,
            super::StructuralRange {
                start: 0,
                end: End::Back,
                count: 2
            }
        );
        let leaf_range = deque.structural_range(leaf).unwrap();
        assert_eq!(
            leaf_range,
            super::StructuralRange {
                start: 0,
                end: End::Front,
                count: 1
            }
        );
    }

    #[test]
    fn removing_an_interior_anchor_removes_its_descendant_prefix_only() {
        let mut deque = Deque::new();
        let leaf = deque.push_back_with_span("leaf", 1).unwrap();
        let branch = deque.push_back_with_span("branch", 2).unwrap();
        let root = deque.push_back_with_span("root", 3).unwrap();
        assert_eq!(deque.logical_parent(leaf), Some(branch));
        assert_eq!(deque.logical_parent(branch), Some(root));
        let removed = deque.drain_subtree(branch).unwrap();
        assert_eq!(
            removed.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            vec!["leaf", "branch"]
        );
        assert_eq!(deque.back(), Some(&"root"));
        assert!(deque.index_of(leaf).is_none());
        assert!(deque.index_of(root).is_some());
    }

    #[test]
    fn subtree_spans_distinguish_siblings_from_parent_and_child() {
        let mut deque = Deque::new();
        let parent = deque.push_back("parent");
        let first = deque.push_front("first-child");
        let second = deque.push_front("second-child");
        assert_eq!(deque.logical_parent(first), Some(parent));
        assert_eq!(deque.logical_parent(second), Some(parent));
        assert!(deque.are_siblings(first, second));
        assert!(!deque.are_siblings(first, parent));
        assert_eq!(deque.logical_children(parent).len(), 2);
        assert_eq!(deque.structural_range(parent).unwrap().count, 3);
        assert_eq!(deque.structural_range(first).unwrap().count, 1);

        let removed = deque.drain_subtree(first).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(deque.index_of(second).is_some());
        assert_eq!(deque.structural_range(parent).unwrap().count, 2);
    }

    #[test]
    fn interior_subtree_can_be_extracted_from_back_while_preserving_the_suffix_parent() {
        let mut deque = Deque::new();
        let sibling_a = deque.push_back_with_span("sibling-a", 1).unwrap();
        let sibling_b = deque.push_back_with_span("sibling-b", 1).unwrap();
        let leaf = deque.push_back_with_span("leaf", 1).unwrap();
        let branch = deque.push_back_with_span("branch", 2).unwrap();
        let root = deque.push_back_with_span("root", 5).unwrap();
        let range = deque.structural_range(branch).unwrap();
        assert_eq!(
            range,
            super::StructuralRange {
                start: 2,
                count: 2,
                end: End::Back
            }
        );
        let removed = deque.drain_subtree(branch).unwrap();
        assert_eq!(
            removed.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            vec!["leaf", "branch"]
        );
        assert!(deque.index_of(sibling_a).is_some());
        assert!(deque.index_of(sibling_b).is_some());
        assert!(deque.index_of(leaf).is_none());
        assert!(deque.index_of(root).is_some());
        assert_eq!(deque.structural_range(root).unwrap().count, 3);
    }

    #[test]
    fn cached_parent_and_entry_indexes_follow_nested_spans_after_extraction() {
        let mut deque = Deque::new();
        let first_leaf = deque.push_back_with_span("first-leaf", 1).unwrap();
        let second_leaf = deque.push_back_with_span("second-leaf", 1).unwrap();
        let branch = deque.push_back_with_span("branch", 3).unwrap();
        let sibling = deque.push_back_with_span("sibling", 1).unwrap();
        let root = deque.push_back_with_span("root", 5).unwrap();

        assert_eq!(deque.logical_parent(first_leaf), Some(branch));
        assert_eq!(deque.logical_parent(second_leaf), Some(branch));
        assert_eq!(deque.logical_parent(branch), Some(root));
        assert_eq!(deque.logical_parent(sibling), Some(root));
        assert_eq!(deque.logical_children(root), vec![sibling, branch]);
        assert_eq!(
            deque.logical_children(branch),
            vec![second_leaf, first_leaf]
        );
        assert_eq!(deque.index_of(root), Some(4));
        assert_eq!(deque.get_by_id(sibling), Some(&"sibling"));

        let removed = deque.drain_subtree(branch).unwrap();
        assert_eq!(removed.len(), 3);
        assert_eq!(deque.index_of(sibling), Some(0));
        assert_eq!(deque.index_of(root), Some(1));
        assert_eq!(deque.logical_parent(sibling), Some(root));
        assert_eq!(deque.logical_parent(root), None);
        assert_eq!(deque.get_by_id(sibling), Some(&"sibling"));
        assert!(deque.validate_structure());
    }

    #[test]
    fn structural_validation_rejects_crossings_after_a_completed_interval() {
        let mut deque = Deque::new();
        for (value, span) in [(0, 1), (1, 1), (2, 2), (3, 1), (4, 3), (5, 6)] {
            assert!(deque.push_back_with_span(value, span).is_some());
        }
        assert!(!deque.validate_structure());
    }

    #[test]
    fn cached_parent_and_direct_child_indexes_match_all_small_valid_span_shapes() {
        fn verify_shapes(length: usize, index: usize, spans: &mut Vec<usize>, valid: &mut usize) {
            if index == length - 1 {
                spans.push(length);
                let (deque, ids) =
                    Deque::from_ordered_spans((0..length).zip(spans.iter().copied())).unwrap();
                if deque.validate_structure() {
                    *valid += 1;
                    let parent_index = |child: usize| {
                        (child + 1..length).find(|parent| {
                            let start = parent + 1 - spans[*parent];
                            start <= child
                        })
                    };
                    for child in 0..length {
                        let expected = parent_index(child).map(|parent| ids[parent]);
                        assert_eq!(deque.logical_parent(ids[child]), expected);
                    }
                    for parent in 0..length {
                        let expected: Vec<_> = (0..parent)
                            .rev()
                            .filter(|child| parent_index(*child) == Some(parent))
                            .map(|child| ids[child])
                            .collect();
                        assert_eq!(
                            deque.logical_children(ids[parent]),
                            expected,
                            "spans={spans:?}, parent_index={parent}"
                        );
                    }
                }
                spans.pop();
                return;
            }

            for span in 1..=index + 1 {
                spans.push(span);
                verify_shapes(length, index + 1, spans, valid);
                spans.pop();
            }
        }

        let mut valid_shapes = 0;
        verify_shapes(7, 0, &mut Vec::new(), &mut valid_shapes);
        assert!(valid_shapes > 0);
    }

    #[test]
    fn structural_validation_rejects_crossing_subtree_intervals() {
        let mut deque = Deque::new();
        deque.push_back_with_span("first", 1).unwrap();
        deque.push_back_with_span("second", 2).unwrap();
        deque.push_back_with_span("crossing", 2).unwrap();
        assert!(!deque.validate_structure());
    }
}
