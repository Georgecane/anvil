#[cfg(test)]
use super::ui::InitialElement;
use super::ui::{DequeHandle, ElementHandle, InitialDeque, UiElement, UiError, UiModel};
use crate::deque_tree::{ChildSide, End};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameworkDequeId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameworkElementId(pub u64);

#[derive(Debug, Clone)]
pub enum FrameworkOperation {
    CreateRootDeque {
        id: FrameworkDequeId,
    },
    CreateChildDeque {
        id: FrameworkDequeId,
        parent: FrameworkDequeId,
        side: ChildSide,
    },
    CreateElement {
        id: FrameworkElementId,
        deque: FrameworkDequeId,
        tag: String,
        end: End,
    },
    CreateText {
        id: FrameworkElementId,
        deque: FrameworkDequeId,
        text: String,
        end: End,
    },
    SetText {
        id: FrameworkElementId,
        text: String,
    },
    SetProperty {
        id: FrameworkElementId,
        key: String,
        value: String,
    },
    RemoveProperty {
        id: FrameworkElementId,
        key: String,
    },
    SetStyle {
        id: FrameworkElementId,
        key: String,
        value: String,
    },
    RemoveStyle {
        id: FrameworkElementId,
        key: String,
    },
    RemoveSubtree {
        anchor: FrameworkElementId,
    },
    MoveSubtree {
        anchor: FrameworkElementId,
        target: FrameworkDequeId,
        target_end: End,
    },
}
#[derive(Debug, Default, Clone)]
pub struct FrameworkCommit {
    operations: Vec<FrameworkOperation>,
}
impl FrameworkCommit {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, op: FrameworkOperation) {
        self.operations.push(op);
    }
    pub fn operations(&self) -> &[FrameworkOperation] {
        &self.operations
    }
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

#[derive(Debug)]
pub enum FrameworkError {
    DuplicateDeque(FrameworkDequeId),
    UnknownDeque(FrameworkDequeId),
    DuplicateElement(FrameworkElementId),
    UnknownElement(FrameworkElementId),
    RootAlreadyBound,
    NotText(FrameworkElementId),
    Ui(UiError),
    InvalidRange,
}
impl From<UiError> for FrameworkError {
    fn from(value: UiError) -> Self {
        Self::Ui(value)
    }
}
/// A framework performs one initial read and exports the already-partitioned Deque groups.
/// After bootstrap, it must emit incremental commits rather than another full-tree snapshot.
pub trait InitialSnapshotSource {
    type Error;
    fn capture_initial_deques(&mut self) -> Result<Vec<InitialDeque>, Self::Error>;
}

#[derive(Debug)]
pub enum BootstrapError<E> {
    Source(E),
    InvalidSnapshot(UiError),
}

pub fn bootstrap_from_snapshot<S: InitialSnapshotSource>(
    source: &mut S,
) -> Result<UiModel, BootstrapError<S::Error>> {
    let groups = source
        .capture_initial_deques()
        .map_err(BootstrapError::Source)?;
    UiModel::from_initial_groups(groups).map_err(BootstrapError::InvalidSnapshot)
}

pub trait FrameworkAdapter {
    fn apply_commit(
        &mut self,
        commit: FrameworkCommit,
        ui: &mut UiModel,
    ) -> Result<(), FrameworkError>;
}

/// Thin framework bridge: stable IDs only, no shadow tree or element ownership.
pub struct CommitAdapter {
    deques: HashMap<FrameworkDequeId, DequeHandle>,
    reverse_deques: HashMap<DequeHandle, FrameworkDequeId>,
    elements: HashMap<FrameworkElementId, ElementHandle>,
    reverse_elements: HashMap<ElementHandle, FrameworkElementId>,
}
impl Default for CommitAdapter {
    fn default() -> Self {
        Self::new()
    }
}
impl CommitAdapter {
    pub fn new() -> Self {
        Self {
            deques: HashMap::new(),
            reverse_deques: HashMap::new(),
            elements: HashMap::new(),
            reverse_elements: HashMap::new(),
        }
    }
    pub fn native_element(&self, id: FrameworkElementId) -> Option<ElementHandle> {
        self.elements.get(&id).copied()
    }
    pub fn native_deque(&self, id: FrameworkDequeId) -> Option<DequeHandle> {
        self.deques.get(&id).copied()
    }
    pub(crate) fn synchronize_with_ui(&mut self, ui: &UiModel) {
        self.deques
            .retain(|_, handle| ui.deque_id(*handle).is_some());
        self.reverse_deques
            .retain(|handle, id| self.deques.get(id) == Some(handle));
        self.elements
            .retain(|_, handle| ui.element(*handle).is_some());
        self.reverse_elements
            .retain(|handle, id| self.elements.get(id) == Some(handle));
    }

    fn remove_element_mapping(&mut self, id: FrameworkElementId) {
        if let Some(handle) = self.elements.remove(&id) {
            self.reverse_elements.remove(&handle);
        }
    }

    pub(crate) fn remove_native_elements(&mut self, handles: &[ElementHandle]) {
        for handle in handles {
            if let Some(id) = self.reverse_elements.remove(handle) {
                self.elements.remove(&id);
            }
        }
    }

    fn apply_create_batch(
        &mut self,
        deque: FrameworkDequeId,
        end: End,
        first: (FrameworkElementId, UiElement),
        operations: &mut std::iter::Peekable<std::vec::IntoIter<FrameworkOperation>>,
        ui: &mut UiModel,
    ) -> Result<(), FrameworkError> {
        let mut batch = vec![first];
        loop {
            let next = match operations.peek() {
                Some(FrameworkOperation::CreateElement {
                    id,
                    deque: next_deque,
                    tag,
                    end: next_end,
                }) if *next_deque == deque && *next_end == end => {
                    Some((*id, UiElement::new(tag.clone())))
                }
                Some(FrameworkOperation::CreateText {
                    id,
                    deque: next_deque,
                    text,
                    end: next_end,
                }) if *next_deque == deque && *next_end == end => {
                    Some((*id, UiElement::text(text.clone())))
                }
                _ => None,
            };
            let Some(next) = next else {
                break;
            };
            operations.next();
            batch.push(next);
        }

        let native_deque = self.deques[&deque];
        let elements = batch.iter().map(|(_, element)| element.clone()).collect();
        let handles = ui.create_elements_batch(native_deque, elements, end)?;
        for ((id, _), handle) in batch.into_iter().zip(handles) {
            self.elements.insert(id, handle);
            self.reverse_elements.insert(handle, id);
        }
        Ok(())
    }

