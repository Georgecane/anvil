use super::style::{ComputedStyle, CssStyleEngine, StyleCommit};
use super::ui::{ElementHandle, LogicalProjection, UiElement, UiMutation, UiMutationKind};
use crate::deque_tree::RenderStack;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::matching::{ElementSelectorFlags, MatchingContext, VisitedHandlingMode};
use selectors::sink::Push;
use selectors::{Element as SelectorsElement, OpaqueElement};
use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};

use style::CaseSensitivityExt;
use style::context::{
    QuirksMode, RegisteredSpeculativePainter, RegisteredSpeculativePainters, SharedStyleContext,
    StyleContext, ThreadLocalStyleContext,
};
use style::data::{ElementDataMut, ElementDataRef, ElementDataWrapper};
use style::device::servo::{FontMetricsProvider, MediaData};
use style::dom::{LayoutIterator, NodeInfo, OpaqueNode, TDocument, TElement, TNode, TShadowRoot};
use style::font_metrics::FontMetrics;
use style::global_style_data::GLOBAL_STYLE_DATA;
use style::media_queries::MediaList;
use style::properties::declaration_block::parse_style_attribute;
use style::properties::{ComputedValues, PropertyDeclarationBlock};
use style::servo::animation::DocumentAnimationSet;
use style::servo::media_features::PointerCapabilities;
use style::servo::selector_parser::SnapshotMap;
use style::shared_lock::{Locked, SharedRwLock, StylesheetGuards};
use style::stylesheets::{
    AllowImportRules, CssRuleType, DocumentStyleSheet, Origin, Stylesheet, UrlExtraData,
};
use style::stylist::{RuleInclusion, Stylist};
use style::traversal::resolve_style;
use style::traversal_flags::TraversalFlags;
use style::values::AtomIdent;
use style::values::computed::Length;
use style::values::computed::font::GenericFontFamily;
use style::{Atom, LocalName, Namespace};
use style_dom::ElementState;
use style_traits::{CSSPixel, DevicePixel, ToCss};

use super::ui::UiElementKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StyloError {
    InvalidBaseUrl,
}

impl fmt::Display for StyloError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBaseUrl => f.write_str("invalid Stylo base URL"),
        }
    }
}

impl std::error::Error for StyloError {}

#[derive(Debug)]
struct FallbackFontMetrics;

impl FontMetricsProvider for FallbackFontMetrics {
    fn query_font_metrics(
        &self,
        _vertical: bool,
        _font: &style::properties::style_structs::Font,
        _base_size: style::values::computed::CSSPixelLength,
        _flags: style::values::specified::font::QueryFontMetricsFlags,
    ) -> FontMetrics {
        FontMetrics::default()
    }

    fn base_size_for_generic(&self, _generic: GenericFontFamily) -> Length {
        Length::new(16.0)
    }
}

struct NoSpeculativePainters;

impl RegisteredSpeculativePainters for NoSpeculativePainters {
    fn get(&self, _name: &Atom) -> Option<&dyn RegisteredSpeculativePainter> {
        None
    }
}

struct StyloElementData {
    inner: UnsafeCell<Option<ElementDataWrapper>>,
}

impl Default for StyloElementData {
    fn default() -> Self {
        Self {
            inner: UnsafeCell::new(None),
        }
    }
}

impl StyloElementData {
    fn has_data(&self) -> bool {
        // SAFETY: the style pass is sequential and this object is owned by the
        // per-pass StyloDocument. No concurrent mutation is performed.
        unsafe { (&*self.inner.get()).is_some() }
    }

    fn borrow(&self) -> Option<ElementDataRef<'_>> {
        // SAFETY: Stylo's immutable data access contract is upheld by the
        // sequential style pass.
        unsafe {
            (&*self.inner.get())
                .as_ref()
                .map(ElementDataWrapper::borrow)
        }
    }

    fn mutate(&self) -> Option<ElementDataMut<'_>> {
        // SAFETY: mutation only occurs during the exclusive sequential style pass.
        unsafe {
            (&*self.inner.get())
                .as_ref()
                .map(ElementDataWrapper::borrow_mut)
        }
    }

    unsafe fn ensure(&self) -> ElementDataMut<'_> {
        // SAFETY: caller has exclusive access to the Stylo style pass and no
        // ElementData borrow may outlive this call.
        unsafe {
            let slot = &mut *self.inner.get();
            if slot.is_none() {
                *slot = Some(ElementDataWrapper::default());
            }
            slot.as_ref().expect("initialized above").borrow_mut()
        }
    }

    unsafe fn clear(&self) {
        // SAFETY: the style pass has exclusive access and there are no active
        // borrows to ElementData.
        unsafe { *self.inner.get() = None };
    }
}

struct StyloElementState {
    is_text: bool,
    has_nonempty_text: bool,
    tag: <style::selector_parser::SelectorImpl as selectors::SelectorImpl>::BorrowedLocalName,
    namespace:
        <style::selector_parser::SelectorImpl as selectors::SelectorImpl>::BorrowedNamespaceUrl,
    id: Option<Atom>,
    classes: Vec<AtomIdent>,
    attributes: Vec<(LocalName, String)>,
    inline_style: Option<style::servo_arc::Arc<Locked<PropertyDeclarationBlock>>>,
    selector_flags: Cell<ElementSelectorFlags>,
    data: Rc<StyloElementData>,
    children_to_process: AtomicIsize,
}

impl StyloElementState {
    fn new(
        element: &UiElement,
        lock: &SharedRwLock,
        url_data: &UrlExtraData,
        data: Rc<StyloElementData>,
    ) -> Self {
        let mut state = Self {
            is_text: false,
            has_nonempty_text: false,
            tag: <style::selector_parser::SelectorImpl as selectors::SelectorImpl>::BorrowedLocalName::from(""),
            namespace: <style::selector_parser::SelectorImpl as selectors::SelectorImpl>::BorrowedNamespaceUrl::from(""),
            id: None,
            classes: Vec::new(),
            attributes: Vec::new(),
            inline_style: None,
            selector_flags: Cell::new(ElementSelectorFlags::empty()),
            data,
            children_to_process: AtomicIsize::new(0),
        };
        state.refresh(element, lock, url_data);
        state
    }

    /// Refreshes selector-facing data in place. Existing Vec and String
    /// capacities are retained, and the Rc state allocation is not replaced.
    fn refresh(&mut self, element: &UiElement, lock: &SharedRwLock, url_data: &UrlExtraData) {
        self.is_text = element.kind == UiElementKind::Text;
        self.has_nonempty_text = element
            .text_content
            .as_deref()
            .is_some_and(|text| !text.is_empty());
        self.tag = <style::selector_parser::SelectorImpl as selectors::SelectorImpl>::BorrowedLocalName::from(element.tag.as_str());
        self.id = element
            .properties
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("id"))
            .map(|(_, value)| Atom::from(value.as_str()));

        self.classes.clear();
        if let Some((_, value)) = element
            .properties
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("class"))
        {
            self.classes
                .extend(value.split_ascii_whitespace().map(AtomIdent::from));
        }

        let mut attribute_index = 0;
        for (name, value) in element
            .properties
            .iter()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("style"))
        {
            if let Some((existing_name, existing_value)) = self.attributes.get_mut(attribute_index)
            {
                *existing_name = LocalName::from(name.as_str());
                existing_value.clone_from(value);
            } else {
                self.attributes
                    .push((LocalName::from(name.as_str()), value.clone()));
            }
            attribute_index += 1;
        }
        self.attributes.truncate(attribute_index);

        self.inline_style = if element.styles.is_empty() {
            None
        } else {
            let mut declarations = String::new();
            for (name, value) in &element.styles {
                use fmt::Write as _;
                let _ = write!(declarations, "{name}: {value};");
            }
            let block = parse_style_attribute(
                &declarations,
                url_data,
                None,
                QuirksMode::NoQuirks,
                CssRuleType::Style,
            );
            Some(style::servo_arc::Arc::new(lock.wrap(block)))
        };
        self.children_to_process.store(0, Ordering::Relaxed);
    }

    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| AsRef::<str>::as_ref(&key.0).eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

struct StyleFingerprintWriter<'a>(&'a mut DefaultHasher);
impl fmt::Write for StyleFingerprintWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.0.write(value.as_bytes());
        Ok(())
    }
}

/// Only inherited style structs can affect descendants' computed values.
/// Hashing the complete ComputedValues included rule-tree and non-inherited
/// data, causing needless descendant recomputation and a large debug allocation.
fn inherited_style_fingerprint(values: &ComputedValues) -> u64 {
    let mut hasher = DefaultHasher::new();
    let _ = write!(
        StyleFingerprintWriter(&mut hasher),
        "{:?}{:?}{:?}",
        values.get_font(),
        values.get_inherited_box(),
        values.get_inherited_text(),
    );
    hasher.finish()
}

fn mark_subtree(
    projection: &LogicalProjection<'_>,
    anchor: ElementHandle,
    dirty: &mut HashSet<ElementHandle>,
) {
    let mut pending = vec![anchor];
    while let Some(handle) = pending.pop() {
        if !dirty.insert(handle) {
            continue;
        }
        pending.extend(projection.children_of(handle));
    }
}

/// Adds a subtree to the current style pass in parent-first order. Already
/// scheduled nodes are not duplicated, but their descendants are still
/// traversed because a newly changed inherited value may affect them.
fn enqueue_subtree_style_order(
    projection: &LogicalProjection<'_>,
    anchor: ElementHandle,
    scheduled: &mut HashSet<ElementHandle>,
    queue: &mut VecDeque<ElementHandle>,
) {
    let mut pending = vec![anchor];
    while let Some(handle) = pending.pop() {
        if scheduled.insert(handle) {
            queue.push_back(handle);
        }
        let children = projection.children_of(handle);
        pending.extend(children.into_iter().rev());
    }
}

