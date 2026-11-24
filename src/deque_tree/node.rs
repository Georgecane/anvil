use super::{Deque, End};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId {
    index: u32,
    generation: u32,
}
impl NodeId {
    pub fn index(self) -> u32 {
        self.index
    }
    pub fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChildSide {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    RootAlreadyExists,
    InvalidNode(NodeId),
    ChildAlreadyExists { parent: NodeId, side: ChildSide },
    TopologySealed,
    CannotRemoveRoot,
    InvalidTopology,
    EmptyTopology,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearedDequeEntries {
    pub node: NodeId,
    pub entries: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DequeLinks {
    pub parent: Option<NodeId>,
    pub left: Option<NodeId>,
    pub right: Option<NodeId>,
}

pub struct Node<T> {
    deque: Box<Deque<T>>,
    parent: Option<NodeId>,
    left: Option<NodeId>,
    right: Option<NodeId>,
}
impl<T> Node<T> {
    pub fn deque(&self) -> &Deque<T> {
        &self.deque
    }
    pub fn deque_mut(&mut self) -> &mut Deque<T> {
        &mut self.deque
    }
    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }
    pub fn left(&self) -> Option<NodeId> {
        self.left
    }
    pub fn right(&self) -> Option<NodeId> {
        self.right
    }
    pub fn links(&self) -> DequeLinks {
        DequeLinks {
            parent: self.parent,
            left: self.left,
            right: self.right,
        }
    }
    pub(crate) fn deque_ptr(&mut self) -> *mut Deque<T> {
        self.deque.as_mut()
    }
}

struct Slot<T> {
    generation: u32,
    node: Option<Node<T>>,
}

/// Immutable ordered snapshot of the Deque Tree's parent-first DFS traversal.
#[derive(Debug, Clone, Default)]
pub struct RenderStack {
    nodes: Arc<[NodeId]>,
}
impl RenderStack {
    pub fn empty() -> Self {
        Self {
            nodes: Arc::from([]),
        }
    }
    pub(crate) fn from_nodes(nodes: Vec<NodeId>) -> Self {
        Self {
            nodes: nodes.into(),
        }
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn as_slice(&self) -> &[NodeId] {
        self.nodes.as_ref()
    }
    pub fn iter(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().copied()
    }
}

/// The one canonical hierarchy. Topology is built from the initial UI snapshot;
/// subsequent UI mutations move entries between existing Deques, not Nodes.
pub struct DequeTree<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    root: Option<NodeId>,
    len: usize,
    topology_sealed: bool,
}
impl<T> Default for DequeTree<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T> DequeTree<T> {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            root: None,
            len: 0,
            topology_sealed: false,
        }
    }
    /// Builds the fixed binary Deque topology from a level-order initial snapshot.
    /// Index 0 is the root; indices 1 and 2 are its left and right Deques, and so on.
    pub fn from_level_order(deques: Vec<Deque<T>>) -> Result<Self, TreeError> {
        if deques.is_empty() {
            return Err(TreeError::EmptyTopology);
        }
        let mut tree = Self::new();
        let mut ids = Vec::with_capacity(deques.len());
        for (index, deque) in deques.into_iter().enumerate() {
            if index == 0 {
                ids.push(tree.insert_root(deque)?);
                continue;
            }
            let parent_index = (index - 1) / 2;
            let side = if index % 2 == 1 {
                ChildSide::Left
            } else {
                ChildSide::Right
            };
            let parent = ids[parent_index];
            ids.push(tree.insert_child(parent, side, deque)?);
        }
        tree.seal_topology();
        Ok(tree)
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn topology_is_sealed(&self) -> bool {
        self.topology_sealed
    }
    pub fn seal_topology(&mut self) {
        self.topology_sealed = true;
    }

    fn alloc(&mut self, node: Node<T>) -> NodeId {
        self.len += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.node = Some(node);
            NodeId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 1,
                node: Some(node),
            });
            NodeId {
                index,
                generation: 1,
            }
        }
    }

    pub fn insert_root(&mut self, deque: Deque<T>) -> Result<NodeId, TreeError> {
        if self.topology_sealed {
            return Err(TreeError::TopologySealed);
        }
        if self.root.is_some() {
            return Err(TreeError::RootAlreadyExists);
        }
        let id = self.alloc(Node {
            deque: Box::new(deque),
            parent: None,
            left: None,
            right: None,
        });
        self.root = Some(id);
        Ok(id)
    }
    pub fn insert_left(&mut self, parent: NodeId, deque: Deque<T>) -> Result<NodeId, TreeError> {
        self.insert_child(parent, ChildSide::Left, deque)
    }
    pub fn insert_right(&mut self, parent: NodeId, deque: Deque<T>) -> Result<NodeId, TreeError> {
        self.insert_child(parent, ChildSide::Right, deque)
    }
    fn insert_child(
        &mut self,
        parent: NodeId,
        side: ChildSide,
        deque: Deque<T>,
    ) -> Result<NodeId, TreeError> {
        if self.topology_sealed {
            return Err(TreeError::TopologySealed);
        }
        let p = self.get(parent).ok_or(TreeError::InvalidNode(parent))?;
        if match side {
            ChildSide::Left => p.left.is_some(),
            ChildSide::Right => p.right.is_some(),
        } {
            return Err(TreeError::ChildAlreadyExists { parent, side });
        }
        let id = self.alloc(Node {
            deque: Box::new(deque),
            parent: Some(parent),
            left: None,
            right: None,
        });
        let p = self.get_mut(parent).expect("validated parent");
        match side {
            ChildSide::Left => p.left = Some(id),
            ChildSide::Right => p.right = Some(id),
        }
        Ok(id)
    }

    pub fn get(&self, id: NodeId) -> Option<&Node<T>> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.node.as_ref()
    }
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node<T>> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.node.as_mut()
    }

    pub fn preorder(&self) -> RenderStack {
        let mut out = Vec::with_capacity(self.len);
        let mut stack = Vec::new();
        if let Some(root) = self.root {
            stack.push(root);
        }
        while let Some(id) = stack.pop() {
            let Some(node) = self.get(id) else {
                continue;
            };
            if !node.deque.is_ghost() {
                out.push(id);
            }
            if let Some(right) = node.right {
                stack.push(right);
            }
            if let Some(left) = node.left {
                stack.push(left);
            }
        }
        RenderStack::from_nodes(out)
    }

    /// Clears a Deque node and all of its DequeTree descendants, preserving
    /// node identities and reconciling Ghost storage against same-depth siblings.
    pub fn clear_subtree_contents(
        &mut self,
        root: NodeId,
    ) -> Result<(Vec<ClearedDequeEntries>, bool), TreeError> {
        if self.get(root).is_none() {
            return Err(TreeError::InvalidNode(root));
        }
        let mut order = Vec::with_capacity(self.len);
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            let node = self.get(id).ok_or(TreeError::InvalidNode(id))?;
            order.push(id);
            if let Some(right) = node.right {
                pending.push(right);
            }
            if let Some(left) = node.left {
                pending.push(left);
            }
        }
        let mut removed = Vec::with_capacity(order.len());
        for id in &order {
            let entries = self
                .get_mut(*id)
                .ok_or(TreeError::InvalidNode(*id))?
                .deque_mut()
                .clear_for_reconcile();
            removed.push(ClearedDequeEntries { node: *id, entries });
        }
        let mut render_stack_changed = false;
        for id in order {
            render_stack_changed |= self.reconcile_ghosts_for(id);
        }
        Ok((removed, render_stack_changed))
    }

    pub fn transfer_subtree(
        &mut self,
        source: NodeId,
        target: NodeId,
        anchor: u64,
        target_end: End,
    ) -> Result<usize, TreeError> {
        if self.get(source).is_none() {
            return Err(TreeError::InvalidNode(source));
        }
        if self.get(target).is_none() {
            return Err(TreeError::InvalidNode(target));
        }
        if source == target {
            return Err(TreeError::InvalidTopology);
        }
        let (source_slot, target_slot) = if source.index < target.index {
            let (left, right) = self.slots.split_at_mut(target.index as usize);
            (&mut left[source.index as usize], &mut right[0])
        } else {
            let (left, right) = self.slots.split_at_mut(source.index as usize);
            (&mut right[0], &mut left[target.index as usize])
        };
        if source_slot.generation != source.generation
            || target_slot.generation != target.generation
        {
            return Err(TreeError::InvalidTopology);
        }
        let (Some(source_node), Some(target_node)) =
            (source_slot.node.as_mut(), target_slot.node.as_mut())
        else {
            return Err(TreeError::InvalidTopology);
        };
        source_node
            .deque
            .transfer_subtree_to(&mut target_node.deque, anchor, target_end)
            .ok_or(TreeError::InvalidTopology)
    }

    /// Reconciles only the changed Deque and its same-depth sibling.
    /// This keeps ordinary mutations local instead of rescanning the DequeTree.
    pub fn reconcile_ghosts_for(&mut self, id: NodeId) -> bool {
        let Some(node) = self.get(id) else {
            return false;
        };
        if !node.deque.is_empty() || node.deque.is_ghost() {
            return false;
        }
        let sibling = node
            .parent
            .and_then(|parent| self.get(parent))
            .and_then(|parent| {
                if parent.left == Some(id) {
                    parent.right
                } else {
                    parent.left
                }
            });
        if let Some(sibling_id) = sibling {
            let sibling_active = self.get(sibling_id).is_some_and(|s| !s.deque.is_empty());
            if sibling_active {
                let (empty_slot, active_slot) = if id.index() < sibling_id.index() {
                    let (left, right) = self.slots.split_at_mut(sibling_id.index() as usize);
                    (&mut left[id.index() as usize], &mut right[0])
                } else {
                    let (left, right) = self.slots.split_at_mut(id.index() as usize);
                    (&mut right[0], &mut left[sibling_id.index() as usize])
                };
                if empty_slot.generation == id.generation()
                    && active_slot.generation == sibling_id.generation()
                    && let (Some(empty), Some(active)) =
                        (empty_slot.node.as_mut(), active_slot.node.as_mut())
                {
                    return empty.deque.transfer_empty_storage_to(&mut active.deque);
                }
                return false;
            }
            let mut changed = false;
            if let Some(node) = self.get_mut(id)
                && !node.deque.is_ghost()
            {
                node.deque.mark_ghost();
                changed = true;
            }
            if let Some(sibling) = self.get_mut(sibling_id)
                && !sibling.deque.is_ghost()
            {
                sibling.deque.mark_ghost();
                changed = true;
            }
            changed
        } else if let Some(node) = self.get_mut(id) {
            node.deque.mark_ghost();
            true
        } else {
            false
        }
    }

    pub fn for_each_node_mut(&mut self, mut f: impl FnMut(NodeId, &mut Node<T>)) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(node) = slot.node.as_mut() {
                f(
                    NodeId {
                        index: index as u32,
                        generation: slot.generation,
                    },
                    node,
                );
            }
        }
    }

    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node<T>)> {
        self.slots.iter().enumerate().filter_map(|(i, slot)| {
            slot.node.as_ref().map(|node| {
                (
                    NodeId {
                        index: i as u32,
                        generation: slot.generation,
                    },
                    node,
                )
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preorder_is_parent_first_depth_first_not_breadth_first() {
        let mut tree = DequeTree::<()>::new();
        let active = || {
            let mut deque = Deque::new();
            deque.push_back(());
            deque
        };
        let root = tree.insert_root(active()).unwrap();
        let left = tree.insert_left(root, active()).unwrap();
        let right = tree.insert_right(root, active()).unwrap();
        let left_left = tree.insert_left(left, active()).unwrap();
        let left_right = tree.insert_right(left, active()).unwrap();
        let right_left = tree.insert_left(right, active()).unwrap();
        let right_right = tree.insert_right(right, active()).unwrap();
        assert_eq!(
            tree.preorder().as_slice(),
            &[
                root,
                left,
                left_left,
                left_right,
                right,
                right_left,
                right_right
            ]
        );
    }

    #[test]
    fn topology_can_be_sealed_after_initial_construction() {
        let mut tree = DequeTree::<u8>::new();
        let root = tree.insert_root(Deque::new()).unwrap();
        tree.insert_left(root, Deque::new()).unwrap();
        tree.seal_topology();
        assert_eq!(
            tree.insert_right(root, Deque::new()),
            Err(TreeError::TopologySealed)
        );
    }

    #[test]
    fn ghost_nodes_remain_in_topology_and_release_empty_storage() {
        let mut tree = DequeTree::<&str>::new();
        let root = tree.insert_root(Deque::new()).unwrap();
        let mut left_deque = Deque::with_capacity(128);
        let left_entry = left_deque.push_back("left");
        let left = tree.insert_left(root, left_deque).unwrap();
        let mut right_deque = Deque::new();
        right_deque.push_back("right");
        let right = tree.insert_right(root, right_deque).unwrap();
        tree.get_mut(left)
            .unwrap()
            .deque_mut()
            .drain_subtree(left_entry);
        tree.reconcile_ghosts_for(left);
        assert!(tree.get(left).unwrap().deque().is_ghost());
        assert!(!tree.get(right).unwrap().deque().is_ghost());
        assert!(tree.get(right).unwrap().deque().capacity() >= 128);
        assert_eq!(tree.preorder().as_slice(), &[right]);
    }

    #[test]
    fn when_both_same_depth_deques_are_empty_both_become_ghosts() {
        let mut tree = DequeTree::<u8>::new();
        let root = tree.insert_root(Deque::new()).unwrap();
        let mut left_deque = Deque::new();
        let left_entry = left_deque.push_back(1);
        let mut right_deque = Deque::new();
        let right_entry = right_deque.push_back(2);
        let left = tree.insert_left(root, left_deque).unwrap();
        let right = tree.insert_right(root, right_deque).unwrap();
        tree.get_mut(left)
            .unwrap()
            .deque_mut()
            .drain_subtree(left_entry);
        tree.get_mut(right)
            .unwrap()
            .deque_mut()
            .drain_subtree(right_entry);
        assert!(tree.reconcile_ghosts_for(left));
        assert!(tree.get(left).unwrap().deque().is_ghost());
        assert!(tree.get(right).unwrap().deque().is_ghost());
        assert_eq!(tree.get(left).unwrap().deque().capacity(), 0);
        assert_eq!(tree.get(right).unwrap().deque().capacity(), 0);
    }
}
