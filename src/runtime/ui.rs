use crate::deque_tree::{
    ChildSide, Deque, DequeRegistry, DequeTree, End, NodeId, RenderFrame, TreeError,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

static NEXT_DEQUE_HANDLE: AtomicU32 = AtomicU32::new(1);
static NEXT_ELEMENT_HANDLE: AtomicU64 = AtomicU64::new(1);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DequeHandle(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementHandle(pub u64);
fn alloc_deque_handle() -> DequeHandle {
    DequeHandle(NEXT_DEQUE_HANDLE.fetch_add(1, Ordering::Relaxed))
}
fn alloc_element_handle() -> ElementHandle {
    ElementHandle(NEXT_ELEMENT_HANDLE.fetch_add(1, Ordering::Relaxed))
}
fn reserve_deque_handle(handle: DequeHandle) {
    NEXT_DEQUE_HANDLE.fetch_max(handle.0.saturating_add(1), Ordering::Relaxed);
}
fn reserve_element_handle(handle: ElementHandle) {
    NEXT_ELEMENT_HANDLE.fetch_max(handle.0.saturating_add(1), Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiElementKind {
    Element,
    Text,
}
#[derive(Debug, Clone)]
pub struct UiElement {
    pub kind: UiElementKind,
    pub tag: String,
    pub text_content: Option<String>,
    pub properties: HashMap<String, String>,
    pub styles: HashMap<String, String>,
    pub computed_styles: BTreeMap<String, String>,
}
fn normalize_style_name(name: String) -> String {
    if name.starts_with("--") {
        name
    } else {
        name.to_ascii_lowercase()
    }
}

fn normalize_ui_element(mut element: UiElement) -> UiElement {
    if !element.is_text() {
        element.tag.make_ascii_lowercase();
        element.properties = element
            .properties
            .into_iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value))
            .collect();
        element.styles = element
            .styles
            .into_iter()
            .map(|(name, value)| (normalize_style_name(name), value))
            .collect();
    }
    element
}

impl UiElement {
    pub fn new(tag: impl Into<String>) -> Self {
        let mut tag = tag.into();
        tag.make_ascii_lowercase();
        Self {
            kind: UiElementKind::Element,
            tag,
            text_content: None,
            properties: HashMap::new(),
            styles: HashMap::new(),
            computed_styles: BTreeMap::new(),
        }
    }
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            kind: UiElementKind::Text,
            tag: "#text".into(),
            text_content: Some(content.into()),
            properties: HashMap::new(),
            styles: HashMap::new(),
            computed_styles: BTreeMap::new(),
        }
    }
    pub fn is_text(&self) -> bool {
        self.kind == UiElementKind::Text
    }
    pub fn text_content(&self) -> Option<&str> {
        self.text_content.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiError {
    RootExists,
    UnknownDeque(DequeHandle),
    UnknownElement(ElementHandle),
    DuplicateDeque(DequeHandle),
    DuplicateElement(ElementHandle),
    InvalidSide,
    NotText(ElementHandle),
    InvalidRange,
    SubtreeSpansMultipleDeques(ElementHandle),
    Tree(TreeError),
    TopologySealed,
}
impl From<TreeError> for UiError {
    fn from(value: TreeError) -> Self {
        match value {
            TreeError::TopologySealed => Self::TopologySealed,
            other => Self::Tree(other),
        }
    }
}

/// One element in the initial reverse-preorder Deque snapshot.
pub struct InitialElement {
    pub handle: ElementHandle,
    pub element: UiElement,
    /// Number of consecutive entries ending here that belong to this element's subtree.
    pub subtree_span: usize,
}

/// One Deque's worth of the one-time initial UI snapshot.
/// Elements are supplied in final Front-to-Back (reverse-preorder) order.
pub struct InitialDeque {
    pub handle: DequeHandle,
    pub elements: Vec<InitialElement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiMutationKind {
    Text,
    Attribute,
    InlineStyle,
    Insert,
    Remove,
    Move,
}

#[derive(Debug, Clone)]
pub(crate) struct UiMutation {
    pub revision: u64,
    pub kind: UiMutationKind,
    pub element: Option<ElementHandle>,
    pub affected: Vec<ElementHandle>,
    pub old_parent: Option<ElementHandle>,
    pub old_siblings: Vec<ElementHandle>,
    pub new_parent: Option<ElementHandle>,
    pub new_siblings: Vec<ElementHandle>,
    pub checked_deques: Vec<NodeId>,
    pub hash_changed_deques: Vec<NodeId>,
}

pub(crate) struct UiBatchState {
    pub deques: HashMap<DequeHandle, VecDeque<ElementHandle>>,
    pub subtree_spans: HashMap<DequeHandle, VecDeque<usize>>,
    pub elements: HashMap<ElementHandle, (DequeHandle, bool)>,
    pub sides: HashMap<DequeHandle, (bool, bool)>,
    pub children: HashMap<DequeHandle, [Option<DequeHandle>; 2]>,
    pub root: Option<DequeHandle>,
    pub sealed: bool,
}

#[derive(Clone, Copy)]
struct ElementLocation {
    deque: NodeId,
    entry: u64,
}

/// Read-only, non-owning logical view over the one canonical DequeTree.
/// It stores no element hierarchy and owns no UI nodes.
#[derive(Clone, Copy)]
pub struct LogicalProjection<'a> {
    model: &'a UiModel,
    order: &'a [NodeId],
}
impl<'a> LogicalProjection<'a> {
    fn new(model: &'a UiModel) -> Self {
        Self {
            model,
            order: model.registry.render_stack().as_slice(),
        }
    }
    pub fn deque_order(&self) -> &[NodeId] {
        self.order
    }
    pub(crate) fn render_stack_snapshot(&self) -> crate::deque_tree::RenderStack {
        self.model.registry.render_stack().clone()
    }
    pub(crate) fn handle_at_deque_position(
        &self,
        deque_id: NodeId,
        logical_position: usize,
    ) -> Option<ElementHandle> {
        let node = self.model.tree.get(deque_id)?;
        let deque = node.deque();
        let physical_index = deque.len().checked_sub(logical_position + 1)?;
        let entry = deque.entry_id_at(physical_index)?;
        self.model.element_handle_for_entry(deque_id, entry)
    }
    pub(crate) fn deque_element_count(&self, deque_id: NodeId) -> usize {
        self.model
            .tree
            .get(deque_id)
            .map_or(0, |node| node.deque().len())
    }
    pub(crate) fn mutation_revision(&self) -> u64 {
        self.model.mutation_revision
    }
    pub(crate) fn mutations_since(&self, revision: u64) -> impl Iterator<Item = &UiMutation> {
        self.model
            .mutation_journal
            .iter()
            .filter(move |mutation| mutation.revision > revision)
    }
    pub fn element(&self, handle: ElementHandle) -> Option<&UiElement> {
        self.model.element(handle)
    }
    pub fn root_element(&self) -> Option<ElementHandle> {
        self.root_elements().into_iter().next()
    }
    pub fn root_elements(&self) -> Vec<ElementHandle> {
        self.model.root_element_handles()
    }
    pub fn handles(&self) -> Vec<ElementHandle> {
        let mut handles = Vec::new();
        self.visit(|_, handle, _| handles.push(handle));
        handles
    }
    pub(crate) fn handles_in_deque(&self, deque_id: NodeId) -> Vec<ElementHandle> {
        let Some(node) = self.model.tree.get(deque_id) else {
            return Vec::new();
        };
        node.deque()
            .ids()
            .filter_map(|entry| self.model.element_handle_for_entry(deque_id, entry))
            .collect()
    }
    pub fn siblings_of(&self, handle: ElementHandle) -> Vec<ElementHandle> {
        self.parent_of(handle)
            .map(|parent| self.children_of(parent))
            .unwrap_or_else(|| self.root_elements())
    }
    pub fn visit(&self, mut f: impl FnMut(NodeId, ElementHandle, &UiElement)) {
        for id in self.order {
            if let Some(node) = self.model.tree.get(*id) {
                for (entry_id, element) in node.deque().ids().rev().zip(node.deque().iter().rev()) {
                    if let Some(handle) = self.model.element_handle_for_entry(*id, entry_id) {
                        f(*id, handle, element);
                    }
                }
            }
        }
    }
    pub fn parent_of(&self, handle: ElementHandle) -> Option<ElementHandle> {
        self.model.logical_parent(handle)
    }
    pub fn children_of(&self, handle: ElementHandle) -> Vec<ElementHandle> {
        self.model.logical_children(handle)
    }
    pub fn are_siblings(&self, left: ElementHandle, right: ElementHandle) -> bool {
        self.model.are_logical_siblings(left, right)
    }
}

/// The sole owner of UI elements. Framework adapters and JS bindings hold only handles.
pub struct UiModel {
    tree: DequeTree<UiElement>,
    registry: DequeRegistry<UiElement>,
    deque_handles: HashMap<DequeHandle, NodeId>,
    element_handles: HashMap<ElementHandle, ElementLocation>,
    entry_handles: HashMap<(NodeId, u64), ElementHandle>,
    root_handle: Option<DequeHandle>,
    topology_sealed: bool,
    mutation_revision: u64,
    acknowledged_mutation_revision: u64,
    mutation_journal: VecDeque<UiMutation>,
}
impl Default for UiModel {
    fn default() -> Self {
        Self::new()
    }
}
impl UiModel {
    pub fn from_initial_groups(groups: Vec<InitialDeque>) -> Result<Self, UiError> {
        if groups.is_empty() {
            return Err(UiError::Tree(TreeError::EmptyTopology));
        }
        let root_handle = groups.first().map(|group| group.handle);
        let total_elements = groups.iter().map(|group| group.elements.len()).sum();
        let mut deques = Vec::with_capacity(groups.len());
        let mut per_deque_handles: Vec<Vec<(ElementHandle, u64)>> =
            Vec::with_capacity(groups.len());
        for group in &groups {
            reserve_deque_handle(group.handle);
            let (deque, entries) =
                Deque::from_ordered_spans(group.elements.iter().map(|initial| {
                    (
                        normalize_ui_element(initial.element.clone()),
                        initial.subtree_span,
                    )
                }))
                .ok_or(UiError::InvalidRange)?;
            let handles: Vec<_> = group
                .elements
                .iter()
                .zip(entries)
                .map(|(initial, entry)| {
                    reserve_element_handle(initial.handle);
                    (initial.handle, entry)
                })
                .collect();
            // This validates every interval and guarantees the final entry's
            // span covers the complete Deque, so a second per-entry parent scan
            // would only repeat work already proven by the interval validator.
            if !deque.validate_structure() {
                return Err(UiError::InvalidRange);
            }
            deques.push(deque);
            per_deque_handles.push(handles);
        }
        let mut tree = DequeTree::from_level_order(deques)?;
        let mut deque_handles = HashMap::with_capacity(groups.len());
        let mut element_handles = HashMap::with_capacity(total_elements);
        let mut entry_handles = HashMap::with_capacity(total_elements);
        for ((group, handles), (node_id, _)) in
            groups.into_iter().zip(per_deque_handles).zip(tree.nodes())
        {
            if deque_handles.insert(group.handle, node_id).is_some() {
                return Err(UiError::DuplicateDeque(group.handle));
            }
            for (handle, entry) in handles {
                if element_handles
                    .insert(
                        handle,
                        ElementLocation {
                            deque: node_id,
                            entry,
                        },
                    )
                    .is_some()
                {
                    return Err(UiError::DuplicateElement(handle));
                }
                entry_handles.insert((node_id, entry), handle);
            }
        }
        let registry = DequeRegistry::build(&mut tree);
        Ok(Self {
            tree,
            registry,
            deque_handles,
            element_handles,
            entry_handles,
            root_handle,
            topology_sealed: true,
            mutation_revision: 0,
            acknowledged_mutation_revision: 0,
            mutation_journal: VecDeque::new(),
        })
    }

    pub fn new() -> Self {
        Self {
            tree: DequeTree::new(),
            registry: DequeRegistry::new(),
            deque_handles: HashMap::new(),
            element_handles: HashMap::new(),
            entry_handles: HashMap::new(),
            root_handle: None,
            topology_sealed: false,
            mutation_revision: 0,
            acknowledged_mutation_revision: 0,
            mutation_journal: VecDeque::new(),
        }
    }
    pub fn allocate_deque_handle() -> DequeHandle {
        alloc_deque_handle()
    }
    pub fn allocate_element_handle() -> ElementHandle {
        alloc_element_handle()
    }
    pub fn tree(&self) -> &DequeTree<UiElement> {
        &self.tree
    }
    pub fn root_handle(&self) -> Option<DequeHandle> {
        self.root_handle
    }
    pub fn seal_initial_topology(&mut self) {
        if self.topology_sealed || self.root_handle.is_none() {
            return;
        }
        self.tree.seal_topology();
        self.topology_sealed = true;
        self.registry = DequeRegistry::build(&mut self.tree);
    }
    pub fn topology_is_sealed(&self) -> bool {
        self.topology_sealed
    }
    pub(crate) fn mutation_revision(&self) -> u64 {
        self.mutation_revision
    }

    fn mutation_context(
        &self,
        handle: ElementHandle,
    ) -> (Option<ElementHandle>, Vec<ElementHandle>) {
        let parent = self.logical_parent(handle);
        let siblings = parent
            .map(|parent| self.logical_children(parent))
            .unwrap_or_else(|| self.root_element_handles());
        (parent, siblings)
    }

    /// Non-structural changes need the parent identity, not a sibling snapshot.
    /// Insert, remove, and move operations continue to capture sibling context.
    fn mutation_parent_context(
        &self,
        handle: ElementHandle,
    ) -> (Option<ElementHandle>, Vec<ElementHandle>) {
        (self.logical_parent(handle), Vec::new())
    }

    fn root_element_handles(&self) -> Vec<ElementHandle> {
        let Some(node_id) = self
            .root_handle
            .and_then(|handle| self.deque_handles.get(&handle))
        else {
            return Vec::new();
        };
        let Some(node) = self.tree.get(*node_id) else {
            return Vec::new();
        };
        if let Some(entry) = node.deque().ids().next_back() {
            return self
                .element_handle_for_entry(*node_id, entry)
                .into_iter()
                .collect();
        }

        let mut roots = Vec::new();
        for child in [node.left(), node.right()].into_iter().flatten() {
            self.append_visible_branch_roots(child, &mut roots);
        }
        roots
    }

    fn append_visible_branch_roots(&self, start: NodeId, output: &mut Vec<ElementHandle>) {
        let mut pending = vec![start];
        while let Some(node_id) = pending.pop() {
            let Some(node) = self.tree.get(node_id) else {
                continue;
            };
            if let Some(entry) = node.deque().ids().next_back() {
                if let Some(handle) = self.element_handle_for_entry(node_id, entry) {
                    output.push(handle);
                }
            } else {
                if let Some(right) = node.right() {
                    pending.push(right);
                }
                if let Some(left) = node.left() {
                    pending.push(left);
                }
            }
        }
    }

    fn record_mutation(
        &mut self,
        kind: UiMutationKind,
        element: Option<ElementHandle>,
        affected: Vec<ElementHandle>,
        old_context: (Option<ElementHandle>, Vec<ElementHandle>),
        new_context: (Option<ElementHandle>, Vec<ElementHandle>),
        checked_deques: Vec<NodeId>,
    ) {
        let mut checked_deques = checked_deques;
        checked_deques.sort_unstable();
        checked_deques.dedup();
        let hash_changed_deques = if self.topology_sealed {
            self.registry
                .detect_changes_for(&mut self.tree, checked_deques.iter().copied())
        } else {
            checked_deques.clear();
            Vec::new()
        };
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        self.mutation_journal.push_back(UiMutation {
            revision: self.mutation_revision,
            kind,
            element,
            affected,
            old_parent: old_context.0,
            old_siblings: old_context.1,
            new_parent: new_context.0,
            new_siblings: new_context.1,
            checked_deques,
            hash_changed_deques,
        });
    }

    pub(crate) fn acknowledge_mutations(&mut self, revision: u64) {
        self.acknowledged_mutation_revision = self.acknowledged_mutation_revision.max(revision);
        while self
            .mutation_journal
            .front()
            .is_some_and(|mutation| mutation.revision <= self.acknowledged_mutation_revision)
        {
            self.mutation_journal.pop_front();
        }
    }

    pub fn create_root_deque(&mut self) -> Result<DequeHandle, UiError> {
        let handle = alloc_deque_handle();
        self.create_root_deque_with_handle(handle)?;
        Ok(handle)
    }
    pub fn create_root_deque_with_handle(&mut self, handle: DequeHandle) -> Result<(), UiError> {
        reserve_deque_handle(handle);
        if self.root_handle.is_some() {
            return Err(UiError::RootExists);
        }
        if self.deque_handles.contains_key(&handle) {
            return Err(UiError::DuplicateDeque(handle));
        }
        let id = self.tree.insert_root(Deque::new())?;
        self.root_handle = Some(handle);
        self.deque_handles.insert(handle, id);
        Ok(())
    }
    pub fn create_child_deque(
        &mut self,
        parent: DequeHandle,
        side: ChildSide,
    ) -> Result<DequeHandle, UiError> {
        let handle = alloc_deque_handle();
        self.create_child_deque_with_handle(parent, handle, side)?;
        Ok(handle)
    }
    pub fn create_child_deque_with_handle(
        &mut self,
        parent: DequeHandle,
        handle: DequeHandle,
        side: ChildSide,
    ) -> Result<(), UiError> {
        reserve_deque_handle(handle);
        if self.topology_sealed {
            return Err(UiError::TopologySealed);
        }
        if self.deque_handles.contains_key(&handle) {
            return Err(UiError::DuplicateDeque(handle));
        }
        let parent_id = *self
            .deque_handles
            .get(&parent)
            .ok_or(UiError::UnknownDeque(parent))?;
        let id = match side {
            ChildSide::Left => self.tree.insert_left(parent_id, Deque::new())?,
            ChildSide::Right => self.tree.insert_right(parent_id, Deque::new())?,
        };
        self.deque_handles.insert(handle, id);
        Ok(())
    }
    pub fn deque_id(&self, handle: DequeHandle) -> Option<NodeId> {
        self.deque_handles.get(&handle).copied()
    }

    pub fn create_element(
        &mut self,
        deque: DequeHandle,
        tag: impl Into<String>,
        end: End,
    ) -> Result<ElementHandle, UiError> {
        let handle = alloc_element_handle();
        self.create_element_with_handle(deque, handle, UiElement::new(tag), end)?;
        Ok(handle)
    }
    pub fn create_text(
        &mut self,
        deque: DequeHandle,
        text: impl Into<String>,
        end: End,
    ) -> Result<ElementHandle, UiError> {
        let handle = alloc_element_handle();
        self.create_element_with_handle(deque, handle, UiElement::text(text), end)?;
        Ok(handle)
    }
    pub fn create_element_with_handle(
        &mut self,
        deque: DequeHandle,
        handle: ElementHandle,
        element: UiElement,
        end: End,
    ) -> Result<(), UiError> {
        reserve_element_handle(handle);
        if self.element_handles.contains_key(&handle) {
            return Err(UiError::DuplicateElement(handle));
        }
        let node_id = *self
            .deque_handles
            .get(&deque)
            .ok_or(UiError::UnknownDeque(deque))?;
        let element = normalize_ui_element(element);
        let was_ghost = self
            .tree
            .get(node_id)
            .ok_or(UiError::UnknownDeque(deque))?
            .deque()
            .is_ghost();
        let node = self
            .tree
            .get_mut(node_id)
            .ok_or(UiError::UnknownDeque(deque))?;
        let entry = match end {
            End::Front => node.deque_mut().push_front(element),
            End::Back => node.deque_mut().push_back(element),
        };
        self.element_handles.insert(
            handle,
            ElementLocation {
                deque: node_id,
                entry,
            },
        );
        self.entry_handles.insert((node_id, entry), handle);
        if self.topology_sealed && was_ghost {
            self.registry.refresh_render_stack(&self.tree);
        }
        let new_context = self.mutation_context(handle);
        self.record_mutation(
            UiMutationKind::Insert,
            Some(handle),
            vec![handle],
            (None, Vec::new()),
            new_context,
            vec![node_id],
        );
        Ok(())
    }
    pub fn element(&self, handle: ElementHandle) -> Option<&UiElement> {
        let loc = self.element_handles.get(&handle)?;
        self.tree.get(loc.deque)?.deque().get_by_id(loc.entry)
    }
    fn element_mut(&mut self, handle: ElementHandle) -> Option<&mut UiElement> {
        let loc = *self.element_handles.get(&handle)?;
        self.tree
            .get_mut(loc.deque)?
            .deque_mut()
            .get_mut_by_id(loc.entry)
    }
    pub fn set_text(
        &mut self,
        handle: ElementHandle,
        text: impl Into<String>,
    ) -> Result<(), UiError> {
        let text = text.into();
        let location = *self
            .element_handles
            .get(&handle)
            .ok_or(UiError::UnknownElement(handle))?;
        let element = self
            .element_mut(handle)
            .ok_or(UiError::UnknownElement(handle))?;
        if !element.is_text() {
            return Err(UiError::NotText(handle));
        }
        if element.text_content.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        element.text_content = Some(text);
        let context = self.mutation_parent_context(handle);
        self.record_mutation(
            UiMutationKind::Text,
            Some(handle),
            vec![handle],
            context.clone(),
            context,
            vec![location.deque],
        );
        Ok(())
    }

    pub fn set_property(
        &mut self,
        handle: ElementHandle,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), UiError> {
        let key = key.into().to_ascii_lowercase();
        let value = value.into();
        let location = *self
            .element_handles
            .get(&handle)
            .ok_or(UiError::UnknownElement(handle))?;
        let element = self
            .element_mut(handle)
            .ok_or(UiError::UnknownElement(handle))?;
        if element.properties.get(&key) == Some(&value) {
            return Ok(());
        }
        element.properties.insert(key, value);
        let context = self.mutation_parent_context(handle);
        self.record_mutation(
            UiMutationKind::Attribute,
            Some(handle),
            vec![handle],
            context.clone(),
            context,
            vec![location.deque],
        );
        Ok(())
    }

    pub fn remove_property(&mut self, handle: ElementHandle, key: &str) -> Result<(), UiError> {
        let key = key.to_ascii_lowercase();
        let location = *self
            .element_handles
            .get(&handle)
            .ok_or(UiError::UnknownElement(handle))?;
        let removed = self
            .element_mut(handle)
            .ok_or(UiError::UnknownElement(handle))?
            .properties
            .remove(&key);
        if removed.is_some() {
            let context = self.mutation_parent_context(handle);
            self.record_mutation(
                UiMutationKind::Attribute,
                Some(handle),
                vec![handle],
                context.clone(),
                context,
                vec![location.deque],
            );
        }
        Ok(())
    }

    pub fn set_style(
        &mut self,
        handle: ElementHandle,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), UiError> {
        let key = normalize_style_name(key.into());
        let value = value.into();
        let location = *self
            .element_handles
            .get(&handle)
            .ok_or(UiError::UnknownElement(handle))?;
        let element = self
            .element_mut(handle)
            .ok_or(UiError::UnknownElement(handle))?;
        if element.styles.get(&key) == Some(&value) {
            return Ok(());
        }
        element.styles.insert(key, value);
        let context = self.mutation_parent_context(handle);
        self.record_mutation(
            UiMutationKind::InlineStyle,
            Some(handle),
            vec![handle],
            context.clone(),
            context,
            vec![location.deque],
        );
        Ok(())
    }

    pub fn remove_style(&mut self, handle: ElementHandle, key: &str) -> Result<(), UiError> {
        let key = normalize_style_name(key.to_owned());
        let location = *self
            .element_handles
            .get(&handle)
            .ok_or(UiError::UnknownElement(handle))?;
        let removed = self
            .element_mut(handle)
            .ok_or(UiError::UnknownElement(handle))?
            .styles
            .remove(&key);
        if removed.is_some() {
            let context = self.mutation_parent_context(handle);
            self.record_mutation(
                UiMutationKind::InlineStyle,
                Some(handle),
                vec![handle],
                context.clone(),
                context,
                vec![location.deque],
            );
        }
        Ok(())
    }

    pub fn preview_subtree_handles(
        &self,
        anchor: ElementHandle,
    ) -> Result<Vec<ElementHandle>, UiError> {
        let loc = *self
            .element_handles
            .get(&anchor)
            .ok_or(UiError::UnknownElement(anchor))?;
        let deque = self
            .tree
            .get(loc.deque)
            .ok_or(UiError::UnknownElement(anchor))?
            .deque();
        let range = deque
            .structural_range(loc.entry)
            .ok_or(UiError::UnknownElement(anchor))?;
        let mut handles = Vec::with_capacity(range.count);
        for entry in deque.ids_in_range(range.start, range.count) {
            let handle = self
                .entry_handles
                .get(&(loc.deque, entry))
                .copied()
                .ok_or(UiError::InvalidRange)?;
            handles.push(handle);
        }
        if handles.len() != range.count {
            return Err(UiError::InvalidRange);
        }
        Ok(handles)
    }

    /// Removes the anchor and its complete subtree, inferred from the parallel range boundaries.
    pub fn remove_subtree(&mut self, anchor: ElementHandle) -> Result<Vec<ElementHandle>, UiError> {
        let loc = *self
            .element_handles
            .get(&anchor)
            .ok_or(UiError::UnknownElement(anchor))?;
        let old_context = self.mutation_context(anchor);
        let is_deque_root = self
            .tree
            .get(loc.deque)
            .ok_or(UiError::UnknownElement(anchor))?
            .deque()
            .ids()
            .next_back()
            == Some(loc.entry);

        let mut handles = Vec::new();
        let mut checked_deques = vec![loc.deque];
        let render_stack_changed;
        if is_deque_root {
            let (cleared, changed) = self.tree.clear_subtree_contents(loc.deque)?;
            checked_deques = cleared.iter().map(|item| item.node).collect();
            render_stack_changed = changed;
            for cleared_deque in cleared {
                for entry_id in cleared_deque.entries.into_iter().rev() {
                    if let Some(handle) = self.entry_handles.remove(&(cleared_deque.node, entry_id))
                    {
                        self.element_handles.remove(&handle);
                        handles.push(handle);
                    }
                }
            }
        } else {
            let removed = self
                .tree
                .get_mut(loc.deque)
                .ok_or(UiError::UnknownElement(anchor))?
                .deque_mut()
                .drain_subtree(loc.entry)
                .ok_or(UiError::UnknownElement(anchor))?;
            for (entry_id, _) in removed.into_iter().rev() {
                if let Some(handle) = self.entry_handles.remove(&(loc.deque, entry_id)) {
                    self.element_handles.remove(&handle);
                    handles.push(handle);
                }
            }
            render_stack_changed = self.tree.reconcile_ghosts_for(loc.deque);
        }
        if render_stack_changed {
            self.registry.refresh_render_stack(&self.tree);
        }
        self.record_mutation(
            UiMutationKind::Remove,
            Some(anchor),
            handles.clone(),
            old_context,
            (None, Vec::new()),
            checked_deques,
        );
        Ok(handles)
    }

    pub fn remove_element(&mut self, handle: ElementHandle) -> Result<(), UiError> {
        self.remove_subtree(handle).map(|_| ())
    }

    /// Moves an anchor's complete subtree. Its range and source end are inferred
    /// from the Deque's structural boundaries, not supplied by the caller.
    pub fn move_subtree(
        &mut self,
        anchor: ElementHandle,
        target: DequeHandle,
        target_end: End,
    ) -> Result<(), UiError> {
        let loc = *self
            .element_handles
            .get(&anchor)
            .ok_or(UiError::UnknownElement(anchor))?;
        let target_id = *self
            .deque_handles
            .get(&target)
            .ok_or(UiError::UnknownDeque(target))?;
        if loc.deque == target_id {
            return Err(UiError::InvalidRange);
        }
        let old_context = self.mutation_context(anchor);
        let affected = self.preview_subtree_handles(anchor)?;
        let source_node = self
            .tree
            .get(loc.deque)
            .ok_or(UiError::UnknownElement(anchor))?;
        if source_node.deque().ids().next_back() == Some(loc.entry)
            && (source_node.left().is_some() || source_node.right().is_some())
        {
            return Err(UiError::SubtreeSpansMultipleDeques(anchor));
        }
        let target_was_ghost = self
            .tree
            .get(target_id)
            .ok_or(UiError::UnknownDeque(target))?
            .deque()
            .is_ghost();
        self.tree
            .transfer_subtree(loc.deque, target_id, loc.entry, target_end)?;
        // `affected` is already the exact moved handle set. Update both indexes
        // through direct HashMap lookups instead of scanning every live element
        // and testing membership in a second O(k) vector.
        for handle in &affected {
            let entry = self
                .element_handles
                .get(handle)
                .ok_or(UiError::UnknownElement(*handle))?
                .entry;
            self.entry_handles.remove(&(loc.deque, entry));
            self.entry_handles.insert((target_id, entry), *handle);
            if let Some(location) = self.element_handles.get_mut(handle) {
                location.deque = target_id;
            }
        }
        let source_became_ghost = self.tree.reconcile_ghosts_for(loc.deque);
        if target_was_ghost || source_became_ghost {
            self.registry.refresh_render_stack(&self.tree);
        }
        let new_context = self.mutation_context(anchor);
        self.record_mutation(
            UiMutationKind::Move,
            Some(anchor),
            affected,
            old_context,
            new_context,
            vec![loc.deque, target_id],
        );
        Ok(())
    }

    pub fn replace_computed_styles(
        &mut self,
        handle: ElementHandle,
        styles: BTreeMap<String, String>,
    ) -> Result<(), UiError> {
        self.element_mut(handle)
            .ok_or(UiError::UnknownElement(handle))?
            .computed_styles = styles;
        Ok(())
    }
    pub fn computed_styles(&self, handle: ElementHandle) -> Option<&BTreeMap<String, String>> {
        Some(&self.element(handle)?.computed_styles)
    }
    pub fn projection(&mut self) -> LogicalProjection<'_> {
        if self.root_handle.is_some() && !self.topology_sealed {
            self.seal_initial_topology();
        }
        LogicalProjection::new(self)
    }
    pub fn element_handle_for_entry(&self, deque: NodeId, entry: u64) -> Option<ElementHandle> {
        self.entry_handles.get(&(deque, entry)).copied()
    }
    pub fn prepare_render(&mut self) -> RenderFrame {
        if self.root_handle.is_some() && !self.topology_sealed {
            self.seal_initial_topology();
        }
        self.registry.prepare_render_pending()
    }
    pub(crate) fn batch_state(&self) -> UiBatchState {
        let mut deques = HashMap::new();
        let mut subtree_spans = HashMap::new();
        let mut sides = HashMap::new();
        let mut children = HashMap::new();
        let mut elements = HashMap::new();
        let reverse_deques: HashMap<_, _> = self
            .deque_handles
            .iter()
            .map(|(handle, node)| (*node, *handle))
            .collect();
        for (handle, node_id) in &self.deque_handles {
            if let Some(node) = self.tree.get(*node_id) {
                let mut contents = VecDeque::new();
                for entry in node.deque().ids() {
                    if let Some(element_handle) =
                        self.entry_handles.get(&(*node_id, entry)).copied()
                    {
                        let is_text = node
                            .deque()
                            .get_by_id(entry)
                            .is_some_and(UiElement::is_text);
                        contents.push_back(element_handle);
                        elements.insert(element_handle, (*handle, is_text));
                    }
                }
                deques.insert(*handle, contents);
                subtree_spans.insert(*handle, node.deque().subtree_spans().collect());
                sides.insert(*handle, (node.left().is_some(), node.right().is_some()));
                children.insert(
                    *handle,
                    [
                        node.left()
                            .and_then(|child| reverse_deques.get(&child).copied()),
                        node.right()
                            .and_then(|child| reverse_deques.get(&child).copied()),
                    ],
                );
            }
        }
        UiBatchState {
            deques,
            subtree_spans,
            elements,
            sides,
            children,
            root: self.root_handle,
            sealed: self.topology_sealed,
        }
    }

    pub fn logical_parent(&self, handle: ElementHandle) -> Option<ElementHandle> {
        let location = self.element_handles.get(&handle)?;
        let node = self.tree.get(location.deque)?;
        if let Some(parent_entry) = node.deque().logical_parent(location.entry) {
            return self.element_handle_for_entry(location.deque, parent_entry);
        }
        // A DequeTree child branch attaches to the root element (Back) of its
        // parent Deque. The binary links alone are not UI relationships; this
        // root-boundary convention supplies the missing logical anchor.
        let mut ancestor = node.parent();
        while let Some(parent_deque) = ancestor {
            let parent_node = self.tree.get(parent_deque)?;
            if let Some(parent_root) = parent_node.deque().ids().next_back() {
                return self.element_handle_for_entry(parent_deque, parent_root);
            }
            ancestor = parent_node.parent();
        }
        None
    }

    pub fn logical_children(&self, handle: ElementHandle) -> Vec<ElementHandle> {
        let Some(location) = self.element_handles.get(&handle) else {
            return Vec::new();
        };
        let Some(node) = self.tree.get(location.deque) else {
            return Vec::new();
        };
        let mut children: Vec<_> = node
            .deque()
            .logical_children(location.entry)
            .into_iter()
            .filter_map(|entry| self.element_handle_for_entry(location.deque, entry))
            .collect();
        if node.deque().ids().next_back() == Some(location.entry) {
            for child_deque in [node.left(), node.right()].into_iter().flatten() {
                self.append_visible_branch_roots(child_deque, &mut children);
            }
        }
        children
    }

    pub fn are_logical_siblings(&self, left: ElementHandle, right: ElementHandle) -> bool {
        if !self.element_handles.contains_key(&left) || !self.element_handles.contains_key(&right) {
            return false;
        }
        let left_parent = self.logical_parent(left);
        if left_parent != self.logical_parent(right) {
            return false;
        }
        if left_parent.is_some() {
            true
        } else {
            let roots = self.root_element_handles();
            roots.contains(&left) && roots.contains(&right)
        }
    }

    pub fn registry(&self) -> &DequeRegistry<UiElement> {
        &self.registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_parent_from_back_removes_its_complete_descendant_range() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let child = ui.create_element(root, "child", End::Front).unwrap();
        let parent = ui.create_element(root, "parent", End::Back).unwrap();
        assert_eq!(
            ui.tree()
                .get(ui.deque_id(root).unwrap())
                .unwrap()
                .deque()
                .len(),
            2
        );
        let removed = ui.remove_subtree(parent).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&parent));
        assert!(removed.contains(&child));
        assert!(ui.element(parent).is_none());
        assert!(ui.element(child).is_none());
    }

    #[test]
    fn preview_subtree_handles_returns_only_the_indexed_structural_range() {
        let mut ui = UiModel::new();
        let root_deque = ui.create_root_deque().unwrap();
        let leaf = ui.create_element(root_deque, "leaf", End::Back).unwrap();
        let branch = ui.create_element(root_deque, "branch", End::Back).unwrap();
        let root = ui.create_element(root_deque, "root", End::Back).unwrap();

        assert_eq!(
            ui.preview_subtree_handles(branch).unwrap(),
            vec![leaf, branch]
        );
        assert_eq!(
            ui.preview_subtree_handles(root).unwrap(),
            vec![leaf, branch, root]
        );
    }

    #[test]
    fn moving_an_interior_subtree_preserves_identity_and_remaining_siblings() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let left = ui.create_child_deque(root, ChildSide::Left).unwrap();
        let c = ui.create_element(root, "c", End::Back).unwrap();
        let a = ui.create_element(root, "a", End::Front).unwrap();
        let b = ui.create_element(root, "b", End::Front).unwrap();
        ui.move_subtree(a, left, End::Back).unwrap();
        assert!(ui.element(a).is_some());
        assert!(ui.element(b).is_some());
        let source = ui.tree().get(ui.deque_id(root).unwrap()).unwrap().deque();
        let target = ui.tree().get(ui.deque_id(left).unwrap()).unwrap().deque();
        assert_eq!(
            source.ids().collect::<Vec<_>>(),
            vec![ui.element_handles[&b].entry, ui.element_handles[&c].entry]
        );
        assert_eq!(
            target.iter().map(|e| e.tag.as_str()).collect::<Vec<_>>(),
            vec!["a"]
        );
    }

    #[test]
    fn interior_removal_automatically_removes_the_descendant_prefix() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let first = ui.create_element(root, "first", End::Back).unwrap();
        let middle = ui.create_element(root, "middle", End::Back).unwrap();
        let last = ui.create_element(root, "last", End::Back).unwrap();
        ui.remove_element(middle).unwrap();
        assert!(ui.element(first).is_none());
        assert!(ui.element(middle).is_none());
        assert!(ui.element(last).is_some());
    }

    #[test]
    fn sealing_without_a_root_does_not_lock_out_initial_construction() {
        let mut ui = UiModel::new();
        ui.seal_initial_topology();
        assert!(!ui.topology_is_sealed());

        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();
        assert!(ui.root_handle().is_some());
    }

    #[test]
    fn sealing_initial_topology_twice_preserves_pending_render_changes() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();
        ui.seal_initial_topology();
        let changed = ui.create_element(root, "section", End::Front).unwrap();
        let root_id = ui.deque_id(root).unwrap();

        ui.seal_initial_topology();
        let frame = ui.prepare_render();

        assert!(ui.element(changed).is_some());
        assert!(frame.changed().contains(&root_id));
    }

    #[test]
    fn topology_is_fixed_after_initial_build() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_child_deque(root, ChildSide::Left).unwrap();
        ui.seal_initial_topology();
        assert!(matches!(
            ui.create_child_deque(root, ChildSide::Right),
            Err(UiError::TopologySealed)
        ));
    }
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    #[test]
    fn initial_snapshot_builds_fixed_binary_deque_tree_then_registry() {
        let groups = vec![
            InitialDeque {
                handle: DequeHandle(100),
                elements: vec![InitialElement {
                    handle: ElementHandle(1),
                    element: UiElement::new("root"),
                    subtree_span: 1,
                }],
            },
            InitialDeque {
                handle: DequeHandle(101),
                elements: vec![
                    InitialElement {
                        handle: ElementHandle(2),
                        element: UiElement::new("a"),
                        subtree_span: 1,
                    },
                    InitialElement {
                        handle: ElementHandle(3),
                        element: UiElement::new("b"),
                        subtree_span: 1,
                    },
                    InitialElement {
                        handle: ElementHandle(6),
                        element: UiElement::new("left-parent"),
                        subtree_span: 3,
                    },
                ],
            },
            InitialDeque {
                handle: DequeHandle(102),
                elements: vec![
                    InitialElement {
                        handle: ElementHandle(4),
                        element: UiElement::new("c"),
                        subtree_span: 1,
                    },
                    InitialElement {
                        handle: ElementHandle(5),
                        element: UiElement::new("d"),
                        subtree_span: 1,
                    },
                    InitialElement {
                        handle: ElementHandle(7),
                        element: UiElement::new("right-parent"),
                        subtree_span: 3,
                    },
                ],
            },
        ];
        let ui = UiModel::from_initial_groups(groups).unwrap();
        assert!(ui.topology_is_sealed());
        assert_eq!(ui.tree().len(), 3);
        assert_eq!(ui.registry().len(), 3);
        assert_eq!(ui.root_handle(), Some(DequeHandle(100)));
        assert_eq!(ui.element(ElementHandle(5)).unwrap().tag, "d");
    }
}