/// Orders only the dirty set. A dirty descendant depends on its nearest dirty
/// ancestor, even when clean elements sit between them. Parent links are cached
/// while climbing so shared ancestor chains are resolved only once.
fn parent_first_dirty_order(
    projection: &LogicalProjection<'_>,
    dirty: &HashSet<ElementHandle>,
) -> Vec<ElementHandle> {
    let mut parent_cache = HashMap::<ElementHandle, Option<ElementHandle>>::new();
    let mut children = HashMap::<ElementHandle, Vec<ElementHandle>>::new();
    let mut has_dirty_parent = HashSet::<ElementHandle>::new();

    for handle in dirty.iter().copied() {
        let mut ancestor = projection.parent_of(handle);
        while let Some(current) = ancestor {
            if dirty.contains(&current) {
                children.entry(current).or_default().push(handle);
                has_dirty_parent.insert(handle);
                break;
            }
            ancestor = *parent_cache
                .entry(current)
                .or_insert_with(|| projection.parent_of(current));
        }
    }

    let mut ready: Vec<_> = dirty
        .iter()
        .copied()
        .filter(|handle| !has_dirty_parent.contains(handle))
        .collect();
    let mut ordered = Vec::with_capacity(dirty.len());
    let mut cursor = 0;
    while cursor < ready.len() {
        let handle = ready[cursor];
        cursor += 1;
        ordered.push(handle);
        if let Some(descendants) = children.get(&handle) {
            ready.extend(descendants.iter().copied());
        }
    }

    if ordered.len() != dirty.len() {
        let emitted: HashSet<_> = ordered.iter().copied().collect();
        ordered.extend(
            dirty
                .iter()
                .copied()
                .filter(|handle| !emitted.contains(handle)),
        );
    }
    ordered
}

fn mark_parent_context_all(
    projection: &LogicalProjection<'_>,
    parent: Option<ElementHandle>,
    dirty: &mut HashSet<ElementHandle>,
) {
    let Some(mut ancestor) = parent else {
        dirty.extend(projection.root_elements());
        return;
    };
    loop {
        dirty.insert(ancestor);
        dirty.extend(projection.children_of(ancestor));
        let Some(parent) = projection.parent_of(ancestor) else {
            break;
        };
        ancestor = parent;
    }
}

fn selector_flags(
    states: &HashMap<ElementHandle, Rc<StyloElementState>>,
    handle: ElementHandle,
) -> ElementSelectorFlags {
    states
        .get(&handle)
        .map_or_else(ElementSelectorFlags::empty, |state| {
            state.selector_flags.get()
        })
}

fn mark_sibling_context(
    projection: &LogicalProjection<'_>,
    handle: ElementHandle,
    dirty: &mut HashSet<ElementHandle>,
) {
    if let Some(parent) = projection.parent_of(handle) {
        dirty.extend(projection.children_of(parent));
    } else {
        dirty.extend(projection.root_elements());
    }
}

/// Non-structural mutations only expand into sibling subtrees when Stylo's
/// selector matching recorded a dependency that can cross sibling boundaries.
/// Ancestors are always dirtied so :has() and inherited values remain correct.
fn mark_parent_context_incremental(
    projection: &LogicalProjection<'_>,
    parent: Option<ElementHandle>,
    changed: Option<ElementHandle>,
    states: &HashMap<ElementHandle, Rc<StyloElementState>>,
    dirty: &mut HashSet<ElementHandle>,
) {
    let sibling_search_flags = ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING
        | ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING;
    let sibling_dependency_flags = ElementSelectorFlags::HAS_SLOW_SELECTOR_LATER_SIBLINGS
        | ElementSelectorFlags::HAS_SLOW_SELECTOR_NTH_OF;

    if let Some(changed) = changed
        && selector_flags(states, changed).intersects(sibling_search_flags)
    {
        mark_sibling_context(projection, changed, dirty);
    }

    let mut ancestor = parent;
    while let Some(current) = ancestor {
        dirty.insert(current);
        let flags = selector_flags(states, current);
        if flags.intersects(sibling_dependency_flags) {
            dirty.extend(projection.children_of(current));
        }
        if flags.intersects(sibling_search_flags) {
            mark_sibling_context(projection, current, dirty);
        }
        ancestor = projection.parent_of(current);
    }
}

struct StylesheetWork {
    stack: RenderStack,
    deque_index: usize,
    element_index: usize,
}

impl StylesheetWork {
    const ELEMENT_BUDGET: usize = 128;

    fn new(projection: &LogicalProjection<'_>) -> Self {
        Self {
            stack: projection.render_stack_snapshot(),
            deque_index: 0,
            element_index: 0,
        }
    }

    fn restart(&mut self, projection: &LogicalProjection<'_>) {
        self.stack = projection.render_stack_snapshot();
        self.deque_index = 0;
        self.element_index = 0;
    }

    fn next_batch(&mut self, projection: &LogicalProjection<'_>) -> Vec<ElementHandle> {
        let mut batch = Vec::with_capacity(Self::ELEMENT_BUDGET);
        let mut work_units = 0;
        while work_units < Self::ELEMENT_BUDGET {
            let Some(&deque_id) = self.stack.as_slice().get(self.deque_index) else {
                break;
            };
            let count = projection.deque_element_count(deque_id);
            if self.element_index >= count {
                self.deque_index += 1;
                self.element_index = 0;
                work_units += 1;
                continue;
            }
            let position = self.element_index;
            self.element_index += 1;
            work_units += 1;
            if let Some(handle) = projection.handle_at_deque_position(deque_id, position) {
                batch.push(handle);
            }
        }
        batch
    }

    fn is_complete(&self) -> bool {
        self.deque_index >= self.stack.len()
    }
}

struct InvalidationBatch {
    dirty: HashSet<ElementHandle>,
    refresh_states: HashSet<ElementHandle>,
    removed_states: HashSet<ElementHandle>,
    checked_deques: usize,
    hash_changed_deques: usize,
}

fn collect_invalidated<'mutation>(
    projection: &LogicalProjection<'_>,
    mutations: impl Iterator<Item = &'mutation UiMutation>,
    suppress_dirty_expansion: bool,
    states: &HashMap<ElementHandle, Rc<StyloElementState>>,
) -> InvalidationBatch {
    let mut batch = InvalidationBatch {
        dirty: HashSet::new(),
        refresh_states: HashSet::new(),
        removed_states: HashSet::new(),
        checked_deques: 0,
        hash_changed_deques: 0,
    };
    let mut checked_deques = HashSet::new();
    let mut hash_changed_deques = HashSet::new();

    for mutation in mutations {
        checked_deques.extend(mutation.checked_deques.iter().copied());
        hash_changed_deques.extend(mutation.hash_changed_deques.iter().copied());

        match mutation.kind {
            UiMutationKind::Text | UiMutationKind::Attribute => {
                if let Some(handle) = mutation.element
                    && !suppress_dirty_expansion
                {
                    batch.refresh_states.insert(handle);
                    mark_subtree(projection, handle, &mut batch.dirty);
                }
                if !suppress_dirty_expansion {
                    mark_parent_context_incremental(
                        projection,
                        mutation.new_parent,
                        mutation.element,
                        states,
                        &mut batch.dirty,
                    );
                }
            }
            UiMutationKind::InlineStyle => {
                if let Some(handle) = mutation.element
                    && !suppress_dirty_expansion
                {
                    batch.refresh_states.insert(handle);
                    mark_subtree(projection, handle, &mut batch.dirty);
                }
            }
            UiMutationKind::Insert => {
                for handle in &mutation.affected {
                    if !suppress_dirty_expansion {
                        batch.refresh_states.insert(*handle);
                        mark_subtree(projection, *handle, &mut batch.dirty);
                    }
                }
                if !suppress_dirty_expansion {
                    mark_parent_context_all(projection, mutation.new_parent, &mut batch.dirty);
                }
            }
            UiMutationKind::Remove => {
                batch
                    .removed_states
                    .extend(mutation.affected.iter().copied());
                if !suppress_dirty_expansion {
                    mark_parent_context_all(projection, mutation.old_parent, &mut batch.dirty);
                }
            }
            UiMutationKind::Move => {
                if !suppress_dirty_expansion {
                    for handle in &mutation.affected {
                        mark_subtree(projection, *handle, &mut batch.dirty);
                    }
                    mark_parent_context_all(projection, mutation.old_parent, &mut batch.dirty);
                    mark_parent_context_all(projection, mutation.new_parent, &mut batch.dirty);
                }
            }
        }

        // The Registry's targeted identity-hash comparison is the structural
        // fast path. It names exactly the Deques whose ordered identity/ranges
        // changed; values and CSS declarations deliberately do not affect hashes.
        if !suppress_dirty_expansion {
            for deque_id in &mutation.hash_changed_deques {
                for handle in projection.handles_in_deque(*deque_id) {
                    mark_subtree(projection, handle, &mut batch.dirty);
                }
            }
        }
    }

    batch.checked_deques = checked_deques.len();
    batch.hash_changed_deques = hash_changed_deques.len();
    batch
}

struct StyloDocument<'projection> {
    projection: LogicalProjection<'projection>,
    // Points to StyloStyleEngine::element_states, which is immutable for the
    // entire lifetime of this per-pass document.
    states: NonNull<HashMap<ElementHandle, Rc<StyloElementState>>>,
    shared_lock: SharedRwLock,
    // Node wrappers are materialized only when Stylo traverses to them. Box
    // keeps each key's address stable across HashMap growth.
    node_keys: RefCell<HashMap<Option<ElementHandle>, Box<StyloNodeKey<'projection>>>>,
    children_cache: RefCell<HashMap<Option<ElementHandle>, Rc<[ElementHandle]>>>,
    child_positions: RefCell<HashMap<ElementHandle, usize>>,
    flagged_nodes: RefCell<HashSet<ElementHandle>>,
}

struct LayoutThreadStateGuard;
impl LayoutThreadStateGuard {
    fn enter() -> Self {
        style::thread_state::enter(style::thread_state::ThreadState::LAYOUT);
        Self
    }
}
impl Drop for LayoutThreadStateGuard {
    fn drop(&mut self) {
        style::thread_state::exit(style::thread_state::ThreadState::LAYOUT);
    }
}

struct StyloNodeKey<'projection> {
    document: NonNull<StyloDocument<'projection>>,
    handle: Option<ElementHandle>,
}

struct StyloChildrenIterator<'projection> {
    document: NonNull<StyloDocument<'projection>>,
    children: Rc<[ElementHandle]>,
    index: usize,
}

impl<'projection> Iterator for StyloChildrenIterator<'projection> {
    type Item = StyloNode<'projection>;

