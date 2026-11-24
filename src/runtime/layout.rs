use super::ui::{ElementHandle, LogicalProjection};
use std::collections::HashMap;

/// A temporary Taffy node index mapped to a stable canonical Anvil element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutNodeId(taffy::NodeId);

/// Read-only Taffy traversal over the canonical Deque Tree.
/// No TaffyTree or parallel parent/child hierarchy is constructed.
pub struct TaffyTreeProjection<'a> {
    projection: LogicalProjection<'a>,
    handles: Vec<ElementHandle>,
    node_ids: HashMap<ElementHandle, taffy::NodeId>,
}

impl<'a> TaffyTreeProjection<'a> {
    /// Create an ephemeral index for the current canonical projection.
    pub fn new(projection: LogicalProjection<'a>) -> Self {
        let handles = projection.handles();
        let node_ids = handles
            .iter()
            .copied()
            .enumerate()
            .map(|(index, handle)| (handle, taffy::NodeId::from(index)))
            .collect();

        Self {
            projection,
            handles,
            node_ids,
        }
    }

    /// Resolve a stable Anvil handle to this projection's Taffy ID.
    pub fn node_id(&self, handle: ElementHandle) -> Option<LayoutNodeId> {
        self.node_ids.get(&handle).copied().map(LayoutNodeId)
    }

    /// Resolve a projection-local Taffy ID to its stable Anvil handle.
    pub fn element_handle(&self, node_id: LayoutNodeId) -> Option<ElementHandle> {
        self.handles.get(usize::from(node_id.0)).copied()
    }

    /// Read the canonical projection used by this adapter.
    pub fn projection(&self) -> &LogicalProjection<'a> {
        &self.projection
    }

    fn handle_for_taffy_id(&self, node_id: taffy::NodeId) -> ElementHandle {
        self.handles[usize::from(node_id)]
    }
}

impl taffy::TraversePartialTree for TaffyTreeProjection<'_> {
    type ChildIter<'a>
        = std::vec::IntoIter<taffy::NodeId>
    where
        Self: 'a;

    fn child_ids(&self, parent_node_id: taffy::NodeId) -> Self::ChildIter<'_> {
        self.projection
            .children_of(self.handle_for_taffy_id(parent_node_id))
            .into_iter()
            .filter_map(|handle| self.node_ids.get(&handle).copied())
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn child_count(&self, parent_node_id: taffy::NodeId) -> usize {
        self.projection
            .children_of(self.handle_for_taffy_id(parent_node_id))
            .len()
    }

    fn get_child_id(&self, parent_node_id: taffy::NodeId, child_index: usize) -> taffy::NodeId {
        let parent = self.handle_for_taffy_id(parent_node_id);
        let children = self.projection.children_of(parent);
        self.node_ids[&children[child_index]]
    }
}

impl taffy::TraverseTree for TaffyTreeProjection<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deque_tree::End;
    use crate::runtime::ui::UiModel;

    #[test]
    fn taffy_traversal_reads_children_from_the_canonical_projection() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let main = ui.create_element(root, "main", End::Back).unwrap();
        let first = ui.create_element(root, "button", End::Front).unwrap();
        let second = ui.create_element(root, "span", End::Front).unwrap();

        let adapter = TaffyTreeProjection::new(ui.projection());
        let main_id = adapter.node_id(main).unwrap().0;

        assert_eq!(
            adapter.element_handle(adapter.node_id(first).unwrap()),
            Some(first)
        );
        assert_eq!(
            adapter.element_handle(adapter.node_id(second).unwrap()),
            Some(second)
        );
        assert_eq!(adapter.child_count(main_id), 2);
        let expected = adapter
            .projection()
            .children_of(main)
            .into_iter()
            .map(|handle| adapter.node_id(handle).unwrap().0)
            .collect::<Vec<_>>();
        assert_eq!(adapter.child_ids(main_id).collect::<Vec<_>>(), expected);
        assert_eq!(adapter.get_child_id(main_id, 0), expected[0]);
    }

    #[test]
    fn taffy_projection_does_not_keep_stale_structure_after_a_commit() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let main = ui.create_element(root, "main", End::Back).unwrap();
        let child = ui.create_element(root, "button", End::Front).unwrap();

        let before = TaffyTreeProjection::new(ui.projection());
        assert_eq!(before.child_count(before.node_id(main).unwrap().0), 1);
        drop(before);

        ui.remove_subtree(child).unwrap();

        let after = TaffyTreeProjection::new(ui.projection());
        assert_eq!(after.child_count(after.node_id(main).unwrap().0), 0);
        assert!(after.node_id(child).is_none());
    }
}