#[cfg(test)]
mod non_structural_mutation_context_tests {
    use super::*;

    #[test]
    fn attribute_mutations_record_parent_without_materializing_sibling_snapshots() {
        let mut ui = UiModel::from_initial_groups(vec![InitialDeque {
            handle: DequeHandle(8_001),
            elements: vec![
                InitialElement {
                    handle: ElementHandle(8_101),
                    element: UiElement::new("span"),
                    subtree_span: 1,
                },
                InitialElement {
                    handle: ElementHandle(8_102),
                    element: UiElement::new("span"),
                    subtree_span: 1,
                },
                InitialElement {
                    handle: ElementHandle(8_103),
                    element: UiElement::new("main"),
                    subtree_span: 3,
                },
            ],
        }])
        .unwrap();

        let target = ElementHandle(8_101);
        let parent = ElementHandle(8_103);
        ui.set_property(target, "class", "active").unwrap();

        let mutation = ui.mutation_journal.back().unwrap();
        assert_eq!(mutation.new_parent, Some(parent));
        assert!(mutation.old_siblings.is_empty());
        assert!(mutation.new_siblings.is_empty());
    }
}

#[cfg(test)]
mod projection_order_tests {
    use super::*;
    #[test]
    fn projection_reads_each_deque_back_to_front_parent_before_descendants() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let child = ui.create_element(root, "child", End::Front).unwrap();
        let parent = ui.create_element(root, "parent", End::Back).unwrap();
        let mut tags = Vec::new();
        ui.projection()
            .visit(|_, _, element| tags.push(element.tag.clone()));
        assert_eq!(tags, vec!["parent", "child"]);
        assert!(ui.element(parent).is_some());
        assert!(ui.element(child).is_some());
    }
}