    fn next(&mut self) -> Option<Self::Item> {
        let handle = *self.children.get(self.index)?;
        self.index += 1;
        // SAFETY: the iterator is created from a StyloNode and cannot outlive
        // the per-pass boxed StyloDocument that owns the cached node keys.
        let document = unsafe { self.document.as_ref() };
        Some(document.node(Some(handle)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.children.len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for StyloChildrenIterator<'_> {}

impl<'projection> StyloDocument<'projection> {
    fn state(&self, handle: ElementHandle) -> Option<&StyloElementState> {
        // SAFETY: the engine's state map is not mutated while this document is
        // alive; compute() drops the document before changing engine state.
        unsafe { self.states.as_ref().get(&handle).map(Rc::as_ref) }
    }

    fn node(&self, handle: Option<ElementHandle>) -> StyloNode<'projection> {
        let mut keys = self.node_keys.borrow_mut();
        let key = keys.entry(handle).or_insert_with(|| {
            Box::new(StyloNodeKey {
                document: NonNull::from(self),
                handle,
            })
        });
        StyloNode(NonNull::from(key.as_ref()))
    }

    fn children(&self, handle: Option<ElementHandle>) -> Rc<[ElementHandle]> {
        if let Some(children) = self.children_cache.borrow().get(&handle) {
            return Rc::clone(children);
        }
        let children: Rc<[ElementHandle]> = match handle {
            Some(handle) => self.projection.children_of(handle).into(),
            None => self.projection.root_elements().into(),
        };
        {
            let mut positions = self.child_positions.borrow_mut();
            for (index, child) in children.iter().copied().enumerate() {
                positions.insert(child, index);
            }
        }
        self.children_cache
            .borrow_mut()
            .insert(handle, Rc::clone(&children));
        children
    }

    fn parent(&self, handle: Option<ElementHandle>) -> Option<ElementHandle> {
        handle.and_then(|handle| self.projection.parent_of(handle))
    }

    fn position(&self, handle: ElementHandle) -> Option<(Rc<[ElementHandle]>, usize)> {
        let parent = self.parent(Some(handle));
        let siblings = self.children(parent);
        let index = self.child_positions.borrow().get(&handle).copied()?;
        Some((siblings, index))
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct StyloNode<'projection>(NonNull<StyloNodeKey<'projection>>);

impl<'projection> StyloNode<'projection> {
    fn key(&self) -> &StyloNodeKey<'projection> {
        // SAFETY: StyloDocument owns each boxed node key for the full style pass;
        // StyloNode is only used while that document remains alive.
        unsafe { self.0.as_ref() }
    }

    fn document(&self) -> &StyloDocument<'projection> {
        // SAFETY: node keys are initialized with the stable address of the boxed
        // StyloDocument and are never used after that document is dropped.
        unsafe { self.key().document.as_ref() }
    }

    fn handle(&self) -> Option<ElementHandle> {
        self.key().handle
    }
}

impl fmt::Debug for StyloNode<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.handle() {
            Some(handle) => f.debug_tuple("AnvilElement").field(&handle.0).finish(),
            None => f.write_str("AnvilDocument"),
        }
    }
}

impl PartialEq for StyloNode<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for StyloNode<'_> {}
impl Hash for StyloNode<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.document() as *const StyloDocument<'_> as usize).hash(state);
        self.handle().hash(state);
    }
}

impl NodeInfo for StyloNode<'_> {
    fn is_element(&self) -> bool {
        self.handle()
            .and_then(|handle| self.document().state(handle))
            .is_some_and(|state| !state.is_text)
    }

    fn is_text_node(&self) -> bool {
        self.handle()
            .and_then(|handle| self.document().state(handle))
            .is_some_and(|state| state.is_text)
    }
}

impl<'projection> TDocument for StyloNode<'projection> {
    type ConcreteNode = Self;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn is_html_document(&self) -> bool {
        true
    }

    fn quirks_mode(&self) -> QuirksMode {
        QuirksMode::NoQuirks
    }

    fn shared_lock(&self) -> &SharedRwLock {
        &self.document().shared_lock
    }
}

impl<'projection> TShadowRoot for StyloNode<'projection> {
    type ConcreteNode = Self;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn host(&self) -> <Self::ConcreteNode as TNode>::ConcreteElement {
        panic!("Anvil does not expose ShadowRoot nodes yet")
    }

    fn style_data<'a>(&self) -> Option<&'a style::stylist::CascadeData>
    where
        Self: 'a,
    {
        None
    }
}

impl<'projection> TNode for StyloNode<'projection> {
    type ConcreteElement = Self;
    type ConcreteDocument = Self;
    type ConcreteShadowRoot = Self;

    fn parent_node(&self) -> Option<Self> {
        self.handle()
            .map(|handle| self.document().node(self.document().parent(Some(handle))))
    }

    fn first_child(&self) -> Option<Self> {
        self.document()
            .children(self.handle())
            .first()
            .copied()
            .map(|handle| self.document().node(Some(handle)))
    }

    fn last_child(&self) -> Option<Self> {
        self.document()
            .children(self.handle())
            .last()
            .copied()
            .map(|handle| self.document().node(Some(handle)))
    }

    fn prev_sibling(&self) -> Option<Self> {
        let handle = self.handle()?;
        let (siblings, index) = self.document().position(handle)?;
        index
            .checked_sub(1)
            .and_then(|previous| siblings.get(previous).copied())
            .map(|previous| self.document().node(Some(previous)))
    }

    fn next_sibling(&self) -> Option<Self> {
        let handle = self.handle()?;
        let (siblings, index) = self.document().position(handle)?;
        siblings
            .get(index + 1)
            .copied()
            .map(|next| self.document().node(Some(next)))
    }

    fn owner_doc(&self) -> Self::ConcreteDocument {
        self.document().node(None)
    }

    fn is_in_document(&self) -> bool {
        true
    }

    fn traversal_parent(&self) -> Option<Self::ConcreteElement> {
        self.parent_node().and_then(|parent| parent.as_element())
    }

    fn opaque(&self) -> OpaqueNode {
        // Stylo opaque identities must be unique across document and element
        // nodes and must not truncate 64-bit handles on 32-bit targets. The
        // boxed node key has a stable address for this document's lifetime.
        OpaqueNode(self.0.as_ptr() as usize)
    }

    fn debug_id(self) -> usize {
        self.handle()
            .map_or(self.0.as_ptr() as usize, |handle| handle.0 as usize)
    }

    fn as_element(&self) -> Option<Self::ConcreteElement> {
        self.is_element().then_some(*self)
    }

    fn as_document(&self) -> Option<Self::ConcreteDocument> {
        self.handle().is_none().then_some(*self)
    }

    fn as_shadow_root(&self) -> Option<Self::ConcreteShadowRoot> {
        None
    }
}

impl SelectorsElement for StyloNode<'_> {
    type Impl = style::selector_parser::SelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        debug_assert!(
            self.handle().is_some(),
            "document nodes are not selector elements"
        );
        OpaqueElement::from_ptr(self.0.cast())
    }

    fn parent_element(&self) -> Option<Self> {
        TNode::parent_node(self).and_then(|parent| parent.as_element())
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let mut sibling = TNode::prev_sibling(self);
        while let Some(node) = sibling {
            if node.is_element() {
                return Some(node);
            }
            sibling = TNode::prev_sibling(&node);
        }
        None
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let mut sibling = TNode::next_sibling(self);
        while let Some(node) = sibling {
            if node.is_element() {
                return Some(node);
            }
            sibling = TNode::next_sibling(&node);
        }
        None
    }

    fn first_element_child(&self) -> Option<Self> {
        self.document()
            .children(self.handle())
            .iter()
            .copied()
            .find(|handle| {
                self.document()
                    .state(*handle)
                    .is_some_and(|state| !state.is_text)
            })
            .map(|handle| self.document().node(Some(handle)))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        true
    }

    fn has_local_name(
        &self,
        local_name: &<Self::Impl as selectors::SelectorImpl>::BorrowedLocalName,
    ) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .is_some_and(|state| state.tag == *local_name)
    }

    fn has_namespace(
        &self,
        namespace: &<Self::Impl as selectors::SelectorImpl>::BorrowedNamespaceUrl,
    ) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .is_some_and(|state| state.namespace == *namespace)
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .zip(other.document().state(other.handle().expect("element")))
            .is_some_and(|(a, b)| a.tag == b.tag && a.namespace == b.namespace)
    }

    fn attr_matches(
        &self,
        namespace: &NamespaceConstraint<&<Self::Impl as selectors::SelectorImpl>::NamespaceUrl>,
        local_name: &<Self::Impl as selectors::SelectorImpl>::LocalName,
        operation: &AttrSelectorOperation<&<Self::Impl as selectors::SelectorImpl>::AttrValue>,
    ) -> bool {
        let Some(state) = self.document().state(self.handle().expect("element")) else {
            return false;
        };
        if matches!(namespace, NamespaceConstraint::Specific(ns) if **ns != Namespace::default()) {
            return false;
        }
        state
            .attributes
            .iter()
            .any(|(name, value)| name == local_name && operation.eval_str(value))
    }

    fn match_non_ts_pseudo_class(
        &self,
        _pseudo_class: &<Self::Impl as selectors::SelectorImpl>::NonTSPseudoClass,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        false
    }

    fn match_pseudo_element(
        &self,
        _pseudo_element: &<Self::Impl as selectors::SelectorImpl>::PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, flags: ElementSelectorFlags) {
        let Some(handle) = self.handle() else {
            return;
        };
        if flags.is_empty() {
            return;
        }

        let document = self.document();
        let self_flags = flags.for_self();
        if !self_flags.is_empty()
            && let Some(state) = document.state(handle)
        {
            state
                .selector_flags
                .set(state.selector_flags.get() | self_flags);
            document.flagged_nodes.borrow_mut().insert(handle);
        }

        let parent_flags = flags.for_parent();
        if !parent_flags.is_empty()
            && let Some(parent) = document.projection.parent_of(handle)
            && let Some(state) = document.state(parent)
        {
            state
                .selector_flags
                .set(state.selector_flags.get() | parent_flags);
            document.flagged_nodes.borrow_mut().insert(parent);
        }
    }

    fn is_link(&self) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .is_some_and(|state| {
                AsRef::<str>::as_ref(&state.tag) == "a" && state.attribute("href").is_some()
            })
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(
        &self,
        id: &<Self::Impl as selectors::SelectorImpl>::Identifier,
        case_sensitivity: CaseSensitivity,
    ) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .and_then(|state| state.id.as_ref())
            .is_some_and(|actual| case_sensitivity.eq_atom(actual, &id.0))
    }

    fn has_class(
        &self,
        class: &<Self::Impl as selectors::SelectorImpl>::Identifier,
        case_sensitivity: CaseSensitivity,
    ) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .is_some_and(|state| {
                state
                    .classes
                    .iter()
                    .any(|actual| case_sensitivity.eq_atom(&actual.0, &class.0))
            })
    }

    fn has_custom_state(
        &self,
        _name: &<Self::Impl as selectors::SelectorImpl>::Identifier,
    ) -> bool {
        false
    }

    fn imported_part(
        &self,
        _name: &<Self::Impl as selectors::SelectorImpl>::Identifier,
    ) -> Option<<Self::Impl as selectors::SelectorImpl>::Identifier> {
        None
    }

    fn is_part(&self, _name: &<Self::Impl as selectors::SelectorImpl>::Identifier) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.document()
            .children(self.handle())
            .iter()
            .all(|handle| {
                self.document()
                    .state(*handle)
                    .is_some_and(|state| state.is_text && !state.has_nonempty_text)
            })
    }

    fn is_root(&self) -> bool {
        self.handle()
            .is_some_and(|handle| self.document().children(None).first().copied() == Some(handle))
    }

    fn add_element_unique_hashes(&self, _filter: &mut selectors::bloom::BloomFilter) -> bool {
        false
    }
}