    fn prune_stale_references(&mut self, commit: &FrameworkCommit, ui: &UiModel) {
        for operation in commit.operations() {
            let referenced = match operation {
                FrameworkOperation::CreateElement { id, .. }
                | FrameworkOperation::CreateText { id, .. }
                | FrameworkOperation::SetText { id, .. }
                | FrameworkOperation::SetProperty { id, .. }
                | FrameworkOperation::RemoveProperty { id, .. }
                | FrameworkOperation::SetStyle { id, .. }
                | FrameworkOperation::RemoveStyle { id, .. } => Some(*id),
                FrameworkOperation::RemoveSubtree { anchor }
                | FrameworkOperation::MoveSubtree { anchor, .. } => Some(*anchor),
                FrameworkOperation::CreateRootDeque { .. }
                | FrameworkOperation::CreateChildDeque { .. } => None,
            };
            if let Some(id) = referenced
                && self
                    .elements
                    .get(&id)
                    .is_some_and(|handle| ui.element(*handle).is_none())
            {
                self.remove_element_mapping(id);
            }
        }
    }
    pub fn bind_existing_root(
        &mut self,
        id: FrameworkDequeId,
        ui: &UiModel,
    ) -> Result<(), FrameworkError> {
        if self.deques.contains_key(&id) {
            return Err(FrameworkError::DuplicateDeque(id));
        }
        let root = ui.root_handle().ok_or(FrameworkError::RootAlreadyBound)?;
        if self.reverse_deques.contains_key(&root) {
            return Err(FrameworkError::RootAlreadyBound);
        }
        self.deques.insert(id, root);
        self.reverse_deques.insert(root, id);
        Ok(())
    }
    fn validate_simple(
        &self,
        commit: &FrameworkCommit,
        ui: &UiModel,
    ) -> Result<(), FrameworkError> {
        let mut has_root = ui.root_handle().is_some();
        let mut new_deques = HashMap::<FrameworkDequeId, (bool, bool)>::new();
        let mut existing_sides = HashMap::<FrameworkDequeId, (bool, bool)>::new();
        let mut new_elements = HashMap::<FrameworkElementId, bool>::new();

        for operation in commit.operations() {
            match operation {
                FrameworkOperation::CreateRootDeque { id } => {
                    if has_root || ui.topology_is_sealed() {
                        return Err(FrameworkError::RootAlreadyBound);
                    }
                    if self.deques.contains_key(id) || new_deques.contains_key(id) {
                        return Err(FrameworkError::DuplicateDeque(*id));
                    }
                    has_root = true;
                    new_deques.insert(*id, (false, false));
                }
                FrameworkOperation::CreateChildDeque { id, parent, side } => {
                    if ui.topology_is_sealed() {
                        return Err(FrameworkError::Ui(UiError::TopologySealed));
                    }
                    if self.deques.contains_key(id) || new_deques.contains_key(id) {
                        return Err(FrameworkError::DuplicateDeque(*id));
                    }
                    let sides = if let Some(sides) = new_deques.get_mut(parent) {
                        sides
                    } else {
                        if !existing_sides.contains_key(parent) {
                            let native_parent = self
                                .deques
                                .get(parent)
                                .copied()
                                .ok_or(FrameworkError::UnknownDeque(*parent))?;
                            let parent_node_id = ui
                                .deque_id(native_parent)
                                .ok_or(FrameworkError::UnknownDeque(*parent))?;
                            let parent_node = ui
                                .tree()
                                .get(parent_node_id)
                                .ok_or(FrameworkError::UnknownDeque(*parent))?;
                            existing_sides.insert(
                                *parent,
                                (parent_node.left().is_some(), parent_node.right().is_some()),
                            );
                        }
                        existing_sides
                            .get_mut(parent)
                            .expect("existing Deque side state inserted above")
                    };
                    let occupied = match side {
                        ChildSide::Left => &mut sides.0,
                        ChildSide::Right => &mut sides.1,
                    };
                    if *occupied {
                        return Err(FrameworkError::Ui(UiError::InvalidSide));
                    }
                    *occupied = true;
                    new_deques.insert(*id, (false, false));
                }
                FrameworkOperation::CreateElement { id, deque, .. }
                | FrameworkOperation::CreateText { id, deque, .. } => {
                    if !self.deques.contains_key(deque) && !new_deques.contains_key(deque) {
                        return Err(FrameworkError::UnknownDeque(*deque));
                    }
                    if self.elements.contains_key(id) || new_elements.contains_key(id) {
                        return Err(FrameworkError::DuplicateElement(*id));
                    }
                    new_elements.insert(
                        *id,
                        matches!(operation, FrameworkOperation::CreateText { .. }),
                    );
                }
                FrameworkOperation::SetText { id, .. } => {
                    let is_text = new_elements.get(id).copied().or_else(|| {
                        self.elements
                            .get(id)
                            .and_then(|handle| ui.element(*handle))
                            .map(|element| element.is_text())
                    });
                    match is_text {
                        Some(true) => {}
                        Some(false) => return Err(FrameworkError::NotText(*id)),
                        None => return Err(FrameworkError::UnknownElement(*id)),
                    }
                }
                FrameworkOperation::SetProperty { id, .. }
                | FrameworkOperation::RemoveProperty { id, .. }
                | FrameworkOperation::SetStyle { id, .. }
                | FrameworkOperation::RemoveStyle { id, .. } => {
                    if !new_elements.contains_key(id) && !self.elements.contains_key(id) {
                        return Err(FrameworkError::UnknownElement(*id));
                    }
                }
                FrameworkOperation::RemoveSubtree { .. }
                | FrameworkOperation::MoveSubtree { .. } => {
                    unreachable!("structural operations use the full commit validator")
                }
            }
        }
        Ok(())
    }

    fn validate_structural_only_batch(
        &self,
        commit: &FrameworkCommit,
        ui: &UiModel,
    ) -> Result<bool, FrameworkError> {
        if commit.operations().is_empty() {
            return Ok(false);
        }
        let mut removals = Vec::new();
        let mut moves = Vec::new();
        for operation in commit.operations() {
            match operation {
                FrameworkOperation::RemoveSubtree { anchor } => {
                    removals.push(
                        self.elements
                            .get(anchor)
                            .copied()
                            .ok_or(FrameworkError::UnknownElement(*anchor))?,
                    );
                }
                FrameworkOperation::MoveSubtree { anchor, target, .. } => {
                    let handle = self
                        .elements
                        .get(anchor)
                        .copied()
                        .ok_or(FrameworkError::UnknownElement(*anchor))?;
                    if !self.deques.contains_key(target) {
                        return Err(FrameworkError::UnknownDeque(*target));
                    }
                    moves.push((handle, *self.deques.get(target).expect("target checked")));
                }
                _ => return Ok(false),
            }
        }
        match ui.validate_structural_batch(&removals, &moves) {
            Ok(()) => Ok(true),
            Err(UiError::InvalidRange) => Ok(false),
            Err(error) => Err(FrameworkError::Ui(error)),
        }
    }

    fn apply_move_batch(
        &mut self,
        first: FrameworkElementId,
        target: FrameworkDequeId,
        target_end: End,
        operations: &mut std::iter::Peekable<std::vec::IntoIter<FrameworkOperation>>,
        ui: &mut UiModel,
    ) -> Result<(), FrameworkError> {
        let first_handle = *self
            .elements
            .get(&first)
            .ok_or(FrameworkError::UnknownElement(first))?;
        let source = ui
            .deque_handle_for_element(first_handle)
            .ok_or(FrameworkError::UnknownElement(first))?;
        let target_handle = *self
            .deques
            .get(&target)
            .ok_or(FrameworkError::UnknownDeque(target))?;
        let mut handles = vec![first_handle];
        while let Some(FrameworkOperation::MoveSubtree {
            anchor,
            target: next_target,
            target_end: next_end,
        }) = operations.peek()
        {
            if *next_target != target || *next_end != target_end {
                break;
            }
            let Some(handle) = self.elements.get(anchor).copied() else {
                break;
            };
            if ui.deque_handle_for_element(handle) != Some(source) {
                break;
            }
            handles.push(handle);
            operations.next();
        }

        if handles.len() == 1 {
            ui.move_subtree(first_handle, target_handle, target_end)?;
        } else {
            match ui.move_subtrees_batch(&handles, target_handle, target_end) {
                Ok(_) => {}
                Err(UiError::InvalidRange) => {
                    // Nested ranges can be valid in operation order even though
                    // they overlap in the initial Deque snapshot.
                    for handle in handles {
                        ui.move_subtree(handle, target_handle, target_end)?;
                    }
                }
                Err(error) => return Err(FrameworkError::Ui(error)),
            }
        }
        Ok(())
    }