#[cfg(test)]
mod ghost_revival_tests {
    use super::*;
    #[test]
    fn logical_projection_traverses_ghost_branches_to_visible_descendants() {
        let groups = vec![
            InitialDeque {
                handle: DequeHandle(400),
                elements: vec![InitialElement {
                    handle: ElementHandle(4000),
                    element: UiElement::new("main"),
                    subtree_span: 1,
                }],
            },
            InitialDeque {
                handle: DequeHandle(401),
                elements: Vec::new(),
            },
            InitialDeque {
                handle: DequeHandle(402),
                elements: Vec::new(),
            },
            InitialDeque {
                handle: DequeHandle(403),
                elements: vec![InitialElement {
                    handle: ElementHandle(4003),
                    element: UiElement::new("article"),
                    subtree_span: 1,
                }],
            },
            InitialDeque {
                handle: DequeHandle(404),
                elements: vec![InitialElement {
                    handle: ElementHandle(4004),
                    element: UiElement::new("aside"),
                    subtree_span: 1,
                }],
            },
        ];
        let mut ui = UiModel::from_initial_groups(groups).unwrap();
        let projection = ui.projection();
        let main = ElementHandle(4000);
        let article = ElementHandle(4003);
        let aside = ElementHandle(4004);

        assert_eq!(projection.root_elements(), vec![main]);
        assert_eq!(projection.children_of(main), vec![article, aside]);
        assert_eq!(projection.parent_of(article), Some(main));
        assert_eq!(projection.parent_of(aside), Some(main));
        assert!(projection.are_siblings(article, aside));
        assert_eq!(projection.siblings_of(article), vec![article, aside]);
        assert_eq!(
            projection
                .deque_order()
                .iter()
                .map(|id| id.index())
                .collect::<Vec<_>>(),
            vec![0, 3, 4]
        );
    }