impl<'projection> TElement for StyloNode<'projection> {
    type ConcreteNode = Self;
    type TraversalChildrenIterator = StyloChildrenIterator<'projection>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn traversal_children(&self) -> LayoutIterator<Self::TraversalChildrenIterator> {
        let document = self.document();
        LayoutIterator(StyloChildrenIterator {
            document: NonNull::from(document),
            children: document.children(self.handle()),
            index: 0,
        })
    }

    fn is_html_element(&self) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .is_some()
    }

    fn is_mathml_element(&self) -> bool {
        false
    }

    fn is_svg_element(&self) -> bool {
        false
    }

    fn style_attribute(
        &self,
    ) -> Option<style::servo_arc::ArcBorrow<'_, Locked<PropertyDeclarationBlock>>> {
        self.document()
            .state(self.handle().expect("element"))?
            .inline_style
            .as_ref()
            .map(|style| style.borrow_arc())
    }

    fn animation_rule(
        &self,
        _context: &SharedStyleContext,
    ) -> Option<style::servo_arc::Arc<Locked<PropertyDeclarationBlock>>> {
        None
    }

    fn transition_rule(
        &self,
        _context: &SharedStyleContext,
    ) -> Option<style::servo_arc::Arc<Locked<PropertyDeclarationBlock>>> {
        None
    }

    fn state(&self) -> ElementState {
        ElementState::empty()
    }

    fn has_part_attr(&self) -> bool {
        false
    }

    fn exports_any_part(&self) -> bool {
        false
    }

    fn id(&self) -> Option<&Atom> {
        self.document()
            .state(self.handle().expect("element"))?
            .id
            .as_ref()
    }

    fn each_class<F>(&self, mut callback: F)
    where
        F: FnMut(&AtomIdent),
    {
        if let Some(state) = self.document().state(self.handle().expect("element")) {
            for class in &state.classes {
                callback(class);
            }
        }
    }

    fn each_attr_name<F>(&self, mut callback: F)
    where
        F: FnMut(&LocalName),
    {
        if let Some(state) = self.document().state(self.handle().expect("element")) {
            for (name, _) in &state.attributes {
                callback(name);
            }
        }
    }

    fn has_dirty_descendants(&self) -> bool {
        false
    }

    fn has_snapshot(&self) -> bool {
        false
    }

    fn handled_snapshot(&self) -> bool {
        true
    }

    unsafe fn set_handled_snapshot(&self) {}

    unsafe fn set_dirty_descendants(&self) {}

    unsafe fn unset_dirty_descendants(&self) {}

    fn store_children_to_process(&self, count: isize) {
        if let Some(state) = self.document().state(self.handle().expect("element")) {
            state.children_to_process.store(count, Ordering::Relaxed);
        }
    }

    fn did_process_child(&self) -> isize {
        self.document()
            .state(self.handle().expect("element"))
            .map_or(0, |state| {
                state.children_to_process.fetch_sub(1, Ordering::Relaxed) - 1
            })
    }

    unsafe fn ensure_data(&self) -> ElementDataMut<'_> {
        // SAFETY: called by Stylo only while the sequential pass owns this document.
        unsafe {
            self.document()
                .state(self.handle().expect("element"))
                .unwrap()
                .data
                .ensure()
        }
    }

    unsafe fn clear_data(&self) {
        // SAFETY: called only while the style pass owns this document.
        unsafe {
            self.document()
                .state(self.handle().expect("element"))
                .unwrap()
                .data
                .clear()
        }
    }

    fn has_data(&self) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .is_some_and(|state| state.data.has_data())
    }

    fn borrow_data(&self) -> Option<ElementDataRef<'_>> {
        self.document()
            .state(self.handle().expect("element"))?
            .data
            .borrow()
            .filter(|data| data.styles.primary.is_some())
    }

    fn mutate_data(&self) -> Option<ElementDataMut<'_>> {
        self.document()
            .state(self.handle().expect("element"))?
            .data
            .mutate()
    }

    fn skip_item_display_fixup(&self) -> bool {
        false
    }

    fn may_have_animations(&self) -> bool {
        false
    }

    fn set_may_have_animations(&self) {}

    fn has_animations(&self, _context: &SharedStyleContext) -> bool {
        false
    }

    fn has_css_animations(
        &self,
        _context: &SharedStyleContext,
        _pseudo_element: Option<style::selector_parser::PseudoElement>,
    ) -> bool {
        false
    }

    fn has_css_transitions(
        &self,
        _context: &SharedStyleContext,
        _pseudo_element: Option<style::selector_parser::PseudoElement>,
    ) -> bool {
        false
    }

    fn shadow_root(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        None
    }

    fn containing_shadow(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        None
    }

    fn get_attr(&self, attr: &LocalName, namespace: &Namespace) -> Option<String> {
        if *namespace != Namespace::default() {
            return None;
        }
        self.document()
            .state(self.handle().expect("element"))?
            .attribute(attr.0.as_ref())
            .map(str::to_owned)
    }

    fn lang_attr(&self) -> Option<style::selector_parser::AttrValue> {
        None
    }

    fn match_element_lang(
        &self,
        _override_lang: Option<Option<style::selector_parser::AttrValue>>,
        _value: &style::selector_parser::Lang,
    ) -> bool {
        false
    }

    fn is_html_document_body_element(&self) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .is_some_and(|state| AsRef::<str>::as_ref(&state.tag).eq_ignore_ascii_case("body"))
    }

    fn synthesize_presentational_hints_for_legacy_attributes<'a, V>(
        &self,
        _visited_handling: VisitedHandlingMode,
        _hints: &mut V,
    ) where
        V: Push<style::applicable_declarations::ApplicableDeclarationBlock<'a>>,
        Self: 'a,
    {
    }

    fn local_name(
        &self,
    ) -> &<style::selector_parser::SelectorImpl as selectors::SelectorImpl>::BorrowedLocalName {
        &self
            .document()
            .state(self.handle().expect("element"))
            .expect("element")
            .tag
    }

    fn namespace(
        &self,
    ) -> &<style::selector_parser::SelectorImpl as selectors::SelectorImpl>::BorrowedNamespaceUrl
    {
        &self
            .document()
            .state(self.handle().expect("element"))
            .expect("element")
            .namespace
    }

    fn query_container_size(
        &self,
        _display: &style::values::specified::Display,
    ) -> euclid::default::Size2D<Option<app_units::Au>> {
        euclid::default::Size2D::new(None, None)
    }

    fn each_custom_state<F>(&self, _callback: F)
    where
        F: FnMut(&AtomIdent),
    {
    }

    fn has_selector_flags(&self, flags: ElementSelectorFlags) -> bool {
        self.document()
            .state(self.handle().expect("element"))
            .is_some_and(|state| state.selector_flags.get().contains(flags))
    }

    fn relative_selector_search_direction(&self) -> ElementSelectorFlags {
        let flags = self
            .document()
            .state(self.handle().expect("element"))
            .map_or_else(ElementSelectorFlags::empty, |state| {
                state.selector_flags.get()
            });
        if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING)
        {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING
        } else if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR)
        {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR
        } else if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING) {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING
        } else {
            ElementSelectorFlags::empty()
        }
    }
}

/// A sequential Stylo CSS engine backed by Anvil's canonical DequeTree projection.
///
/// Stylo receives transient node handles and selector-facing accessors. The
/// engine does not create, own, or cache a second UI hierarchy.
pub struct StyloStyleEngine {
    shared_lock: SharedRwLock,
    url_data: UrlExtraData,
    stylist: Stylist,
    element_states: HashMap<ElementHandle, Rc<StyloElementState>>,
    // Only nodes that actually received selector flags are visited next pass.
    selector_flagged: HashSet<ElementHandle>,
    style_epoch: u64,
    computed_style_epoch: Option<u64>,
    pending_style_epoch: Option<u64>,
    stylesheet_work: Option<StylesheetWork>,
    stylesheet_work_epoch: Option<u64>,
    last_mutation_revision: u64,
    pending_mutation_revision: Option<u64>,
    style_fingerprints: HashMap<ElementHandle, u64>,
    last_recomputed_count: usize,
    last_checked_deque_count: usize,
    last_hash_changed_deque_count: usize,
    last_selector_flag_reset_count: usize,
}

impl Default for StyloStyleEngine {
    fn default() -> Self {
        Self::new().expect("the built-in Anvil Stylo URL is valid")
    }
}

impl StyloStyleEngine {
    pub fn new() -> Result<Self, StyloError> {
        static_prefs::set_pref!("layout.css.has-selector.enabled", true);
        let base_url =
            url::Url::parse("https://anvil.invalid/").map_err(|_| StyloError::InvalidBaseUrl)?;
        let url_data = UrlExtraData::from(base_url);
        let shared_lock = SharedRwLock::new();

        let viewport_size = euclid::Size2D::<f32, CSSPixel>::new(1024.0, 768.0);
        let device_pixel_ratio = euclid::Scale::<f32, CSSPixel, DevicePixel>::new(1.0);
        let device_size = viewport_size * device_pixel_ratio;
        let media_data = MediaData {
            media_type: style::media_queries::MediaType::screen(),
            viewport_size,
            device_size,
            device_pixel_ratio,
            quirks_mode: QuirksMode::NoQuirks,
            prefers_color_scheme: style::queries::values::PrefersColorScheme::Light,
            primary_pointer_capabilities: PointerCapabilities::default(),
            all_pointer_capabilities: PointerCapabilities::default(),
        };
        let default_values = ComputedValues::initial_values_with_font_override(
            style::properties::style_structs::Font::initial_values(),
        );
        let device = style::device::Device::new(
            default_values,
            Box::new(FallbackFontMetrics),
            "Submit Query".into(),
            media_data,
        );

        Ok(Self {
            shared_lock,
            url_data,
            stylist: Stylist::new(device, QuirksMode::NoQuirks),
            element_states: HashMap::new(),
            selector_flagged: HashSet::new(),
            style_epoch: 0,
            computed_style_epoch: None,
            pending_style_epoch: None,
            stylesheet_work: None,
            stylesheet_work_epoch: None,
            last_mutation_revision: 0,
            pending_mutation_revision: None,
            style_fingerprints: HashMap::new(),
            last_recomputed_count: 0,
            last_checked_deque_count: 0,
            last_hash_changed_deque_count: 0,
            last_selector_flag_reset_count: 0,
        })
    }