    fn apply_remove_batch(
        &mut self,
        first: FrameworkElementId,
        operations: &mut std::iter::Peekable<std::vec::IntoIter<FrameworkOperation>>,
        ui: &mut UiModel,
    ) -> Result<(), FrameworkError> {
        let first_handle = *self
            .elements
            .get(&first)
            .ok_or(FrameworkError::UnknownElement(first))?;
        ui.deque_handle_for_element(first_handle)
            .ok_or(FrameworkError::UnknownElement(first))?;
        let mut handles = vec![first_handle];
        while let Some(FrameworkOperation::RemoveSubtree { anchor }) = operations.peek() {
            let Some(handle) = self.elements.get(anchor).copied() else {
                break;
            };
            if ui.deque_handle_for_element(handle).is_none() {
                break;
            }
            operations.next();
            handles.push(handle);
        }

        if handles.len() == 1 {
            let removed = ui.remove_subtree(handles[0])?;
            self.remove_native_elements(&removed);
        } else {
            match ui.remove_subtrees_batch(&handles) {
                Ok(removed) => {
                    for group in removed {
                        self.remove_native_elements(&group);
                    }
                }
                Err(UiError::InvalidRange) => {
                    // Overlapping ranges may still be valid when earlier
                    // operations remove descendants before their ancestors.
                    for handle in handles {
                        let removed = ui.remove_subtree(handle)?;
                        self.remove_native_elements(&removed);
                    }
                }
                Err(error) => return Err(FrameworkError::Ui(error)),
            }
        }
        Ok(())
    }

    /// Applies a commit-wide plan for independent structural operations.
    /// Every source participates in one extraction group, every target receives
    /// moves from one source group, and all ranges were validated against the
    /// same initial Deque snapshot before this method is entered.
    fn apply_global_structural_batch(
        &mut self,
        commit: &FrameworkCommit,
        ui: &mut UiModel,
    ) -> Result<bool, FrameworkError> {
        let mut removals = Vec::new();
        let mut removal_sources = HashSet::new();
        let mut move_sources = HashSet::new();
        let mut move_targets = HashSet::new();
        let mut move_groups: Vec<(usize, DequeHandle, DequeHandle, End, Vec<ElementHandle>)> =
            Vec::new();

        for (index, operation) in commit.operations().iter().enumerate() {
            match operation {
                FrameworkOperation::RemoveSubtree { anchor } => {
                    let handle = *self
                        .elements
                        .get(anchor)
                        .ok_or(FrameworkError::UnknownElement(*anchor))?;
                    let source = ui
                        .deque_handle_for_element(handle)
                        .ok_or(FrameworkError::UnknownElement(*anchor))?;
                    if move_sources.contains(&source) {
                        return Ok(false);
                    }
                    removal_sources.insert(source);
                    removals.push(handle);
                }
                FrameworkOperation::MoveSubtree {
                    anchor,
                    target,
                    target_end,
                } => {
                    let handle = *self
                        .elements
                        .get(anchor)
                        .ok_or(FrameworkError::UnknownElement(*anchor))?;
                    let source = ui
                        .deque_handle_for_element(handle)
                        .ok_or(FrameworkError::UnknownElement(*anchor))?;
                    let target_handle = *self
                        .deques
                        .get(target)
                        .ok_or(FrameworkError::UnknownDeque(*target))?;
                    if removal_sources.contains(&source) {
                        return Ok(false);
                    }

                    if let Some((_, existing_source, existing_target, existing_end, handles)) =
                        move_groups.iter_mut().find(|(_, group_source, _, _, _)| {
                            *group_source == source
                        })
                    {
                        if *existing_source != source
                            || *existing_target != target_handle
                            || *existing_end != *target_end
                        {
                            return Ok(false);
                        }
                        handles.push(handle);
                    } else {
                        if !move_sources.insert(source) || !move_targets.insert(target_handle) {
                            return Ok(false);
                        }
                        move_groups.push((index, source, target_handle, *target_end, vec![handle]));
                    }
                }
                _ => return Ok(false),
            }
        }

        if move_targets.iter().any(|target| {
            removal_sources.contains(target) || move_sources.contains(target)
        }) {
            return Ok(false);
        }

        move_groups.sort_unstable_by_key(|(index, _, _, _, _)| *index);
        if !removals.is_empty() {
            let removed = ui.remove_subtrees_batch(&removals)?;
            for group in removed {
                self.remove_native_elements(&group);
            }
        }
        for (_, _, target, end, handles) in move_groups {
            ui.move_subtrees_batch(&handles, target, end)?;
        }
        Ok(true)
    }