    #[test]
    fn a_ghost_root_exposes_all_visible_descendants_as_document_roots() {
        let groups = vec![
            InitialDeque {
                handle: DequeHandle(410),
                elements: Vec::new(),
            },
            InitialDeque {
                handle: DequeHandle(411),
                elements: Vec::new(),
            },
            InitialDeque {
                handle: DequeHandle(412),
                elements: vec![InitialElement {
                    handle: ElementHandle(4012),
                    element: UiElement::new("article"),
                    subtree_span: 1,
                }],
            },
        ];
        let mut ui = UiModel::from_initial_groups(groups).unwrap();
        let projection = ui.projection();
        let article = ElementHandle(4012);

        assert_eq!(projection.root_elements(), vec![article]);
        assert_eq!(projection.parent_of(article), None);
        assert_eq!(projection.siblings_of(article), vec![article]);
    }

    #[test]
    fn adding_content_to_a_ghost_revives_it_in_the_cached_render_stack() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let left = ui.create_child_deque(root, ChildSide::Left).unwrap();
        let right = ui.create_child_deque(root, ChildSide::Right).unwrap();
        ui.create_element(left, "left", End::Back).unwrap();
        ui.create_element(right, "right", End::Back).unwrap();
        ui.prepare_render();
        let left_id = ui.deque_id(left).unwrap();
        let left_element = ui
            .element_handles
            .iter()
            .find_map(|(handle, location)| (location.deque == left_id).then_some(*handle))
            .unwrap();
        ui.remove_element(left_element).unwrap();
        assert!(ui.tree().get(left_id).unwrap().deque().is_ghost());
        assert!(!ui.registry().render_stack().as_slice().contains(&left_id));
        ui.create_element(left, "revived", End::Back).unwrap();
        assert!(!ui.tree().get(left_id).unwrap().deque().is_ghost());
        assert!(ui.registry().render_stack().as_slice().contains(&left_id));
    }

    #[test]
    fn deleting_a_parent_from_a_multi_entry_deque_removes_its_descendants() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let first = ui.create_element(root, "first", End::Back).unwrap();
        let second = ui.create_element(root, "second", End::Back).unwrap();
        ui.remove_element(second).unwrap();
        assert!(ui.element(first).is_none());
        assert!(ui.element(second).is_none());
    }

    #[test]
    fn removing_middle_parent_automatically_removes_its_descendant_prefix() {
        let mut ui = UiModel::from_initial_groups(vec![InitialDeque {
            handle: DequeHandle(9_001),
            elements: vec![
                InitialElement {
                    handle: ElementHandle(9_101),
                    element: UiElement::new("leaf"),
                    subtree_span: 1,
                },
                InitialElement {
                    handle: ElementHandle(9_102),
                    element: UiElement::new("branch"),
                    subtree_span: 2,
                },
                InitialElement {
                    handle: ElementHandle(9_103),
                    element: UiElement::new("root"),
                    subtree_span: 3,
                },
            ],
        }])
        .unwrap();
        let branch = ElementHandle(9_102);
        let leaf = ElementHandle(9_101);
        let root_element = ElementHandle(9_103);
        let removed = ui.remove_subtree(branch).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&branch));
        assert!(removed.contains(&leaf));
        assert!(ui.element(root_element).is_some());
    }

    #[test]
    fn projection_combines_deque_spans_with_root_anchored_deque_branches() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let left = ui.create_child_deque(root, ChildSide::Left).unwrap();
        let right = ui.create_child_deque(root, ChildSide::Right).unwrap();
        let parent = ui.create_element(root, "parent", End::Back).unwrap();
        let first = ui.create_element(root, "first", End::Front).unwrap();
        let second = ui.create_element(root, "second", End::Front).unwrap();
        let left_root = ui.create_element(left, "left-root", End::Back).unwrap();
        let right_root = ui.create_element(right, "right-root", End::Back).unwrap();

        assert_eq!(ui.logical_parent(first), Some(parent));
        assert_eq!(ui.logical_parent(second), Some(parent));
        assert!(ui.are_logical_siblings(first, second));
        assert_eq!(ui.logical_parent(left_root), Some(parent));
        assert_eq!(ui.logical_parent(right_root), Some(parent));
        assert!(ui.are_logical_siblings(left_root, right_root));
        let children = ui.logical_children(parent);
        assert!(children.contains(&first));
        assert!(children.contains(&second));
        assert!(children.contains(&left_root));
        assert!(children.contains(&right_root));
    }

    #[test]
    fn removing_a_deque_root_clears_all_descendant_deques_and_ghosts_them() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let left = ui.create_child_deque(root, ChildSide::Left).unwrap();
        let right = ui.create_child_deque(root, ChildSide::Right).unwrap();
        let parent = ui.create_element(root, "parent", End::Back).unwrap();
        let left_child = ui.create_element(left, "left-child", End::Back).unwrap();
        let right_child = ui.create_element(right, "right-child", End::Back).unwrap();

        let removed = ui.remove_subtree(parent).unwrap();
        assert_eq!(removed.len(), 3);
        assert!(removed.contains(&parent));
        assert!(removed.contains(&left_child));
        assert!(removed.contains(&right_child));
        assert!(
            ui.tree()
                .get(ui.deque_id(root).unwrap())
                .unwrap()
                .deque()
                .is_ghost()
        );
        assert!(
            ui.tree()
                .get(ui.deque_id(left).unwrap())
                .unwrap()
                .deque()
                .is_ghost()
        );
        assert!(
            ui.tree()
                .get(ui.deque_id(right).unwrap())
                .unwrap()
                .deque()
                .is_ghost()
        );
    }
}