    pub fn add_stylesheet(&mut self, css: &str) {
        let sheet = Stylesheet::from_str(
            css,
            self.url_data.clone(),
            Origin::Author,
            style::servo_arc::Arc::new(self.shared_lock.wrap(MediaList::empty())),
            self.shared_lock.clone(),
            None,
            None,
            QuirksMode::NoQuirks,
            AllowImportRules::No,
        );
        let sheet = DocumentStyleSheet(style::servo_arc::Arc::new(sheet));
        let guard = self.shared_lock.read();
        self.stylist.append_stylesheet(sheet, &guard);
        self.style_epoch = self.style_epoch.wrapping_add(1);
    }

    /// Number of elements whose CSS was recomputed by the most recent pass.
    pub fn last_recomputed_count(&self) -> usize {
        self.last_recomputed_count
    }
    /// Number of Deque IDs looked up in the registry during the last style pass.
    pub fn last_checked_deque_count(&self) -> usize {
        self.last_checked_deque_count
    }

    /// Number of those Deques whose cached identity hash actually changed.
    pub fn last_hash_changed_deque_count(&self) -> usize {
        self.last_hash_changed_deque_count
    }

    /// Number of elements whose selector flags were reset at the start of the
    /// most recent pass. This is proportional to prior selector-flag usage.
    pub fn last_selector_flag_reset_count(&self) -> usize {
        self.last_selector_flag_reset_count
    }

    fn serialize_computed_values(values: &ComputedValues) -> BTreeMap<String, String> {
        let mut result = BTreeMap::new();
        macro_rules! insert_css {
            ($name:literal, $value:expr) => {
                result.insert($name.into(), $value.to_css_string());
            };
        }

        insert_css!("display", values.get_box().get_display());
        insert_css!("box-sizing", values.slow_clone_box_sizing());
        insert_css!("position", values.get_box().get_position());

        // Flexbox inputs are read from Stylo's computed Position and
        // InheritedBox structs. This remains serialization only; layout is
        // computed later by the native layout adapter.
        insert_css!(
            "direction",
            values.get_inherited_box().slow_clone_direction()
        );
        insert_css!(
            "flex-direction",
            values.get_position().slow_clone_flex_direction()
        );
        insert_css!("flex-wrap", values.get_position().slow_clone_flex_wrap());
        insert_css!("flex-grow", values.get_position().slow_clone_flex_grow());
        insert_css!(
            "flex-shrink",
            values.get_position().slow_clone_flex_shrink()
        );
        insert_css!("flex-basis", values.get_position().slow_clone_flex_basis());
        insert_css!("row-gap", values.get_position().slow_clone_row_gap());
        insert_css!("column-gap", values.get_position().slow_clone_column_gap());

        // Physical dimensions and edges are copied from Stylo's computed
        // values. This adapter does not parse declarations or resolve CSS.
        insert_css!("width", values.slow_clone_width());
        insert_css!("height", values.slow_clone_height());
        insert_css!("min-width", values.slow_clone_min_width());
        insert_css!("min-height", values.slow_clone_min_height());
        insert_css!("max-width", values.slow_clone_max_width());
        insert_css!("max-height", values.slow_clone_max_height());

        insert_css!("margin-top", values.slow_clone_margin_top());
        insert_css!("margin-right", values.slow_clone_margin_right());
        insert_css!("margin-bottom", values.slow_clone_margin_bottom());
        insert_css!("margin-left", values.slow_clone_margin_left());

        insert_css!("padding-top", values.slow_clone_padding_top());
        insert_css!("padding-right", values.slow_clone_padding_right());
        insert_css!("padding-bottom", values.slow_clone_padding_bottom());
        insert_css!("padding-left", values.slow_clone_padding_left());

        insert_css!("border-top-width", values.slow_clone_border_top_width());
        insert_css!("border-right-width", values.slow_clone_border_right_width());
        insert_css!(
            "border-bottom-width",
            values.slow_clone_border_bottom_width()
        );
        insert_css!("border-left-width", values.slow_clone_border_left_width());

        insert_css!("top", values.slow_clone_top());
        insert_css!("right", values.slow_clone_right());
        insert_css!("bottom", values.slow_clone_bottom());
        insert_css!("left", values.slow_clone_left());

        insert_css!("color", values.get_inherited_text().get_color());
        insert_css!(
            "background-color",
            values.get_background().get_background_color()
        );
        insert_css!("font-size", values.slow_clone_font_size());
        insert_css!("font-family", values.get_font().get_font_family());
        insert_css!("font-weight", values.get_font().get_font_weight());
        insert_css!("font-style", values.get_font().get_font_style());
        insert_css!("line-height", values.get_font().get_line_height());
        result.insert(
            "opacity".into(),
            values.get_effects().get_opacity().to_string(),
        );
        result
    }
}

impl CssStyleEngine for StyloStyleEngine {
    type Error = StyloError;

    fn compute(&mut self, projection: &LogicalProjection<'_>) -> Result<StyleCommit, Self::Error> {
        let topology_changed =
            projection
                .mutations_since(self.last_mutation_revision)
                .any(|mutation| {
                    matches!(
                        mutation.kind,
                        UiMutationKind::Insert | UiMutationKind::Remove | UiMutationKind::Move
                    )
                });
        let mutations = projection.mutations_since(self.last_mutation_revision);
        if self.stylesheet_work_epoch != Some(self.style_epoch) {
            self.stylesheet_work = Some(StylesheetWork::new(projection));
            self.stylesheet_work_epoch = Some(self.style_epoch);
        } else if topology_changed && let Some(work) = self.stylesheet_work.as_mut() {
            work.restart(projection);
        }
        let global_recalculation_active = self.stylesheet_work.is_some();
        let mut invalidation = collect_invalidated(
            projection,
            mutations,
            global_recalculation_active,
            &self.element_states,
        );
        self.last_selector_flag_reset_count = 0;
        while self.last_selector_flag_reset_count < StylesheetWork::ELEMENT_BUDGET {
            let Some(handle) = self.selector_flagged.iter().next().copied() else {
                break;
            };
            self.selector_flagged.remove(&handle);
            if let Some(state) = self.element_states.get(&handle) {
                state.selector_flags.set(ElementSelectorFlags::empty());
                self.last_selector_flag_reset_count += 1;
            }
        }
        if let Some(work) = self.stylesheet_work.as_mut() {
            invalidation.dirty.extend(work.next_batch(projection));
        }
        let revision = projection.mutation_revision();
        self.last_checked_deque_count = invalidation.checked_deques;
        self.last_hash_changed_deque_count = invalidation.hash_changed_deques;
        self.pending_mutation_revision = Some(revision);
        self.pending_style_epoch = None;
        if self
            .stylesheet_work
            .as_ref()
            .is_some_and(StylesheetWork::is_complete)
        {
            self.pending_style_epoch = Some(self.style_epoch);
        }
        for handle in &invalidation.removed_states {
            self.element_states.remove(handle);
            self.style_fingerprints.remove(handle);
        }

        for handle in &invalidation.refresh_states {
            let Some(element) = projection.element(*handle) else {
                continue;
            };
            if let Some(state) = self.element_states.get_mut(handle).and_then(Rc::get_mut) {
                state.refresh(element, &self.shared_lock, &self.url_data);
            } else {
                let data = self
                    .element_states
                    .get(handle)
                    .map(|state| Rc::clone(&state.data))
                    .unwrap_or_else(|| Rc::new(StyloElementData::default()));
                self.element_states.insert(
                    *handle,
                    Rc::new(StyloElementState::new(
                        element,
                        &self.shared_lock,
                        &self.url_data,
                        data,
                    )),
                );
            }
        }

        if global_recalculation_active {
            // Refresh both element and text-node state as the bounded cursor
            // reaches each handle. Text nodes are needed by selectors such as :empty.
            for handle in &invalidation.dirty {
                if let Some(element) = projection.element(*handle) {
                    if let Some(state) = self.element_states.get_mut(handle).and_then(Rc::get_mut) {
                        state.refresh(element, &self.shared_lock, &self.url_data);
                    } else {
                        let data = self
                            .element_states
                            .get(handle)
                            .map(|state| Rc::clone(&state.data))
                            .unwrap_or_else(|| Rc::new(StyloElementData::default()));
                        self.element_states.insert(
                            *handle,
                            Rc::new(StyloElementState::new(
                                element,
                                &self.shared_lock,
                                &self.url_data,
                                data,
                            )),
                        );
                    }
                }
            }
        }
        let dirty: HashSet<_> = std::mem::take(&mut invalidation.dirty)
            .into_iter()
            .filter(|handle| {
                projection
                    .element(*handle)
                    .is_some_and(|element| !element.is_text())
            })
            .collect();
        if !global_recalculation_active {
            for handle in &dirty {
                if !self.element_states.contains_key(handle)
                    && let Some(element) = projection.element(*handle)
                {
                    self.element_states.insert(
                        *handle,
                        Rc::new(StyloElementState::new(
                            element,
                            &self.shared_lock,
                            &self.url_data,
                            Rc::new(StyloElementData::default()),
                        )),
                    );
                }
            }
        }
        // Parent-before-child order is derived from dirty ancestry only. No
        // full projection walk or repeated depth calculation is required.
        let mut style_handles = VecDeque::from(parent_first_dirty_order(projection, &dirty));
        let mut scheduled: HashSet<_> = style_handles.iter().copied().collect();
        self.last_recomputed_count = 0;
        if style_handles.is_empty() {
            return Ok(StyleCommit::new());
        }

        let guard = self.shared_lock.read();
        let guards = StylesheetGuards::same(&guard);
        self.stylist.flush(&guards);

        let snapshots = SnapshotMap::new();
        let shared = SharedStyleContext {
            stylist: &self.stylist,
            visited_styles_enabled: false,
            options: GLOBAL_STYLE_DATA.options.clone(),
            guards,
            current_time_for_animations: 0.0,
            traversal_flags: TraversalFlags::empty(),
            snapshot_map: &snapshots,
            animations: DocumentAnimationSet::default(),
            registered_speculative_painters: &NoSpeculativePainters,
        };

        let thread_state_guard = LayoutThreadStateGuard::enter();
        let mut thread_local = ThreadLocalStyleContext::new();
        let document = Box::new(StyloDocument {
            projection: *projection,
            states: NonNull::from(&self.element_states),
            shared_lock: self.shared_lock.clone(),
            node_keys: RefCell::new(HashMap::new()),
            children_cache: RefCell::new(HashMap::new()),
            child_positions: RefCell::new(HashMap::new()),
            flagged_nodes: RefCell::new(HashSet::new()),
        });
        let mut commit = StyleCommit::new();

        while let Some(handle) = style_handles.pop_front() {
            let node = document.node(Some(handle));
            if !node.is_element() {
                continue;
            }

            if node.has_data() {
                unsafe { node.clear_data() };
            }

            let styles = {
                let mut context = StyleContext {
                    shared: &shared,
                    thread_local: &mut thread_local,
                };
                resolve_style(&mut context, node, RuleInclusion::All, None, None)
            };

            let computed = Self::serialize_computed_values(styles.primary());
            let fingerprint = inherited_style_fingerprint(styles.primary());
            let mut data = unsafe { node.ensure_data() };
            data.styles = styles;

            let old_fingerprint = self.style_fingerprints.insert(handle, fingerprint);
            self.last_recomputed_count += 1;
            commit.push(ComputedStyle {
                element: handle,
                properties: computed,
            });

            if old_fingerprint != Some(fingerprint) && !global_recalculation_active {
                for child in projection.children_of(handle) {
                    enqueue_subtree_style_order(
                        projection,
                        child,
                        &mut scheduled,
                        &mut style_handles,
                    );
                }
            }
        }

        self.selector_flagged
            .extend(std::mem::take(&mut *document.flagged_nodes.borrow_mut()));
        drop(document);
        drop(thread_local);
        drop(thread_state_guard);
        Ok(commit)
    }

