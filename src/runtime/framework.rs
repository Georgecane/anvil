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
        let native = ui.batch_state();
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
        self.validate(&commit, ui)?;
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
                    let removed = ui.remove_subtree(self.elements[&anchor])?;
                    self.remove_native_elements(&removed);
                }
                FrameworkOperation::MoveSubtree {
                    anchor,
                    target,
                    target_end,
                } => {
                    ui.move_subtree(self.elements[&anchor], self.deques[&target], target_end)?;
                }
            }
        }
        Ok(())
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