    fn validate(&self, commit: &FrameworkCommit, ui: &UiModel) -> Result<(), FrameworkError> {
        if !commit.operations().iter().any(|operation| {
            matches!(
                operation,
                FrameworkOperation::RemoveSubtree { .. } | FrameworkOperation::MoveSubtree { .. }
            )
        }) {
            return self.validate_simple(commit, ui);
        }

        use std::collections::VecDeque;
        let mut required_deques = Vec::new();
        for operation in commit.operations() {
            match operation {
                FrameworkOperation::RemoveSubtree { anchor } => {
                    if let Some(handle) = self.elements.get(anchor)
                        && let Some(deque) = ui.deque_handle_for_element(*handle)
                    {
                        required_deques.push(deque);
                    }
                }
                FrameworkOperation::MoveSubtree { anchor, target, .. } => {
                    if let Some(handle) = self.elements.get(anchor)
                        && let Some(deque) = ui.deque_handle_for_element(*handle)
                    {
                        required_deques.push(deque);
                    }
                    if let Some(deque) = self.deques.get(target) {
                        required_deques.push(*deque);
                    }
                }
                _ => {}
            }
        }
        required_deques.sort_unstable_by_key(|handle| handle.0);
        required_deques.dedup();
        let native = ui.batch_state_for(&required_deques);
        let reverse_deques = &self.reverse_deques;
        let reverse_elements = &self.reverse_elements;

        let mut deques: HashSet<_> = self.deques.keys().copied().collect();
        let mut elements: HashSet<_> = self.elements.keys().copied().collect();
        let mut text_nodes: HashSet<_> = self
            .elements
            .iter()
            .filter_map(|(id, handle)| {
                ui.element(*handle)
                    .is_some_and(|element| element.is_text())
                    .then_some(*id)
            })
            .collect();
        let mut has_root = ui.root_handle().is_some();
        let mut contents: HashMap<FrameworkDequeId, VecDeque<Option<FrameworkElementId>>> =
            HashMap::new();
        let mut subtree_spans: HashMap<FrameworkDequeId, VecDeque<usize>> = HashMap::new();
        let mut element_deques: HashMap<FrameworkElementId, FrameworkDequeId> = HashMap::new();
        let mut sides: HashMap<FrameworkDequeId, (bool, bool)> = HashMap::new();
        let mut children: HashMap<FrameworkDequeId, [Option<FrameworkDequeId>; 2]> = HashMap::new();
        let mut has_children = HashSet::<FrameworkDequeId>::new();

        for (native_id, handles) in native.deques {
            if let Some(framework_id) = reverse_deques.get(&native_id).copied() {
                let mut sequence = VecDeque::with_capacity(handles.len());
                for handle in handles {
                    let framework_element = reverse_elements.get(&handle).copied();
                    if let Some(id) = framework_element {
                        element_deques.insert(id, framework_id);
                    }
                    sequence.push_back(framework_element);
                }
                contents.insert(framework_id, sequence);
                if let Some(spans) = native.subtree_spans.get(&native_id) {
                    subtree_spans.insert(framework_id, spans.clone());
                }
                if let Some((left, right)) = native.sides.get(&native_id) {
                    sides.insert(framework_id, (*left, *right));
                    if *left || *right {
                        has_children.insert(framework_id);
                    }
                }
                if let Some(native_children) = native.children.get(&native_id) {
                    children.insert(
                        framework_id,
                        [
                            native_children[0]
                                .and_then(|child| reverse_deques.get(&child).copied()),
                            native_children[1]
                                .and_then(|child| reverse_deques.get(&child).copied()),
                        ],
                    );
                }
            }
        }

        for op in commit.operations() {
            match op {
                FrameworkOperation::CreateRootDeque { id } => {
                    if has_root {
                        return Err(FrameworkError::RootAlreadyBound);
                    }
                    if !deques.insert(*id) {
                        return Err(FrameworkError::DuplicateDeque(*id));
                    }
                    has_root = true;
                    contents.insert(*id, VecDeque::new());
                    subtree_spans.insert(*id, VecDeque::new());
                    sides.insert(*id, (false, false));
                    children.insert(*id, [None, None]);
                }
                FrameworkOperation::CreateChildDeque { id, parent, side } => {
                    if ui.topology_is_sealed() {
                        return Err(FrameworkError::Ui(UiError::TopologySealed));
                    }
                    if !deques.contains(parent) {
                        return Err(FrameworkError::UnknownDeque(*parent));
                    }
                    if !deques.insert(*id) {
                        return Err(FrameworkError::DuplicateDeque(*id));
                    }
                    let parent_sides = sides
                        .get_mut(parent)
                        .ok_or(FrameworkError::UnknownDeque(*parent))?;
                    let occupied = match side {
                        ChildSide::Left => &mut parent_sides.0,
                        ChildSide::Right => &mut parent_sides.1,
                    };
                    if *occupied {
                        return Err(FrameworkError::Ui(UiError::InvalidSide));
                    }
                    *occupied = true;
                    has_children.insert(*parent);
                    let child_index = match side {
                        ChildSide::Left => 0,
                        ChildSide::Right => 1,
                    };
                    children
                        .get_mut(parent)
                        .ok_or(FrameworkError::UnknownDeque(*parent))?[child_index] = Some(*id);
                    contents.insert(*id, VecDeque::new());
                    subtree_spans.insert(*id, VecDeque::new());
                    sides.insert(*id, (false, false));
                    children.insert(*id, [None, None]);
                }
                FrameworkOperation::CreateElement { id, deque, end, .. }
                | FrameworkOperation::CreateText { id, deque, end, .. } => {
                    if !deques.contains(deque) {
                        return Err(FrameworkError::UnknownDeque(*deque));
                    }
                    if !elements.insert(*id) {
                        return Err(FrameworkError::DuplicateElement(*id));
                    }
                    let sequence = contents
                        .get_mut(deque)
                        .ok_or(FrameworkError::UnknownDeque(*deque))?;
                    let spans = subtree_spans
                        .get_mut(deque)
                        .ok_or(FrameworkError::UnknownDeque(*deque))?;
                    match end {
                        End::Front => {
                            if let Some(root_span) = spans.back_mut() {
                                *root_span += 1;
                            }
                            sequence.push_front(Some(*id));
                            spans.push_front(1);
                        }
                        End::Back => {
                            let span = sequence.len() + 1;
                            sequence.push_back(Some(*id));
                            spans.push_back(span);
                        }
                    }
                    element_deques.insert(*id, *deque);
                    if matches!(op, FrameworkOperation::CreateText { .. }) {
                        text_nodes.insert(*id);
                    }
                }
                FrameworkOperation::SetText { id, .. } => {
                    if !elements.contains(id) {
                        return Err(FrameworkError::UnknownElement(*id));
                    }
                    if !text_nodes.contains(id) {
                        return Err(FrameworkError::NotText(*id));
                    }
                }
                FrameworkOperation::SetProperty { id, .. }
                | FrameworkOperation::RemoveProperty { id, .. }
                | FrameworkOperation::SetStyle { id, .. }
                | FrameworkOperation::RemoveStyle { id, .. } => {
                    if !elements.contains(id) {
                        return Err(FrameworkError::UnknownElement(*id));
                    }
                }
                FrameworkOperation::RemoveSubtree { anchor } => {
                    if !elements.contains(anchor) {
                        return Err(FrameworkError::UnknownElement(*anchor));
                    }
                    let deque_id = *element_deques
                        .get(anchor)
                        .ok_or(FrameworkError::UnknownElement(*anchor))?;
                    let (start, count, is_root) = {
                        let sequence = contents
                            .get(&deque_id)
                            .ok_or(FrameworkError::UnknownDeque(deque_id))?;
                        let spans = subtree_spans
                            .get(&deque_id)
                            .ok_or(FrameworkError::UnknownDeque(deque_id))?;
                        let index = sequence
                            .iter()
                            .position(|item| *item == Some(*anchor))
                            .ok_or(FrameworkError::UnknownElement(*anchor))?;
                        let count = spans[index];
                        if count == 0 || count > index + 1 {
                            return Err(FrameworkError::InvalidRange);
                        }
                        let start = index + 1 - count;
                        (
                            start,
                            count,
                            index + 1 == sequence.len() && start == 0 && count == sequence.len(),
                        )
                    };
                    if is_root {
                        let mut pending = vec![deque_id];
                        let mut descendants = Vec::new();
                        while let Some(current) = pending.pop() {
                            descendants.push(current);
                            if let Some(branches) = children.get(&current) {
                                if let Some(right) = branches[1] {
                                    pending.push(right);
                                }
                                if let Some(left) = branches[0] {
                                    pending.push(left);
                                }
                            }
                        }
                        for current in descendants {
                            for removed in contents
                                .get_mut(&current)
                                .ok_or(FrameworkError::UnknownDeque(current))?
                                .drain(..)
                                .flatten()
                            {
                                elements.remove(&removed);
                                text_nodes.remove(&removed);
                                element_deques.remove(&removed);
                            }
                            subtree_spans
                                .get_mut(&current)
                                .ok_or(FrameworkError::UnknownDeque(current))?
                                .clear();
                        }
                    } else {
                        let sequence = contents
                            .get_mut(&deque_id)
                            .ok_or(FrameworkError::UnknownDeque(deque_id))?;
                        let spans = subtree_spans
                            .get_mut(&deque_id)
                            .ok_or(FrameworkError::UnknownDeque(deque_id))?;
                        let end = start + count;
                        for (ancestor_index, span) in spans.iter_mut().enumerate().skip(end) {
                            let old_start = ancestor_index + 1 - *span;
                            if old_start <= start {
                                *span -= count;
                            }
                        }
                        for removed in sequence.drain(start..end).flatten() {
                            elements.remove(&removed);
                            text_nodes.remove(&removed);
                            element_deques.remove(&removed);
                        }
                        spans.drain(start..end);
                    }
                }
                FrameworkOperation::MoveSubtree {
                    anchor,
                    target,
                    target_end,
                } => {
                    if !elements.contains(anchor) {
                        return Err(FrameworkError::UnknownElement(*anchor));
                    }
                    if !deques.contains(target) {
                        return Err(FrameworkError::UnknownDeque(*target));
                    }
                    let source_id = *element_deques
                        .get(anchor)
                        .ok_or(FrameworkError::UnknownElement(*anchor))?;
                    if source_id == *target {
                        return Err(FrameworkError::InvalidRange);
                    }
                    let source_sequence = contents
                        .get(&source_id)
                        .ok_or(FrameworkError::UnknownDeque(source_id))?;
                    let source_spans = subtree_spans
                        .get(&source_id)
                        .ok_or(FrameworkError::UnknownDeque(source_id))?;
                    let anchor_index = source_sequence
                        .iter()
                        .position(|item| *item == Some(*anchor))
                        .ok_or(FrameworkError::UnknownElement(*anchor))?;
                    let is_root = anchor_index + 1 == source_sequence.len()
                        && source_spans[anchor_index] == source_sequence.len();
                    if is_root && has_children.contains(&source_id) {
                        return Err(FrameworkError::InvalidRange);
                    }
                    let (moved, moved_spans) = {
                        let source = contents
                            .get_mut(&source_id)
                            .ok_or(FrameworkError::UnknownDeque(source_id))?;
                        let spans = subtree_spans
                            .get_mut(&source_id)
                            .ok_or(FrameworkError::UnknownDeque(source_id))?;
                        let index = anchor_index;
                        let count = spans[index];
                        if count == 0 || count > index + 1 {
                            return Err(FrameworkError::InvalidRange);
                        }
                        let start = index + 1 - count;
                        let end = start + count;
                        for (ancestor_index, span) in spans.iter_mut().enumerate().skip(end) {
                            let old_start = ancestor_index + 1 - *span;
                            if old_start <= start {
                                *span -= count;
                            }
                        }
                        (
                            source.drain(start..end).collect::<Vec<_>>(),
                            spans.drain(start..end).collect::<Vec<_>>(),
                        )
                    };
                    let target_contents = contents
                        .get_mut(target)
                        .ok_or(FrameworkError::UnknownDeque(*target))?;
                    let target_spans = subtree_spans
                        .get_mut(target)
                        .ok_or(FrameworkError::UnknownDeque(*target))?;
                    let target_old_len = target_contents.len();
                    match target_end {
                        End::Front => {
                            if let Some(root_span) = target_spans.back_mut() {
                                *root_span += moved.len();
                            }
                            for (item, span) in
                                moved.iter().copied().zip(moved_spans.iter().copied()).rev()
                            {
                                target_contents.push_front(item);
                                target_spans.push_front(span);
                            }
                        }
                        End::Back => {
                            for (item, mut span) in
                                moved.iter().copied().zip(moved_spans.iter().copied())
                            {
                                if target_old_len > 0 && span == moved.len() {
                                    span += target_old_len;
                                }
                                target_contents.push_back(item);
                                target_spans.push_back(span);
                            }
                        }
                    }
                    for id in moved.into_iter().flatten() {
                        element_deques.insert(id, *target);
                    }
                }
            }
        }
        Ok(())
    }
}
impl FrameworkAdapter for CommitAdapter {
    fn apply_commit(
        &mut self,
        commit: FrameworkCommit,
        ui: &mut UiModel,
    ) -> Result<(), FrameworkError> {
        self.prune_stale_references(&commit, ui);
        let structural_batch = self.validate_structural_only_batch(&commit, ui)?;
        if !structural_batch {
            self.validate(&commit, ui)?;
        }
        let defer_registry_updates = commit.operations().iter().any(|operation| {
            matches!(
                operation,
                FrameworkOperation::RemoveSubtree { .. } | FrameworkOperation::MoveSubtree { .. }
            )
        });
        if defer_registry_updates {
            ui.begin_registry_batch();
        }
        let apply_result = (|| -> Result<(), FrameworkError> {
            if structural_batch && self.apply_global_structural_batch(&commit, ui)? {
                return Ok(());
            }
            let mut operations = commit.operations.into_iter().peekable();
            while let Some(op) = operations.next() {
                match op {
                    FrameworkOperation::CreateRootDeque { id } => {
                        let h = ui.create_root_deque()?;
                        self.deques.insert(id, h);
                        self.reverse_deques.insert(h, id);
                    }
                    FrameworkOperation::CreateChildDeque { id, parent, side } => {
                        let p = self.deques[&parent];
                        let h = ui.create_child_deque(p, side)?;
                        self.deques.insert(id, h);
                        self.reverse_deques.insert(h, id);
                    }
                    FrameworkOperation::CreateElement {
                        id,
                        deque,
                        tag,
                        end,
                    } => {
                        self.apply_create_batch(
                            deque,
                            end,
                            (id, UiElement::new(tag)),
                            &mut operations,
                            ui,
                        )?;
                    }
                    FrameworkOperation::CreateText {
                        id,
                        deque,
                        text,
                        end,
                    } => {
                        self.apply_create_batch(
                            deque,
                            end,
                            (id, UiElement::text(text)),
                            &mut operations,
                            ui,
                        )?;
                    }
                    FrameworkOperation::SetText { id, text } => {
                        let h = self.elements[&id];
                        ui.set_text(h, text).map_err(|e| match e {
                            UiError::NotText(_) => FrameworkError::NotText(id),
                            other => FrameworkError::Ui(other),
                        })?;
                    }
                    FrameworkOperation::SetProperty { id, key, value } => {
                        ui.set_property(self.elements[&id], key, value)?
                    }
                    FrameworkOperation::RemoveProperty { id, key } => {
                        ui.remove_property(self.elements[&id], &key)?
                    }
                    FrameworkOperation::SetStyle { id, key, value } => {
                        ui.set_style(self.elements[&id], key, value)?
                    }
                    FrameworkOperation::RemoveStyle { id, key } => {
                        ui.remove_style(self.elements[&id], &key)?
                    }
                    FrameworkOperation::RemoveSubtree { anchor } => {
                        self.apply_remove_batch(anchor, &mut operations, ui)?
                    }
                    FrameworkOperation::MoveSubtree {
                        anchor,
                        target,
                        target_end,
                    } => self.apply_move_batch(anchor, target, target_end, &mut operations, ui)?,
                }
            }
            Ok(())
        })();
        if defer_registry_updates {
            ui.finish_registry_batch();
        }
        apply_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deque_tree::End;

    #[test]
    fn framework_root_cannot_be_bound_to_multiple_framework_ids() {
        let mut ui = UiModel::new();
        ui.create_root_deque().unwrap();
        let mut adapter = CommitAdapter::new();
        adapter
            .bind_existing_root(FrameworkDequeId(1), &ui)
            .unwrap();

        assert!(matches!(
            adapter.bind_existing_root(FrameworkDequeId(2), &ui),
            Err(FrameworkError::RootAlreadyBound)
        ));
        assert_eq!(adapter.native_deque(FrameworkDequeId(1)), ui.root_handle());
        assert!(adapter.native_deque(FrameworkDequeId(2)).is_none());
    }

    #[test]
    fn consecutive_front_creates_are_batched_without_changing_order_or_handles() {
        let mut ui = UiModel::from_initial_groups(vec![InitialDeque {
            handle: DequeHandle(100),
            elements: vec![InitialElement {
                handle: ElementHandle(1_000),
                element: UiElement::new("main"),
                subtree_span: 1,
            }],
        }])
        .unwrap();
        let parent = ElementHandle(1_000);
        let mut adapter = CommitAdapter::new();
        adapter
            .bind_existing_root(FrameworkDequeId(1), &ui)
            .unwrap();

        let mut commit = FrameworkCommit::new();
        commit.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(1),
            deque: FrameworkDequeId(1),
            tag: "article".into(),
            end: End::Front,
        });
        commit.push(FrameworkOperation::CreateText {
            id: FrameworkElementId(2),
            deque: FrameworkDequeId(1),
            text: "batched text".into(),
            end: End::Front,
        });
        commit.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(3),
            deque: FrameworkDequeId(1),
            tag: "aside".into(),
            end: End::Front,
        });

        adapter.apply_commit(commit, &mut ui).unwrap();

        let first = adapter.native_element(FrameworkElementId(1)).unwrap();
        let second = adapter.native_element(FrameworkElementId(2)).unwrap();
        let third = adapter.native_element(FrameworkElementId(3)).unwrap();
        assert_eq!(ui.logical_children(parent), vec![first, second, third]);
        assert_eq!(ui.element(first).unwrap().tag, "article");
        assert_eq!(
            ui.element(second).unwrap().text_content(),
            Some("batched text")
        );
        assert_eq!(ui.element(third).unwrap().tag, "aside");
    }

    #[test]
    fn consecutive_moves_from_one_deque_are_batched_and_preserve_target_order() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Left,
        });
        setup.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(100),
            deque: FrameworkDequeId(1),
            tag: "main".into(),
            end: End::Back,
        });
        for id in 1..=3 {
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(id),
                deque: FrameworkDequeId(1),
                tag: "span".into(),
                end: End::Front,
            });
        }
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();
        let parent = adapter.native_element(FrameworkElementId(100)).unwrap();
        let original: Vec<_> = (1..=3)
            .map(|id| adapter.native_element(FrameworkElementId(id)).unwrap())
            .collect();

        let mut move_commit = FrameworkCommit::new();
        for id in 1..=3 {
            move_commit.push(FrameworkOperation::MoveSubtree {
                anchor: FrameworkElementId(id),
                target: FrameworkDequeId(2),
                target_end: End::Back,
            });
        }
        adapter.apply_commit(move_commit, &mut ui).unwrap();

        assert_eq!(ui.logical_children(parent), vec![original[2]]);
        assert_eq!(ui.logical_parent(original[2]), Some(parent));
        assert_eq!(ui.logical_parent(original[1]), Some(original[2]));
        assert_eq!(ui.logical_parent(original[0]), Some(original[1]));
        for (index, handle) in original.iter().enumerate() {
            assert_eq!(
                adapter.native_element(FrameworkElementId(index as u64 + 1)),
                Some(*handle)
            );
        }
    }

    #[test]
    fn interleaved_independent_moves_are_batched_by_source_across_the_commit() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Left,
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(3),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Right,
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(4),
            parent: FrameworkDequeId(2),
            side: crate::deque_tree::ChildSide::Left,
        });
        for (id, deque, end) in [
            (100, 1, End::Back),
            (1, 1, End::Front),
            (2, 1, End::Front),
            (200, 3, End::Back),
            (11, 3, End::Front),
        ] {
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(id),
                deque: FrameworkDequeId(deque),
                tag: "span".into(),
                end,
            });
        }
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();

        let first = adapter.native_element(FrameworkElementId(1)).unwrap();
        let second = adapter.native_element(FrameworkElementId(2)).unwrap();
        let third = adapter.native_element(FrameworkElementId(11)).unwrap();
        let target_left = adapter.native_deque(FrameworkDequeId(2)).unwrap();
        let target_nested = adapter.native_deque(FrameworkDequeId(4)).unwrap();

        let mut commit = FrameworkCommit::new();
        commit.push(FrameworkOperation::MoveSubtree {
            anchor: FrameworkElementId(1),
            target: FrameworkDequeId(2),
            target_end: End::Back,
        });
        commit.push(FrameworkOperation::MoveSubtree {
            anchor: FrameworkElementId(11),
            target: FrameworkDequeId(4),
            target_end: End::Back,
        });
        commit.push(FrameworkOperation::MoveSubtree {
            anchor: FrameworkElementId(2),
            target: FrameworkDequeId(2),
            target_end: End::Back,
        });

        adapter.apply_commit(commit, &mut ui).unwrap();

        assert_eq!(ui.deque_handle_for_element(first), Some(target_left));
        assert_eq!(ui.deque_handle_for_element(second), Some(target_left));
        assert_eq!(ui.logical_parent(first), Some(second));
        assert_eq!(ui.deque_handle_for_element(third), Some(target_nested));
        assert!(ui.logical_parent(third).is_none());
        assert_eq!(
            adapter.native_element(FrameworkElementId(1)),
            Some(first)
        );
        assert_eq!(
            adapter.native_element(FrameworkElementId(2)),
            Some(second)
        );
    }

    #[test]
    fn nested_move_batch_falls_back_to_sequential_semantics_when_ranges_overlap() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Left,
        });
        for (id, tag) in [(1, "i"), (2, "section"), (100, "main")] {
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(id),
                deque: FrameworkDequeId(1),
                tag: tag.into(),
                end: End::Back,
            });
        }
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();
        let leaf = adapter.native_element(FrameworkElementId(1)).unwrap();
        let parent = adapter.native_element(FrameworkElementId(2)).unwrap();
        let root = adapter.native_element(FrameworkElementId(100)).unwrap();

        let mut move_commit = FrameworkCommit::new();
        for id in [1, 2] {
            move_commit.push(FrameworkOperation::MoveSubtree {
                anchor: FrameworkElementId(id),
                target: FrameworkDequeId(2),
                target_end: End::Back,
            });
        }
        adapter.apply_commit(move_commit, &mut ui).unwrap();
        assert_eq!(ui.logical_parent(parent), Some(root));
        assert_eq!(ui.logical_parent(leaf), Some(parent));
    }

    #[test]
    fn nested_remove_batch_falls_back_to_sequential_semantics_when_ranges_overlap() {
        let mut ui = UiModel::new();
        let root_deque = ui.create_root_deque().unwrap();
        let mut adapter = CommitAdapter::new();
        adapter
            .bind_existing_root(FrameworkDequeId(1), &ui)
            .unwrap();
        let mut setup = FrameworkCommit::new();
        for (id, tag) in [(1, "i"), (2, "section"), (100, "main")] {
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(id),
                deque: FrameworkDequeId(1),
                tag: tag.into(),
                end: End::Back,
            });
        }
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();
        let leaf = adapter.native_element(FrameworkElementId(1)).unwrap();
        let parent = adapter.native_element(FrameworkElementId(2)).unwrap();
        let root = adapter.native_element(FrameworkElementId(100)).unwrap();

        let mut remove = FrameworkCommit::new();
        remove.push(FrameworkOperation::RemoveSubtree {
            anchor: FrameworkElementId(1),
        });
        remove.push(FrameworkOperation::RemoveSubtree {
            anchor: FrameworkElementId(2),
        });
        adapter.apply_commit(remove, &mut ui).unwrap();
        assert!(ui.element(leaf).is_none());
        assert!(ui.element(parent).is_none());
        assert!(ui.element(root).is_some());
        assert!(ui.deque_id(root_deque).is_some());
    }

    #[test]
    fn consecutive_removals_across_multiple_deques_use_the_multi_source_batch_path() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Left,
        });
        for (deque, base) in [(1u64, 1u64), (2u64, 11u64)] {
            for index in 0..4u64 {
                setup.push(FrameworkOperation::CreateElement {
                    id: FrameworkElementId(base + index),
                    deque: FrameworkDequeId(deque),
                    tag: "span".into(),
                    end: End::Front,
                });
            }
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(base + 4),
                deque: FrameworkDequeId(deque),
                tag: "main".into(),
                end: End::Back,
            });
        }
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();
        let removed_a = adapter.native_element(FrameworkElementId(3)).unwrap();
        let removed_a2 = adapter.native_element(FrameworkElementId(4)).unwrap();
        let kept_a = adapter.native_element(FrameworkElementId(5)).unwrap();
        let removed_b = adapter.native_element(FrameworkElementId(13)).unwrap();
        let removed_b2 = adapter.native_element(FrameworkElementId(14)).unwrap();
        let kept_b = adapter.native_element(FrameworkElementId(15)).unwrap();
        assert!(ui.element(removed_a).is_some());
        assert!(ui.element(removed_a2).is_some());
        assert!(ui.element(removed_b).is_some());
        assert!(ui.element(removed_b2).is_some());

        let mut remove = FrameworkCommit::new();
        for id in [3, 4, 13, 14] {
            remove.push(FrameworkOperation::RemoveSubtree {
                anchor: FrameworkElementId(id),
            });
        }
        adapter.apply_commit(remove, &mut ui).unwrap();

        assert!(ui.element(removed_a).is_none());
        assert!(ui.element(removed_b).is_none());
        assert!(ui.element(removed_a2).is_none());
        assert!(ui.element(removed_b2).is_none());
        assert!(ui.element(kept_a).is_some());
        assert!(ui.element(kept_b).is_some());
        assert!(adapter.native_element(FrameworkElementId(3)).is_none());
        assert!(adapter.native_element(FrameworkElementId(4)).is_none());
        assert!(adapter.native_element(FrameworkElementId(13)).is_none());
        assert!(adapter.native_element(FrameworkElementId(14)).is_none());
    }

    #[test]
    fn mixed_multi_source_remove_and_move_commit_uses_structural_fast_path() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Left,
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(3),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Right,
        });
        for (deque, base) in [(1u64, 1u64), (2u64, 11u64)] {
            for index in 0..4u64 {
                setup.push(FrameworkOperation::CreateElement {
                    id: FrameworkElementId(base + index),
                    deque: FrameworkDequeId(deque),
                    tag: "span".into(),
                    end: End::Front,
                });
            }
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(base + 4),
                deque: FrameworkDequeId(deque),
                tag: "main".into(),
                end: End::Back,
            });
        }
        setup.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(30),
            deque: FrameworkDequeId(3),
            tag: "aside".into(),
            end: End::Back,
        });
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();

        let removed_a = adapter.native_element(FrameworkElementId(3)).unwrap();
        let removed_a2 = adapter.native_element(FrameworkElementId(4)).unwrap();
        let moved_b = adapter.native_element(FrameworkElementId(13)).unwrap();
        let removed_b = adapter.native_element(FrameworkElementId(14)).unwrap();
        let target_root = adapter.native_element(FrameworkElementId(30)).unwrap();

        let mut commit = FrameworkCommit::new();
        for id in [3, 4] {
            commit.push(FrameworkOperation::RemoveSubtree {
                anchor: FrameworkElementId(id),
            });
        }
        commit.push(FrameworkOperation::MoveSubtree {
            anchor: FrameworkElementId(13),
            target: FrameworkDequeId(3),
            target_end: End::Front,
        });
        commit.push(FrameworkOperation::RemoveSubtree {
            anchor: FrameworkElementId(14),
        });

        assert!(
            adapter
                .validate_structural_only_batch(&commit, &ui)
                .unwrap()
        );
        adapter.apply_commit(commit, &mut ui).unwrap();

        assert!(ui.element(removed_a).is_none());
        assert!(ui.element(removed_a2).is_none());
        assert!(ui.element(removed_b).is_none());
        assert_eq!(ui.logical_parent(moved_b), Some(target_root));
        assert_eq!(
            adapter.native_element(FrameworkElementId(13)),
            Some(moved_b)
        );
        assert!(adapter.native_element(FrameworkElementId(3)).is_none());
        assert!(adapter.native_element(FrameworkElementId(4)).is_none());
        assert!(adapter.native_element(FrameworkElementId(14)).is_none());
    }

    #[test]
    fn structural_fast_path_falls_back_when_a_move_target_is_also_a_source() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Left,
        });
        for (id, deque, end) in [
            (1, 1, End::Front),
            (2, 1, End::Back),
            (3, 2, End::Front),
            (4, 2, End::Back),
        ] {
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(id),
                deque: FrameworkDequeId(deque),
                tag: "div".into(),
                end,
            });
        }
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();
        let moved = adapter.native_element(FrameworkElementId(1)).unwrap();
        let removed = adapter.native_element(FrameworkElementId(3)).unwrap();
        let target_root = adapter.native_element(FrameworkElementId(4)).unwrap();

        let mut commit = FrameworkCommit::new();
        commit.push(FrameworkOperation::MoveSubtree {
            anchor: FrameworkElementId(1),
            target: FrameworkDequeId(2),
            target_end: End::Front,
        });
        commit.push(FrameworkOperation::RemoveSubtree {
            anchor: FrameworkElementId(3),
        });
        assert!(
            !adapter
                .validate_structural_only_batch(&commit, &ui)
                .unwrap()
        );
        adapter.apply_commit(commit, &mut ui).unwrap();

        assert!(ui.element(removed).is_none());
        assert_eq!(ui.logical_parent(moved), Some(target_root));
    }

    #[test]
    fn consecutive_moves_from_multiple_source_deques_share_one_validated_commit() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Left,
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(3),
            parent: FrameworkDequeId(1),
            side: crate::deque_tree::ChildSide::Right,
        });
        for (id, deque, tag, end) in [
            (1, 1, "span", End::Front),
            (2, 1, "main", End::Back),
            (3, 2, "i", End::Front),
            (4, 2, "section", End::Back),
            (5, 3, "aside", End::Back),
        ] {
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(id),
                deque: FrameworkDequeId(deque),
                tag: tag.into(),
                end,
            });
        }
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();
        let moved_a = adapter.native_element(FrameworkElementId(1)).unwrap();
        let moved_b = adapter.native_element(FrameworkElementId(3)).unwrap();
        let target_root = adapter.native_element(FrameworkElementId(5)).unwrap();

        let mut moves = FrameworkCommit::new();
        for id in [1, 3] {
            moves.push(FrameworkOperation::MoveSubtree {
                anchor: FrameworkElementId(id),
                target: FrameworkDequeId(3),
                target_end: End::Front,
            });
        }
        adapter.apply_commit(moves, &mut ui).unwrap();

        assert_eq!(ui.logical_children(target_root), vec![moved_a, moved_b]);
        assert_eq!(ui.logical_parent(moved_a), Some(target_root));
        assert_eq!(ui.logical_parent(moved_b), Some(target_root));
        assert_eq!(adapter.native_element(FrameworkElementId(1)), Some(moved_a));
        assert_eq!(adapter.native_element(FrameworkElementId(3)), Some(moved_b));
    }

    #[test]
    fn consecutive_removals_from_one_deque_are_batched_and_keep_framework_mappings_consistent() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let mut adapter = CommitAdapter::new();
        adapter
            .bind_existing_root(FrameworkDequeId(1), &ui)
            .unwrap();

        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(100),
            deque: FrameworkDequeId(1),
            tag: "main".into(),
            end: End::Back,
        });
        for id in 1..=3 {
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(id),
                deque: FrameworkDequeId(1),
                tag: "span".into(),
                end: End::Front,
            });
        }
        adapter.apply_commit(setup, &mut ui).unwrap();
        ui.prepare_render();
        let parent = adapter.native_element(FrameworkElementId(100)).unwrap();
        assert_eq!(ui.logical_children(parent).len(), 3);

        let mut remove = FrameworkCommit::new();
        for id in 1..=3 {
            remove.push(FrameworkOperation::RemoveSubtree {
                anchor: FrameworkElementId(id),
            });
        }
        adapter.apply_commit(remove, &mut ui).unwrap();

        assert!(ui.logical_children(parent).is_empty());
        for id in 1..=3 {
            assert!(adapter.native_element(FrameworkElementId(id)).is_none());
        }
        assert!(ui.deque_id(root).is_some());
        assert!(ui.element(parent).is_some());
    }

    #[test]
    fn consecutive_back_creates_are_batched_with_ancestor_order_preserved() {
        let mut ui = UiModel::from_initial_groups(vec![InitialDeque {
            handle: DequeHandle(101),
            elements: vec![InitialElement {
                handle: ElementHandle(1_001),
                element: UiElement::new("main"),
                subtree_span: 1,
            }],
        }])
        .unwrap();
        let existing_root = ElementHandle(1_001);
        let mut adapter = CommitAdapter::new();
        adapter
            .bind_existing_root(FrameworkDequeId(1), &ui)
            .unwrap();

        let mut commit = FrameworkCommit::new();
        commit.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(11),
            deque: FrameworkDequeId(1),
            tag: "section".into(),
            end: End::Back,
        });
        commit.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(12),
            deque: FrameworkDequeId(1),
            tag: "article".into(),
            end: End::Back,
        });

        adapter.apply_commit(commit, &mut ui).unwrap();

        let first = adapter.native_element(FrameworkElementId(11)).unwrap();
        let second = adapter.native_element(FrameworkElementId(12)).unwrap();
        assert_eq!(ui.logical_parent(existing_root), Some(first));
        assert_eq!(ui.logical_parent(first), Some(second));
        assert_eq!(ui.logical_parent(second), None);
        assert_eq!(ui.logical_children(second), vec![first]);
        assert_eq!(ui.logical_children(first), vec![existing_root]);
    }

    #[test]
    fn framework_commit_uses_only_handles_and_shared_native_tree() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut commit = FrameworkCommit::new();
        commit.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        commit.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: ChildSide::Left,
        });
        commit.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(10),
            deque: FrameworkDequeId(1),
            tag: "parent".into(),
            end: End::Back,
        });
        commit.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(11),
            deque: FrameworkDequeId(1),
            tag: "child".into(),
            end: End::Front,
        });
        adapter.apply_commit(commit, &mut ui).unwrap();
        let parent = adapter.native_element(FrameworkElementId(10)).unwrap();
        let child = adapter.native_element(FrameworkElementId(11)).unwrap();
        let mut remove = FrameworkCommit::new();
        remove.push(FrameworkOperation::RemoveSubtree {
            anchor: FrameworkElementId(10),
        });
        adapter.apply_commit(remove, &mut ui).unwrap();
        assert!(ui.element(parent).is_none());
        assert!(ui.element(child).is_none());
        assert!(adapter.native_element(FrameworkElementId(10)).is_none());
        assert!(adapter.native_element(FrameworkElementId(11)).is_none());
    }

    #[test]
    fn framework_parent_removal_clears_descendant_deque_branches() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut create = FrameworkCommit::new();
        create.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(20),
        });
        create.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(21),
            parent: FrameworkDequeId(20),
            side: ChildSide::Left,
        });
        create.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(22),
            parent: FrameworkDequeId(20),
            side: ChildSide::Right,
        });
        create.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(30),
            deque: FrameworkDequeId(20),
            tag: "parent".into(),
            end: End::Back,
        });
        create.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(31),
            deque: FrameworkDequeId(21),
            tag: "left-child".into(),
            end: End::Back,
        });
        create.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(32),
            deque: FrameworkDequeId(22),
            tag: "right-child".into(),
            end: End::Back,
        });
        create.push(FrameworkOperation::RemoveSubtree {
            anchor: FrameworkElementId(30),
        });
        adapter.apply_commit(create, &mut ui).unwrap();
        assert!(adapter.native_element(FrameworkElementId(30)).is_none());
        assert!(adapter.native_element(FrameworkElementId(31)).is_none());
        assert!(adapter.native_element(FrameworkElementId(32)).is_none());
        for id in [
            FrameworkDequeId(20),
            FrameworkDequeId(21),
            FrameworkDequeId(22),
        ] {
            let native = adapter.native_deque(id).unwrap();
            assert!(
                ui.tree()
                    .get(ui.deque_id(native).unwrap())
                    .unwrap()
                    .deque()
                    .is_ghost()
            );
        }
    }

    #[test]
    fn framework_move_subtree_uses_derived_span_and_preserves_sibling_relations() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut create = FrameworkCommit::new();
        create.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        create.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: ChildSide::Left,
        });
        create.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(10),
            deque: FrameworkDequeId(1),
            tag: "parent".into(),
            end: End::Back,
        });
        create.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(11),
            deque: FrameworkDequeId(1),
            tag: "first".into(),
            end: End::Front,
        });
        create.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(12),
            deque: FrameworkDequeId(1),
            tag: "second".into(),
            end: End::Front,
        });
        adapter.apply_commit(create, &mut ui).unwrap();

        let mut move_commit = FrameworkCommit::new();
        move_commit.push(FrameworkOperation::MoveSubtree {
            anchor: FrameworkElementId(11),
            target: FrameworkDequeId(2),
            target_end: End::Back,
        });
        adapter.apply_commit(move_commit, &mut ui).unwrap();

        let parent = adapter.native_element(FrameworkElementId(10)).unwrap();
        let first = adapter.native_element(FrameworkElementId(11)).unwrap();
        let second = adapter.native_element(FrameworkElementId(12)).unwrap();
        assert!(ui.element(first).is_some());
        assert!(ui.element(second).is_some());
        assert!(ui.element(parent).is_some());
        assert_eq!(adapter.native_element(FrameworkElementId(11)), Some(first));
        assert_eq!(adapter.native_element(FrameworkElementId(12)), Some(second));
    }

    #[test]
    fn duplicate_child_side_in_simple_commit_is_rejected_atomically() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut commit = FrameworkCommit::new();
        commit.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(80),
        });
        commit.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(81),
            parent: FrameworkDequeId(80),
            side: ChildSide::Left,
        });
        commit.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(82),
            parent: FrameworkDequeId(80),
            side: ChildSide::Left,
        });

        assert!(adapter.apply_commit(commit, &mut ui).is_err());
        assert!(ui.root_handle().is_none());
        assert_eq!(ui.tree().len(), 0);
        assert!(adapter.native_deque(FrameworkDequeId(80)).is_none());
    }

    #[test]
    fn structural_validation_detects_unmapped_child_deques_before_applying_prior_ops() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut setup = FrameworkCommit::new();
        setup.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(2),
            parent: FrameworkDequeId(1),
            side: ChildSide::Left,
        });
        setup.push(FrameworkOperation::CreateChildDeque {
            id: FrameworkDequeId(3),
            parent: FrameworkDequeId(1),
            side: ChildSide::Right,
        });
        setup.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(4),
            deque: FrameworkDequeId(2),
            tag: "section".into(),
            end: End::Back,
        });
        setup.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(5),
            deque: FrameworkDequeId(3),
            tag: "aside".into(),
            end: End::Back,
        });
        adapter.apply_commit(setup, &mut ui).unwrap();

        // Mutate the shared UI directly, leaving this thin adapter without an ID
        // for the new child Deque. Its existence must still constrain root moves.
        let source = adapter.native_deque(FrameworkDequeId(2)).unwrap();
        ui.create_child_deque(source, ChildSide::Left).unwrap();
        ui.seal_initial_topology();

        let mut invalid = FrameworkCommit::new();
        invalid.push(FrameworkOperation::SetProperty {
            id: FrameworkElementId(5),
            key: "before".into(),
            value: "must-not-apply".into(),
        });
        invalid.push(FrameworkOperation::MoveSubtree {
            anchor: FrameworkElementId(4),
            target: FrameworkDequeId(3),
            target_end: End::Back,
        });

        assert!(adapter.apply_commit(invalid, &mut ui).is_err());
        let target = adapter.native_element(FrameworkElementId(5)).unwrap();
        assert!(
            !ui.element(target)
                .unwrap()
                .properties
                .contains_key("before")
        );
    }

    #[test]
    fn commit_validation_rejects_unknown_references_before_mutation() {
        let mut ui = UiModel::new();
        let mut adapter = CommitAdapter::new();
        let mut commit = FrameworkCommit::new();
        commit.push(FrameworkOperation::CreateRootDeque {
            id: FrameworkDequeId(1),
        });
        commit.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(2),
            deque: FrameworkDequeId(999),
            tag: "bad".into(),
            end: End::Back,
        });
        assert!(matches!(
            adapter.apply_commit(commit, &mut ui),
            Err(FrameworkError::UnknownDeque(FrameworkDequeId(999)))
        ));
        assert!(ui.root_handle().is_none());
    }
}

#[cfg(test)]
mod initial_snapshot_tests {
    use super::*;
    use crate::runtime::InitialElement;
    use std::cell::Cell;

    struct Source {
        reads: Cell<u32>,
    }
    impl InitialSnapshotSource for Source {
        type Error = ();
        fn capture_initial_deques(&mut self) -> Result<Vec<InitialDeque>, Self::Error> {
            self.reads.set(self.reads.get() + 1);
            Ok(vec![InitialDeque {
                handle: DequeHandle(70_001),
                elements: vec![InitialElement {
                    handle: ElementHandle(80_001),
                    element: super::super::ui::UiElement::new("root"),
                    subtree_span: 1,
                }],
            }])
        }
    }

    #[test]
    fn initial_ui_snapshot_is_captured_once_then_owned_by_the_deque_tree() {
        let mut source = Source {
            reads: Cell::new(0),
        };
        let ui = bootstrap_from_snapshot(&mut source).unwrap();
        assert_eq!(source.reads.get(), 1);
        assert_eq!(ui.tree().len(), 1);
        assert_eq!(ui.registry().len(), 1);
        assert!(ui.topology_is_sealed());
    }
}