    fn acknowledge_mutations(&mut self, revision: u64) {
        self.last_mutation_revision = self.last_mutation_revision.max(revision);
        if self
            .pending_mutation_revision
            .is_some_and(|pending| pending <= revision)
        {
            self.pending_mutation_revision = None;
            if let Some(epoch) = self.pending_style_epoch.take() {
                self.computed_style_epoch = Some(epoch);
                if self.stylesheet_work_epoch == Some(epoch) {
                    self.stylesheet_work = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deque_tree::End;
    use crate::runtime::style::{StyleCommitAdapter, run_style_pass};
    use crate::runtime::ui::UiModel;

    fn style_value(ui: &UiModel, handle: ElementHandle, name: &str) -> String {
        ui.computed_styles(handle)
            .and_then(|values| values.get(name))
            .unwrap_or_else(|| panic!("computed {name} style missing for {handle:?}"))
            .clone()
    }

    #[test]
    fn refreshing_stylo_state_reuses_state_and_attribute_storage() {
        let lock = SharedRwLock::new();
        let url_data = UrlExtraData::from(url::Url::parse("https://anvil.invalid/").unwrap());
        let mut element = UiElement::new("section");
        element
            .properties
            .insert("class".into(), "alpha beta".into());
        element.properties.insert("title".into(), "before".into());
        let mut state = StyloElementState::new(
            &element,
            &lock,
            &url_data,
            Rc::new(StyloElementData::default()),
        );

        let state_address = std::ptr::from_ref(&state);
        let classes_allocation = state.classes.as_ptr();
        let attributes_allocation = state.attributes.as_ptr();
        let title_allocation = state
            .attributes
            .iter()
            .find(|(name, _)| AsRef::<str>::as_ref(&name.0) == "title")
            .unwrap()
            .1
            .as_ptr();

        element
            .properties
            .insert("class".into(), "gamma delta".into());
        element.properties.insert("title".into(), "after".into());
        state.refresh(&element, &lock, &url_data);

        assert_eq!(std::ptr::from_ref(&state), state_address);
        assert_eq!(state.classes.as_ptr(), classes_allocation);
        assert_eq!(state.attributes.as_ptr(), attributes_allocation);
        assert_eq!(
            state
                .attributes
                .iter()
                .find(|(name, _)| AsRef::<str>::as_ref(&name.0) == "title")
                .unwrap()
                .1
                .as_ptr(),
            title_allocation
        );
        assert_eq!(
            state
                .attributes
                .iter()
                .find(|(name, _)| AsRef::<str>::as_ref(&name.0) == "title")
                .unwrap()
                .1,
            "after"
        );
    }

    #[test]
    fn stylo_opaque_node_ids_are_unique_for_document_and_zero_handle() {
        let mut ui = UiModel::new();
        let projection = ui.projection();
        let states = HashMap::new();
        let document = Box::new(StyloDocument {
            projection,
            states: NonNull::from(&states),
            shared_lock: SharedRwLock::new(),
            node_keys: RefCell::new(HashMap::new()),
            children_cache: RefCell::new(HashMap::new()),
            child_positions: RefCell::new(HashMap::new()),
            flagged_nodes: RefCell::new(HashSet::new()),
        });
        let document_node = document.node(None);
        let zero_handle_node = document.node(Some(ElementHandle(0)));
        let second_node = document.node(Some(ElementHandle(1)));

        assert_ne!(
            TNode::opaque(&document_node),
            TNode::opaque(&zero_handle_node)
        );
        assert_ne!(
            TNode::opaque(&zero_handle_node),
            TNode::opaque(&second_node)
        );
    }

    #[test]
    fn initial_snapshot_normalizes_html_names_and_css_property_names() {
        let mut element = UiElement::new("div");
        element.tag = "BUTTON".into();
        element.properties.insert("CLASS".into(), "action".into());
        element.styles.insert("COLOR".into(), "green".into());
        let groups = vec![crate::runtime::ui::InitialDeque {
            handle: crate::runtime::ui::DequeHandle(490),
            elements: vec![crate::runtime::ui::InitialElement {
                handle: ElementHandle(4900),
                element,
                subtree_span: 1,
            }],
        }];
        let mut ui = UiModel::from_initial_groups(groups).unwrap();
        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("button.action { color: red; display: block; }");

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(ui.element(ElementHandle(4900)).unwrap().tag, "button");
        assert_eq!(style_value(&ui, ElementHandle(4900), "display"), "block");
        assert_eq!(
            style_value(&ui, ElementHandle(4900), "color"),
            "rgb(0, 128, 0)"
        );
    }

    #[test]
    fn stylo_matches_id_and_class_selectors_and_cascades_declarations() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let _container = ui.create_element(root, "div", End::Back).unwrap();
        let button = ui.create_element(root, "BUTTON", End::Front).unwrap();
        ui.set_property(button, "ID", "primary").unwrap();
        ui.set_property(button, "CLASS", "action selected").unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(
            "#primary { display: block; } button.action { color: red; } .selected { color: blue !important; background-color: blue; font-size: 20px; opacity: 0.5; }",
        );

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(style_value(&ui, button, "display"), "block");
        assert_eq!(style_value(&ui, button, "color"), "rgb(0, 0, 255)");
        assert_eq!(
            style_value(&ui, button, "background-color"),
            "rgb(0, 0, 255)"
        );
        assert_eq!(style_value(&ui, button, "font-size"), "20px");
        assert_eq!(style_value(&ui, button, "opacity"), "0.5");
    }

    #[test]
    fn computed_typography_is_serialized_from_stylo() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let text = ui.create_text(root, "Typography", End::Back).unwrap();
        let container = ui.create_element(root, "div", End::Back).unwrap();
        ui.set_style(container, "font-family", "\"DejaVu Sans\", serif")
            .unwrap();
        ui.set_style(container, "font-weight", "650").unwrap();
        ui.set_style(container, "font-style", "oblique 12deg")
            .unwrap();
        ui.set_style(container, "line-height", "1.75").unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(style_value(&ui, container, "font-family"), "\"DejaVu Sans\", serif");
        assert_eq!(style_value(&ui, container, "font-weight"), "650");
        assert_eq!(style_value(&ui, container, "font-style"), "oblique 12deg");
        assert_eq!(style_value(&ui, container, "line-height"), "1.75");
        let _ = text;
    }

    #[test]
    fn custom_property_names_remain_case_sensitive() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let item = ui.create_element(root, "div", End::Back).unwrap();
        ui.set_style(item, "--ThemeColor", "rgb(12, 34, 56)")
            .unwrap();
        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(
            "div { color: var(--ThemeColor); background-color: var(--themecolor, rgb(0, 0, 0)); }",
        );

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(style_value(&ui, item, "color"), "rgb(12, 34, 56)");
        assert_eq!(style_value(&ui, item, "background-color"), "rgb(0, 0, 0)");
    }

    #[test]
    fn removing_a_sibling_recomputes_nth_child_selectors_for_remaining_nodes() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let parent = ui.create_element(root, "main", End::Back).unwrap();
        let first = ui.create_element(root, "span", End::Front).unwrap();
        let second = ui.create_element(root, "span", End::Front).unwrap();
        let third = ui.create_element(root, "span", End::Front).unwrap();
        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("span:nth-child(2) { color: rgb(200, 10, 20); }");

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(style_value(&ui, first, "color"), "rgb(0, 0, 0)");
        assert_eq!(style_value(&ui, second, "color"), "rgb(200, 10, 20)");
        assert_eq!(style_value(&ui, third, "color"), "rgb(0, 0, 0)");

        ui.remove_subtree(first).unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(style_value(&ui, second, "color"), "rgb(0, 0, 0)");
        assert_eq!(style_value(&ui, third, "color"), "rgb(200, 10, 20)");
        assert!(ui.element(parent).is_some());
    }

