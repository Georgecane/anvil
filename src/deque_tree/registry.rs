use super::{Deque, DequeLinks, DequeTree, NodeId, RenderStack};
use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;

pub enum DequeMapField<T> {
    DequePointer(NonNull<Deque<T>>),
    IdentityHash(u64),
    DequeId(NodeId),
}
pub type DequeMapValue<T> = [DequeMapField<T>; 3];

/// Non-owning, live index linked to the canonical DequeTree.
pub struct DequeRegistry<T> {
    entries: HashMap<NodeId, DequeMapValue<T>>,
    links: HashMap<NodeId, DequeLinks>,
    render_stack: RenderStack,
    pending_render_changes: HashSet<NodeId>,
}
pub struct RenderFrame {
    changed: Vec<NodeId>,
    stack: RenderStack,
}
impl RenderFrame {
    pub fn changed(&self) -> &[NodeId] {
        &self.changed
    }
    pub fn stack(&self) -> &RenderStack {
        &self.stack
    }
    pub fn visit<T>(&self, tree: &DequeTree<T>, mut f: impl FnMut(NodeId, &Deque<T>)) -> usize {
        let mut n = 0;
        for id in self.stack.iter() {
            if let Some(node) = tree.get(id)
                && !node.deque().is_ghost()
            {
                f(id, node.deque());
                n += 1;
            }
        }
        n
    }
}
impl<T> Default for DequeRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T> DequeRegistry<T> {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            links: HashMap::new(),
            render_stack: RenderStack::empty(),
            pending_render_changes: HashSet::new(),
        }
    }
    pub fn build(tree: &mut DequeTree<T>) -> Self {
        let mut map = Self::new();
        tree.for_each_node_mut(|id, node| {
            let ptr = NonNull::new(node.deque_ptr()).expect("live node has a Deque allocation");
            map.entries.insert(
                id,
                [
                    DequeMapField::DequePointer(ptr),
                    DequeMapField::IdentityHash(node.deque().identity_hash()),
                    DequeMapField::DequeId(id),
                ],
            );
        });
        for (id, node) in tree.nodes() {
            map.links.insert(id, node.links());
        }
        map.render_stack = tree.preorder();
        map
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, id: NodeId) -> Option<&DequeMapValue<T>> {
        self.entries.get(&id)
    }
    /// Cached non-owning parent/left/right links mirroring the canonical tree topology.
    pub fn links(&self, id: NodeId) -> Option<DequeLinks> {
        self.links.get(&id).copied()
    }
    pub fn link_count(&self) -> usize {
        self.links.len()
    }
    pub fn render_stack(&self) -> &RenderStack {
        &self.render_stack
    }
    /// Compares only the supplied Deque IDs against cached HashMap entries.
    /// Each map value retains the exact three-field [pointer, identity hash, id] contract.
    pub fn detect_changes_for(
        &mut self,
        tree: &mut DequeTree<T>,
        candidates: impl IntoIterator<Item = NodeId>,
    ) -> Vec<NodeId> {
        let mut changed = Vec::new();
        let mut seen = HashSet::new();
        for id in candidates {
            if !seen.insert(id) {
                continue;
            }
            let Some(fields) = self.entries.get_mut(&id) else {
                continue;
            };
            let Some(node) = tree.get_mut(id) else {
                continue;
            };
            let hash = node.deque().identity_hash();
            let previous = match fields[1] {
                DequeMapField::IdentityHash(value) => value,
                _ => unreachable!(),
            };
            fields[0] =
                DequeMapField::DequePointer(NonNull::new(node.deque_ptr()).expect("live Deque"));
            if hash != previous {
                fields[1] = DequeMapField::IdentityHash(hash);
                changed.push(id);
                self.pending_render_changes.insert(id);
            }
        }
        changed.sort_unstable();
        changed
    }

    /// Full fallback detector. Normal UI mutations use targeted HashMap lookup.
    pub fn detect_changes(&mut self, tree: &mut DequeTree<T>) -> Vec<NodeId> {
        self.synchronize(tree);
        let mut changed: Vec<_> = self.pending_render_changes.drain().collect();
        changed.sort_unstable();
        changed
    }

    pub fn synchronize(&mut self, tree: &mut DequeTree<T>) {
        self.entries.retain(|id, _| tree.get(*id).is_some());
        self.links.retain(|id, _| tree.get(*id).is_some());
        self.pending_render_changes
            .retain(|id| tree.get(*id).is_some());
        for (id, node) in tree.nodes() {
            self.links.insert(id, node.links());
        }
        tree.for_each_node_mut(|id, node| {
            let ptr = NonNull::new(node.deque_ptr()).expect("live Deque");
            let hash = node.deque().identity_hash();
            if let Some(fields) = self.entries.get_mut(&id) {
                fields[0] = DequeMapField::DequePointer(ptr);
                let previous_hash = match fields[1] {
                    DequeMapField::IdentityHash(value) => value,
                    _ => unreachable!("registry field 1 always stores the identity hash"),
                };
                if previous_hash != hash {
                    fields[1] = DequeMapField::IdentityHash(hash);
                    self.pending_render_changes.insert(id);
                }
            } else {
                self.entries.insert(
                    id,
                    [
                        DequeMapField::DequePointer(ptr),
                        DequeMapField::IdentityHash(hash),
                        DequeMapField::DequeId(id),
                    ],
                );
                self.pending_render_changes.insert(id);
            }
        });
        self.render_stack = tree.preorder();
    }
    pub fn refresh_render_stack(&mut self, tree: &DequeTree<T>) {
        let root = self
            .links
            .iter()
            .find_map(|(id, links)| links.parent.is_none().then_some(*id));
        let mut nodes = Vec::with_capacity(self.links.len());
        let mut pending = Vec::with_capacity(self.links.len());
        if let Some(root) = root {
            pending.push(root);
        }
        while let Some(id) = pending.pop() {
            let Some(links) = self.links.get(&id) else {
                continue;
            };
            if tree.get(id).is_some_and(|node| !node.deque().is_ghost()) {
                nodes.push(id);
            }
            if let Some(right) = links.right {
                pending.push(right);
            }
            if let Some(left) = links.left {
                pending.push(left);
            }
        }
        self.render_stack = RenderStack::from_nodes(nodes);
    }
    /// Builds a frame from mutations already detected through targeted HashMap entries.
    /// This is the normal UiModel path and does not scan unrelated Deques.
    pub fn prepare_render_pending(&mut self) -> RenderFrame {
        let mut changed: Vec<_> = self.pending_render_changes.drain().collect();
        changed.sort_unstable();
        RenderFrame {
            changed,
            stack: self.render_stack.clone(),
        }
    }

    /// Full fallback path for standalone consumers that mutate a DequeTree directly.
    pub fn prepare_render(&mut self, tree: &mut DequeTree<T>) -> RenderFrame {
        let changed = self.detect_changes(tree);
        RenderFrame {
            changed,
            stack: self.render_stack.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keeps_stable_heap_pointer_and_exact_three_fields() {
        let mut tree = DequeTree::new();
        let root = tree.insert_root(Deque::<u8>::new()).unwrap();
        tree.insert_left(root, Deque::new()).unwrap();
        let mut registry = DequeRegistry::build(&mut tree);
        let before = match registry.get(root).unwrap()[0] {
            DequeMapField::DequePointer(pointer) => pointer,
            _ => unreachable!(),
        };
        tree.get_mut(root).unwrap().deque_mut().push_back(7);
        assert_eq!(registry.detect_changes(&mut tree), vec![root]);
        let after = match registry.get(root).unwrap()[0] {
            DequeMapField::DequePointer(pointer) => pointer,
            _ => unreachable!(),
        };
        assert_eq!(before, after);
        assert!(matches!(
            registry.get(root).unwrap()[1],
            DequeMapField::IdentityHash(_)
        ));
        assert!(matches!(registry.get(root).unwrap()[2], DequeMapField::DequeId(id) if id == root));
    }

    #[test]
    fn targeted_hash_detection_touches_only_changed_deques_and_preserves_render_notice() {
        let mut tree = DequeTree::new();
        let root = tree.insert_root(Deque::<u8>::new()).unwrap();
        let left = tree.insert_left(root, Deque::new()).unwrap();
        let right = tree.insert_right(root, Deque::new()).unwrap();
        let mut registry = DequeRegistry::build(&mut tree);

        tree.get_mut(left).unwrap().deque_mut().push_back(1);
        assert_eq!(registry.detect_changes_for(&mut tree, [left]), vec![left]);

        // The render consumer still receives the structural change, even though
        // the style consumer already refreshed the cached identity hash.
        let frame = registry.prepare_render_pending();
        assert_eq!(frame.changed(), &[left]);
        assert_eq!(registry.links(right).unwrap().parent, Some(root));
    }

    #[test]
    fn synchronize_refreshes_existing_hashes_and_reports_new_nodes() {
        let mut tree = DequeTree::new();
        let root = tree.insert_root(Deque::<u8>::new()).unwrap();
        let mut registry = DequeRegistry::build(&mut tree);

        tree.get_mut(root).unwrap().deque_mut().push_back(1);
        let child = tree.insert_left(root, Deque::new()).unwrap();
        tree.get_mut(child).unwrap().deque_mut().push_back(2);
        registry.synchronize(&mut tree);

        let cached_root_hash = match registry.get(root).unwrap()[1] {
            DequeMapField::IdentityHash(hash) => hash,
            _ => unreachable!(),
        };
        assert_eq!(
            cached_root_hash,
            tree.get(root).unwrap().deque().identity_hash()
        );
        let cached_child_hash = match registry.get(child).unwrap()[1] {
            DequeMapField::IdentityHash(hash) => hash,
            _ => unreachable!(),
        };
        assert_eq!(
            cached_child_hash,
            tree.get(child).unwrap().deque().identity_hash()
        );

        let frame = registry.prepare_render_pending();
        assert!(frame.changed().contains(&root));
        assert!(frame.changed().contains(&child));
        assert_eq!(frame.stack().as_slice(), &[root, child]);
    }

    #[test]
    fn registry_links_mirror_the_binary_deque_topology() {
        let mut tree = DequeTree::new();
        let root = tree.insert_root(Deque::<u8>::new()).unwrap();
        let left = tree.insert_left(root, Deque::new()).unwrap();
        let right = tree.insert_right(root, Deque::new()).unwrap();
        let registry = DequeRegistry::build(&mut tree);
        assert_eq!(registry.link_count(), 3);
        assert_eq!(registry.links(root).unwrap().left, Some(left));
        assert_eq!(registry.links(root).unwrap().right, Some(right));
        assert_eq!(registry.links(left).unwrap().parent, Some(root));
    }

    #[test]
    fn full_render_fallback_refreshes_stack_when_a_ghost_deque_is_revived() {
        let mut tree = DequeTree::new();
        let root = tree.insert_root(Deque::<u8>::new()).unwrap();
        let child = tree.insert_left(root, Deque::new()).unwrap();
        tree.get_mut(root).unwrap().deque_mut().push_back(1);
        let mut registry = DequeRegistry::build(&mut tree);
        assert_eq!(registry.render_stack().as_slice(), &[root]);

        tree.get_mut(child).unwrap().deque_mut().push_back(2);
        let frame = registry.prepare_render(&mut tree);

        assert_eq!(frame.changed(), &[child]);
        assert_eq!(frame.stack().as_slice(), &[root, child]);
    }

    #[test]
    fn child_local_mutation_does_not_change_parent_hash() {
        let mut tree = DequeTree::new();
        let root = tree.insert_root(Deque::<u8>::new()).unwrap();
        let child = tree.insert_left(root, Deque::new()).unwrap();
        let mut registry = DequeRegistry::build(&mut tree);
        let root_hash = tree.get(root).unwrap().deque().identity_hash();
        tree.get_mut(child).unwrap().deque_mut().push_back(3);
        assert_eq!(registry.detect_changes(&mut tree), vec![child]);
        assert_eq!(tree.get(root).unwrap().deque().identity_hash(), root_hash);
    }
}