    #[test]
    fn stylo_applies_inline_style_after_author_stylesheets() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let button = ui.create_element(root, "button", End::Back).unwrap();
        ui.set_style(button, "COLOR", "green").unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("button { color: red; display: block; }");

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        let color = style_value(&ui, button, "color");
        assert_eq!(
            color, "rgb(0, 128, 0)",
            "inline style did not win cascade: {color}"
        );
        assert_eq!(style_value(&ui, button, "display"), "block");
    }

    #[test]
    fn stylo_selectors_and_inheritance_cross_ghost_deque_branches() {
        let groups = vec![
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(500),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(5000),
                    element: UiElement::new("main"),
                    subtree_span: 1,
                }],
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(501),
                elements: Vec::new(),
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(502),
                elements: Vec::new(),
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(503),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(5003),
                    element: UiElement::new("article"),
                    subtree_span: 1,
                }],
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(504),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(5004),
                    element: UiElement::new("aside"),
                    subtree_span: 1,
                }],
            },
        ];
        let mut ui = UiModel::from_initial_groups(groups).unwrap();
        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(
            "main > article { color: rgb(40, 80, 160); } article + aside { background-color: rgb(10, 20, 30); }",
        );

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(
            style_value(&ui, ElementHandle(5003), "color"),
            "rgb(40, 80, 160)"
        );
        assert_eq!(
            style_value(&ui, ElementHandle(5004), "background-color"),
            "rgb(10, 20, 30)"
        );
    }

    #[test]
    fn has_selector_invalidation_reaches_through_ghost_deques() {
        let groups = vec![
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(520),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(5200),
                    element: UiElement::new("main"),
                    subtree_span: 1,
                }],
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(521),
                elements: Vec::new(),
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(522),
                elements: Vec::new(),
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(523),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(5203),
                    element: UiElement::new("article"),
                    subtree_span: 1,
                }],
            },
        ];
        let mut ui = UiModel::from_initial_groups(groups).unwrap();
        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("main:has(> article) { color: rgb(20, 80, 120); }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(
            style_value(&ui, ElementHandle(5200), "color"),
            "rgb(20, 80, 120)"
        );

        ui.remove_subtree(ElementHandle(5203)).unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(
            style_value(&ui, ElementHandle(5200), "color"),
            "rgb(0, 0, 0)"
        );
    }

    #[test]
    fn stylo_matches_sibling_selectors_between_document_roots_under_a_ghost_root() {
        let groups = vec![
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(600),
                elements: Vec::new(),
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(601),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(6001),
                    element: UiElement::new("article"),
                    subtree_span: 1,
                }],
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(602),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(6002),
                    element: UiElement::new("aside"),
                    subtree_span: 1,
                }],
            },
        ];
        let mut ui = UiModel::from_initial_groups(groups).unwrap();
        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(
            ":root { color: rgb(90, 30, 10); } article + aside { background-color: rgb(10, 20, 30); }",
        );

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(
            style_value(&ui, ElementHandle(6001), "color"),
            "rgb(90, 30, 10)"
        );
        assert_eq!(
            style_value(&ui, ElementHandle(6002), "color"),
            "rgb(0, 0, 0)"
        );
        assert_eq!(
            style_value(&ui, ElementHandle(6002), "background-color"),
            "rgb(10, 20, 30)"
        );
    }

    #[test]
    fn structural_sibling_removal_invalidates_relative_has_anchor() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();
        let anchor = ui.create_element(root, "span", End::Front).unwrap();
        let target = ui.create_element(root, "span", End::Front).unwrap();
        ui.set_property(anchor, "class", "anchor").unwrap();
        ui.set_property(target, "class", "active").unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("main > .anchor:has(+ .active) { color: red; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(style_value(&ui, anchor, "color"), "rgb(255, 0, 0)");

        ui.remove_subtree(target).unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(style_value(&ui, anchor, "color"), "rgb(0, 0, 0)");
    }

    #[test]
    fn structural_root_removal_invalidates_sibling_selector_across_ghost_root() {
        let groups = vec![
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(610),
                elements: Vec::new(),
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(611),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(6101),
                    element: UiElement::new("article"),
                    subtree_span: 1,
                }],
            },
            crate::runtime::ui::InitialDeque {
                handle: crate::runtime::ui::DequeHandle(612),
                elements: vec![crate::runtime::ui::InitialElement {
                    handle: ElementHandle(6102),
                    element: UiElement::new("aside"),
                    subtree_span: 1,
                }],
            },
        ];
        let mut ui = UiModel::from_initial_groups(groups).unwrap();
        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("article + aside { background-color: red; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(
            style_value(&ui, ElementHandle(6102), "background-color"),
            "rgb(255, 0, 0)"
        );

        ui.remove_subtree(ElementHandle(6101)).unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(
            style_value(&ui, ElementHandle(6102), "background-color"),
            "rgba(0, 0, 0, 0)"
        );
    }

    #[test]
    fn stylo_inherits_computed_values_through_the_deque_projection() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let parent = ui.create_element(root, "section", End::Back).unwrap();
        let child = ui.create_element(root, "span", End::Front).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("section { color: rgb(12, 34, 56); }");

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(
            style_value(&ui, child, "color"),
            style_value(&ui, parent, "color"),
            "child should inherit color from its logical Deque parent"
        );
    }

    #[test]
    fn selector_flags_are_reset_only_for_nodes_touched_by_selector_matching() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let parent = ui.create_element(root, "main", End::Back).unwrap();
        let child = ui.create_element(root, "span", End::Front).unwrap();
        ui.set_property(child, "class", "active").unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("main:has(.active) { color: red; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert!(stylo.last_recomputed_count() >= 2);

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(stylo.last_recomputed_count(), 0);
        assert!(stylo.last_selector_flag_reset_count() > 0);
        assert!(stylo.last_selector_flag_reset_count() <= 2);
        assert_eq!(style_value(&ui, parent, "color"), "rgb(255, 0, 0)");
    }

    #[test]
    fn attribute_mutation_without_sibling_dependencies_does_not_recompute_siblings() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let parent = ui.create_element(root, "main", End::Back).unwrap();
        let first = ui.create_element(root, "span", End::Front).unwrap();
        let second = ui.create_element(root, "span", End::Front).unwrap();
        let third = ui.create_element(root, "span", End::Front).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(".active { color: red; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        ui.set_property(first, "class", "active").unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(style_value(&ui, first, "color"), "rgb(255, 0, 0)");
        assert_eq!(stylo.last_recomputed_count(), 2);
        assert!(ui.element(parent).is_some());
        assert!(ui.element(second).is_some());
        assert!(ui.element(third).is_some());
    }

    #[test]
    fn relative_sibling_has_dependency_invalidates_the_anchor() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let parent = ui.create_element(root, "main", End::Back).unwrap();
        let anchor = ui.create_element(root, "span", End::Front).unwrap();
        let target = ui.create_element(root, "span", End::Front).unwrap();
        ui.set_property(anchor, "class", "anchor").unwrap();
        ui.set_property(target, "class", "idle").unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("main > .anchor:has(+ .active) { color: red; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(style_value(&ui, anchor, "color"), "rgb(0, 0, 0)");

        ui.set_property(target, "class", "active").unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(style_value(&ui, anchor, "color"), "rgb(255, 0, 0)");
        assert!(stylo.last_recomputed_count() >= 2);
        assert!(ui.element(parent).is_some());
    }

    #[test]
    fn dirty_style_order_respects_dirty_ancestors_across_clean_intermediates() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let ancestor = ui.create_element(root, "main", End::Back).unwrap();
        let middle_deque = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let middle = ui
            .create_element(middle_deque, "section", End::Back)
            .unwrap();
        let leaf_deque = ui
            .create_child_deque(middle_deque, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let leaf = ui.create_element(leaf_deque, "span", End::Back).unwrap();
        assert_eq!(ui.logical_parent(leaf), Some(middle));
        assert_eq!(ui.logical_parent(middle), Some(ancestor));

        let dirty = HashSet::from([leaf, ancestor]);
        let ordered = parent_first_dirty_order(&ui.projection(), &dirty);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0], ancestor);
        assert_eq!(ordered[1], leaf);
    }

    #[test]
    fn unchanged_second_pass_recomputes_zero_elements() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let parent = ui.create_element(root, "section", End::Back).unwrap();
        let child = ui.create_element(root, "span", End::Front).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("section { color: red; } span { display: inline-block; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(stylo.last_recomputed_count(), 2);
        assert_eq!(style_value(&ui, child, "display"), "inline-block");

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(stylo.last_recomputed_count(), 0);
        assert_eq!(style_value(&ui, parent, "color"), "rgb(255, 0, 0)");
    }

    #[test]
    fn inherited_change_invalidates_descendants_but_unrelated_branch_stays_cached() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();

        let left = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let right = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Right)
            .unwrap();
        let left_child = ui.create_element(left, "article", End::Back).unwrap();
        let right_child = ui.create_element(right, "aside", End::Back).unwrap();

        let left_leaf_deque = ui
            .create_child_deque(left, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let right_leaf_deque = ui
            .create_child_deque(right, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let left_leaf = ui
            .create_element(left_leaf_deque, "span", End::Back)
            .unwrap();
        let right_leaf = ui
            .create_element(right_leaf_deque, "span", End::Back)
            .unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(".changed { color: rgb(200, 10, 20); }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(stylo.last_recomputed_count(), 5);
        assert_eq!(
            style_value(&ui, left_leaf, "color"),
            style_value(&ui, right_leaf, "color")
        );

        ui.set_property(left_child, "class", "changed").unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(stylo.last_checked_deque_count(), 1);
        assert_eq!(stylo.last_hash_changed_deque_count(), 0);
        assert_eq!(style_value(&ui, left_child, "color"), "rgb(200, 10, 20)");
        assert_eq!(style_value(&ui, left_leaf, "color"), "rgb(200, 10, 20)");
        assert_ne!(style_value(&ui, right_leaf, "color"), "rgb(200, 10, 20)");
        assert!(stylo.last_recomputed_count() < 5);
        assert!(stylo.last_recomputed_count() > 0);

        let _ = right_child;
    }

    #[test]
    fn css_value_mutation_uses_deque_map_without_changing_identity_hash() {
        use crate::deque_tree::DequeMapField;

        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let item = ui.create_element(root, "article", End::Back).unwrap();
        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(".active { color: red; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        let deque_id = ui.deque_id(root).unwrap();
        let hash_before = match ui.registry().get(deque_id).unwrap()[1] {
            DequeMapField::IdentityHash(value) => value,
            _ => unreachable!(),
        };

        ui.set_property(item, "class", "active").unwrap();
        let hash_after = match ui.registry().get(deque_id).unwrap()[1] {
            DequeMapField::IdentityHash(value) => value,
            _ => unreachable!(),
        };
        assert_eq!(
            hash_before, hash_after,
            "CSS values are not structural identity"
        );

        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(stylo.last_checked_deque_count(), 1);
        assert_eq!(stylo.last_hash_changed_deque_count(), 0);
        assert_eq!(style_value(&ui, item, "color"), "rgb(255, 0, 0)");
    }

    #[test]
    fn inserting_and_removing_descendants_invalidates_ancestor_has_selectors() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let main = ui.create_element(root, "main", End::Back).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("main:has(.active) { color: rgb(40, 80, 160); }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_ne!(style_value(&ui, main, "color"), "rgb(40, 80, 160)");

        let inserted = ui.create_element(root, "span", End::Front).unwrap();
        ui.set_property(inserted, "class", "active").unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(style_value(&ui, main, "color"), "rgb(40, 80, 160)");

        ui.remove_subtree(inserted).unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_ne!(style_value(&ui, main, "color"), "rgb(40, 80, 160)");
        assert_eq!(stylo.last_hash_changed_deque_count(), 1);
    }

    #[test]
    fn text_mutation_invalidates_empty_pseudo_on_its_parent() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let parent = ui.create_element(root, "main", End::Back).unwrap();
        let text = ui.create_text(root, "", End::Front).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("main:empty { color: red; } main:not(:empty) { color: blue; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(style_value(&ui, parent, "color"), "rgb(255, 0, 0)");

        ui.set_text(text, "now non-empty").unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(stylo.last_checked_deque_count(), 1);
        assert_eq!(stylo.last_hash_changed_deque_count(), 0);
        assert_eq!(style_value(&ui, parent, "color"), "rgb(0, 0, 255)");
    }

    #[test]
    fn no_op_property_and_style_mutations_do_not_create_revisions() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let item = ui.create_element(root, "div", End::Back).unwrap();
        ui.set_property(item, "class", "same").unwrap();
        ui.set_style(item, "color", "red").unwrap();

        let before = ui.projection().mutation_revision();
        ui.set_property(item, "class", "same").unwrap();
        ui.set_style(item, "color", "red").unwrap();
        let after = ui.projection().mutation_revision();

        assert_eq!(before, after);
    }

    #[test]
    fn structural_move_uses_only_source_and_target_hash_entries_and_notifies_renderer() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();
        let left = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let right = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Right)
            .unwrap();
        let moving = ui.create_element(left, "article", End::Back).unwrap();
        let right_item = ui.create_element(right, "aside", End::Back).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("article { color: red; } aside { color: blue; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        let left_id = ui.deque_id(left).unwrap();
        let right_id = ui.deque_id(right).unwrap();
        ui.move_subtree(moving, right, End::Back).unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(stylo.last_checked_deque_count(), 2);
        assert_eq!(stylo.last_hash_changed_deque_count(), 2);
        assert_eq!(style_value(&ui, moving, "color"), "rgb(255, 0, 0)");
        assert_eq!(style_value(&ui, right_item, "color"), "rgb(0, 0, 255)");

        let frame = ui.prepare_render();
        assert_eq!(frame.changed().len(), 2);
        assert!(frame.changed().contains(&left_id));
        assert!(frame.changed().contains(&right_id));
    }

    #[test]
    fn non_inherited_parent_style_changes_do_not_recompute_descendants() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();
        let left = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let right = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Right)
            .unwrap();
        let trigger = ui.create_element(left, "section", End::Back).unwrap();
        let target = ui.create_element(right, "section", End::Back).unwrap();
        let child_deque = ui
            .create_child_deque(right, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let child = ui.create_element(child_deque, "span", End::Back).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(".trigger + section { background-color: rgb(10, 20, 30); }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        ui.set_property(trigger, "class", "trigger").unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(
            style_value(&ui, target, "background-color"),
            "rgb(10, 20, 30)"
        );
        assert_eq!(stylo.last_recomputed_count(), 3);
        assert_eq!(style_value(&ui, child, "color"), "rgb(0, 0, 0)");
    }

    #[test]
    fn inherited_style_changes_propagate_through_multiple_clean_levels_in_one_pass() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();
        let left = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let right = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Right)
            .unwrap();
        let trigger = ui.create_element(left, "section", End::Back).unwrap();
        let target = ui.create_element(right, "section", End::Back).unwrap();
        let target_children = ui
            .create_child_deque(right, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let child = ui
            .create_element(target_children, "span", End::Back)
            .unwrap();
        let grandchild_deque = ui
            .create_child_deque(target_children, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let grandchild = ui
            .create_element(grandchild_deque, "em", End::Back)
            .unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(".trigger + section { color: rgb(40, 80, 160); }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        let initial_grandchild_color = style_value(&ui, grandchild, "color");

        ui.set_property(trigger, "class", "trigger").unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        let expected = "rgb(40, 80, 160)";
        assert_eq!(style_value(&ui, target, "color"), expected);
        assert_eq!(style_value(&ui, child, "color"), expected);
        assert_ne!(initial_grandchild_color, expected);
        assert_eq!(style_value(&ui, grandchild, "color"), expected);
        assert_eq!(stylo.last_recomputed_count(), 5);
    }

    #[test]
    fn deep_descendant_mutation_invalidates_ancestor_has_selectors() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let main = ui.create_element(root, "main", End::Back).unwrap();
        let section_deque = ui
            .create_child_deque(root, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let section = ui
            .create_element(section_deque, "section", End::Back)
            .unwrap();
        let span_deque = ui
            .create_child_deque(section_deque, crate::deque_tree::ChildSide::Left)
            .unwrap();
        let span = ui.create_element(span_deque, "span", End::Back).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(
            "main:has(.changed) { color: rgb(40, 80, 160); } main .changed { color: rgb(9, 8, 7); } .changed { background-color: rgb(10, 20, 30); }",
        );
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_ne!(style_value(&ui, main, "color"), "rgb(40, 80, 160)");

        ui.set_property(span, "class", "changed").unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(
            style_value(&ui, span, "background-color"),
            "rgb(10, 20, 30)"
        );
        assert_eq!(style_value(&ui, span, "color"), "rgb(9, 8, 7)");
        assert_eq!(style_value(&ui, main, "color"), "rgb(40, 80, 160)");
        assert_eq!(style_value(&ui, section, "color"), "rgb(40, 80, 160)");
    }

    #[test]
    fn adding_stylesheet_invalidates_the_complete_projection() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();
        let child = ui.create_element(root, "span", End::Front).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(stylo.last_recomputed_count(), 2);

        stylo.add_stylesheet("span { color: purple; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(stylo.last_recomputed_count(), 2);
        assert_eq!(style_value(&ui, child, "color"), "rgb(128, 0, 128)");
    }

    #[test]
    fn stylesheet_recalculation_is_bounded_across_frames() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let mut elements = Vec::with_capacity(300);
        for _ in 0..300 {
            elements.push(ui.create_element(root, "div", End::Back).unwrap());
        }

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("div { color: purple; }");
        let mut adapter = StyleCommitAdapter;

        run_style_pass(&mut stylo, &mut adapter, &mut ui).unwrap();
        assert_eq!(
            stylo.last_recomputed_count(),
            StylesheetWork::ELEMENT_BUDGET
        );
        assert_eq!(ui.computed_styles(elements[0]).unwrap().get("color"), None);

        // A structural mutation while the work is pending restarts the cursor
        // against the current Deque contents, so the inserted node is included.
        let inserted = ui.create_element(root, "div", End::Back).unwrap();

        run_style_pass(&mut stylo, &mut adapter, &mut ui).unwrap();
        assert_eq!(
            stylo.last_recomputed_count(),
            StylesheetWork::ELEMENT_BUDGET
        );
        assert_eq!(ui.computed_styles(elements[0]).unwrap().get("color"), None);

        run_style_pass(&mut stylo, &mut adapter, &mut ui).unwrap();
        assert_eq!(
            stylo.last_recomputed_count(),
            StylesheetWork::ELEMENT_BUDGET
        );
        run_style_pass(&mut stylo, &mut adapter, &mut ui).unwrap();
        assert_eq!(stylo.last_recomputed_count(), 45);
        assert_eq!(style_value(&ui, elements[0], "color"), "rgb(128, 0, 128)");
        assert_eq!(style_value(&ui, inserted, "color"), "rgb(128, 0, 128)");

        stylo.add_stylesheet("div { color: blue; }");
        run_style_pass(&mut stylo, &mut adapter, &mut ui).unwrap();
        assert_eq!(
            stylo.last_recomputed_count(),
            StylesheetWork::ELEMENT_BUDGET
        );
        run_style_pass(&mut stylo, &mut adapter, &mut ui).unwrap();
        assert_eq!(
            stylo.last_recomputed_count(),
            StylesheetWork::ELEMENT_BUDGET
        );
        run_style_pass(&mut stylo, &mut adapter, &mut ui).unwrap();
        assert_eq!(stylo.last_recomputed_count(), 45);
        assert_eq!(style_value(&ui, elements[0], "color"), "rgb(0, 0, 255)");
        assert_eq!(style_value(&ui, inserted, "color"), "rgb(0, 0, 255)");
    }

    #[test]
    fn stylo_exports_computed_box_geometry_without_re_resolving_css() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let element = ui.create_element(root, "section", End::Back).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(
            "section { box-sizing: border-box; position: absolute; width: 120px; height: 48px; min-width: 60px; max-width: 240px; margin: 4px 8px 12px 16px; padding: 2px 4px 6px 8px; border-width: 1px 2px 3px 4px; top: 10px; right: 11px; bottom: 12px; left: 13px; }",
        );
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        let computed = ui.computed_styles(element).unwrap();
        for (property, expected) in [
            ("box-sizing", "border-box"),
            ("position", "absolute"),
            ("width", "120px"),
            ("height", "48px"),
            ("min-width", "60px"),
            ("max-width", "240px"),
            ("margin-top", "4px"),
            ("margin-right", "8px"),
            ("margin-bottom", "12px"),
            ("margin-left", "16px"),
            ("padding-top", "2px"),
            ("padding-right", "4px"),
            ("padding-bottom", "6px"),
            ("padding-left", "8px"),
            ("border-top-width", "1px"),
            ("border-right-width", "2px"),
            ("border-bottom-width", "3px"),
            ("border-left-width", "4px"),
            ("top", "10px"),
            ("right", "11px"),
            ("bottom", "12px"),
            ("left", "13px"),
        ] {
            assert_eq!(
                computed.get(property).map(String::as_str),
                Some(expected),
                "{property}"
            );
        }
    }

    #[test]
    fn stylo_exports_computed_flexbox_inputs_without_re_resolving_css() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let element = ui.create_element(root, "section", End::Back).unwrap();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet(
            "section { direction: rtl; display: flex; flex-direction: row-reverse; flex-wrap: wrap-reverse; flex-grow: 2.5; flex-shrink: 0.25; flex-basis: 35%; row-gap: 12px; column-gap: 4%; }",
        );
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        let computed = ui.computed_styles(element).unwrap();
        for (property, expected) in [
            ("direction", "rtl"),
            ("flex-direction", "row-reverse"),
            ("flex-wrap", "wrap-reverse"),
            ("flex-grow", "2.5"),
            ("flex-shrink", "0.25"),
            ("flex-basis", "35%"),
            ("row-gap", "12px"),
            ("column-gap", "4%"),
        ] {
            assert_eq!(
                computed.get(property).map(String::as_str),
                Some(expected),
                "{property}"
            );
        }
    }

    #[test]
    fn stylo_does_not_change_deque_topology_or_element_identity() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let parent = ui.create_element(root, "div", End::Back).unwrap();
        let child = ui.create_element(root, "span", End::Front).unwrap();
        let before_parent = ui.logical_parent(child);
        let before_order = ui.projection().handles();

        let mut stylo = StyloStyleEngine::new().unwrap();
        stylo.add_stylesheet("div { color: purple; }");
        run_style_pass(&mut stylo, &mut StyleCommitAdapter, &mut ui).unwrap();

        assert_eq!(ui.logical_parent(child), before_parent);
        assert_eq!(ui.projection().handles(), before_order);
        assert_eq!(ui.logical_parent(child), Some(parent));
    }
}
