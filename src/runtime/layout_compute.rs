use super::layout::{
    LayoutNodeId, LayoutNodeIndex, LayoutStyleError, PersistentTaffyTreeProjection,
    taffy_style_from_computed,
};
use super::text::{FontAttributes, TextSystem};
use super::ui::{ComputedStyleConsumer, ElementHandle, LogicalProjection, UiModel, UiMutation, UiMutationKind};
use std::collections::{HashMap, HashSet};
use taffy::{
    AvailableSpace, Cache, CacheTree, Layout, LayoutInput, LayoutOutput, Size,
    compute_cached_layout, compute_flexbox_layout, compute_hidden_layout, compute_leaf_layout,
    compute_oof_layout, compute_root_layout, round_layout,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutComputeError {
    MissingRoot,
    UnknownElement(ElementHandle),
    NodeIdSpaceExhausted,
    InvalidViewport,
    UnsupportedNode { handle: ElementHandle, reason: &'static str },
    InvalidStyle { handle: ElementHandle, source: LayoutStyleError },
}

struct ElementLayoutState {
    style: taffy::Style,
    text_content: Option<String>,
    font_size: f32,
    font_family: String,
    font_weight: u16,
    font_style: String,
    font_stretch: String,
    line_height: f32,
    cache: Cache,
    unrounded: Layout,
    final_layout: Layout,
    hoisted: Vec<taffy::NodeId>,
    cache_invalidations: u64,
}

impl ElementLayoutState {
    fn new(style: taffy::Style) -> Self {
        Self {
            style, text_content: None, font_size: 16.0,
            font_family: "sans-serif".into(), font_weight: 400,
            font_style: "normal".into(), font_stretch: "100%".into(),
            line_height: 19.2, cache: Cache::new(),
            unrounded: Layout::with_order(0), final_layout: Layout::with_order(0),
            hoisted: Vec::new(), cache_invalidations: 0,
        }
    }
}

fn layout_state_from_element(
    projection: &LogicalProjection<'_>,
    handle: ElementHandle,
) -> Result<ElementLayoutState, LayoutComputeError> {
    let element = projection.element(handle).ok_or(LayoutComputeError::UnknownElement(handle))?;
    let inherited = if element.kind == super::ui::UiElementKind::Text {
        projection.parent_of(handle)
            .and_then(|parent| projection.element(parent))
            .map(|parent| &parent.computed_styles)
    } else { None };
    let computed = inherited.unwrap_or(&element.computed_styles);
    let style = if element.kind == super::ui::UiElementKind::Text {
        taffy::Style { display: taffy::Display::Block, ..taffy::Style::default() }
    } else {
        taffy_style_from_computed(computed)
            .map_err(|source| LayoutComputeError::InvalidStyle { handle, source })?
    };

    let mut hidden = false;
    let mut ancestor = projection.parent_of(handle);
    while let Some(parent) = ancestor {
        if projection.element(parent)
            .and_then(|parent| parent.computed_styles.get("display"))
            .is_some_and(|display| display == "none") {
            hidden = true;
            break;
        }
        ancestor = projection.parent_of(parent);
    }
    if !projection.children_of(handle).is_empty()
        && style.display != taffy::Display::Flex
        && style.display != taffy::Display::None
        && !hidden
    {
        return Err(LayoutComputeError::UnsupportedNode {
            handle,
            reason: "only flex containers may have children in this layout milestone",
        });
    }

    let mut state = ElementLayoutState::new(style);
    if element.kind == super::ui::UiElementKind::Text {
        state.text_content = Some(element.text_content().unwrap_or_default().to_owned());
        state.font_family = computed.get("font-family").cloned().unwrap_or_else(|| "sans-serif".into());
        state.font_weight = computed.get("font-weight")
            .and_then(|v| v.parse::<u16>().ok()).unwrap_or(400).clamp(1, 1000);
        state.font_style = computed.get("font-style").cloned().unwrap_or_else(|| "normal".into());
        state.font_stretch = computed.get("font-stretch").cloned().unwrap_or_else(|| "100%".into());
        state.font_size = computed.get("font-size").and_then(|v| v.strip_suffix("px"))
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0).unwrap_or(16.0);
        state.line_height = computed.get("line-height").map(String::as_str).and_then(|v| {
            if v == "normal" { Some(state.font_size * 1.2) }
            else if let Some(px) = v.strip_suffix("px") { px.trim().parse::<f32>().ok() }
            else { v.parse::<f32>().ok().map(|scale| state.font_size * scale) }
        }).filter(|v| v.is_finite() && *v > 0.0).unwrap_or(state.font_size * 1.2);
    }
    Ok(state)
}

fn property_affects_layout(property: &str) -> bool {
    matches!(property,
        "display" | "box-sizing" | "position" | "direction" | "writing-mode"
        | "width" | "height" | "min-width" | "min-height" | "max-width" | "max-height"
        | "margin-top" | "margin-right" | "margin-bottom" | "margin-left"
        | "padding-top" | "padding-right" | "padding-bottom" | "padding-left"
        | "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width"
        | "top" | "right" | "bottom" | "left"
        | "flex-direction" | "flex-wrap" | "flex-grow" | "flex-shrink" | "flex-basis"
        | "row-gap" | "column-gap"
        | "grid-template-columns" | "grid-template-rows" | "grid-template-areas"
        | "grid-auto-columns" | "grid-auto-rows" | "grid-auto-flow" | "grid-column" | "grid-row"
        | "align-content" | "align-items" | "align-self" | "justify-content" | "justify-items"
        | "justify-self" | "place-content" | "place-items" | "place-self"
        | "font-family" | "font-size" | "font-weight" | "font-style" | "font-stretch"
        | "line-height" | "letter-spacing" | "word-spacing" | "white-space"
        | "text-indent" | "text-transform" | "word-break" | "overflow-wrap" | "hyphens" | "tab-size"
    )
}

fn viewport_key(viewport: Size<f32>) -> Result<(u32, u32), LayoutComputeError> {
    if !viewport.width.is_finite() || !viewport.height.is_finite()
        || viewport.width < 0.0 || viewport.height < 0.0 {
        return Err(LayoutComputeError::InvalidViewport);
    }
    Ok((viewport.width.to_bits(), viewport.height.to_bits()))
}

/// Persistent derived layout state. No UI elements or child relationships
/// are owned here; those remain in the canonical Deque Tree.
pub struct NativeLayoutEngine {
    node_index: LayoutNodeIndex,
    states: HashMap<ElementHandle, ElementLayoutState>,
    initialized: bool,
    last_ui_mutation_revision: u64,
    last_root: Option<ElementHandle>,
    last_viewport: Option<(u32, u32)>,
}

impl Default for NativeLayoutEngine {
    fn default() -> Self { Self::new() }
}

impl NativeLayoutEngine {
    pub fn new() -> Self {
        Self {
            node_index: LayoutNodeIndex::new(), states: HashMap::new(),
            initialized: false, last_ui_mutation_revision: 0,
            last_root: None, last_viewport: None,
        }
    }

    pub fn compute(
        &mut self,
        ui: &mut UiModel,
        text_system: &mut TextSystem,
        root: ElementHandle,
        viewport: Size<f32>,
    ) -> Result<(), LayoutComputeError> {
        if ui.element(root).is_none() {
            return Err(LayoutComputeError::UnknownElement(root));
        }
        let viewport = viewport_key(viewport)?;
        if !self.initialized {
            ui.register_layout_mutation_consumer();
        }

        let (ui_revision, style_revision) = {
            let projection = ui.projection();
            let ui_revision = projection.mutation_revision();
            let style_revision = projection.computed_style_revision();
            let mut layout_required = !self.initialized
                || self.last_root != Some(root)
                || self.last_viewport != Some(viewport);

            if !self.initialized {
                let handles = projection.handles();
                let mut index = LayoutNodeIndex::new();
                let mut states = HashMap::with_capacity(handles.len());
                for handle in handles {
                    index.insert(handle).map_err(|()| LayoutComputeError::NodeIdSpaceExhausted)?;
                    states.insert(handle, layout_state_from_element(&projection, handle)?);
                }
                self.node_index = index;
                self.states = states;
                self.initialized = true;
            } else {
                let mutations = projection.mutations_since(self.last_ui_mutation_revision)
                    .cloned().collect::<Vec<_>>();
                let style_changes = projection.computed_style_changes_for(ComputedStyleConsumer::Layout)
                    .cloned().collect::<Vec<_>>();
                for mutation in &mutations {
                    layout_required |= self.apply_ui_mutation(&projection, mutation)?;
                }
                for change in &style_changes {
                    if change.changed_properties.iter().any(|p| property_affects_layout(p))
                        && projection.element(change.element).is_some() {
                        self.refresh_subtree(&projection, change.element)?;
                        self.invalidate_path(&projection, change.element);
                        layout_required = true;
                    }
                }
            }

            if self.last_root != Some(root) || self.last_viewport != Some(viewport) {
                self.invalidate_path(&projection, root);
            }
            if layout_required {
                let persistent = PersistentTaffyTreeProjection::new(projection, &self.node_index);
                let root_id = persistent.node_id(root)
                    .ok_or(LayoutComputeError::UnknownElement(root))?.0;
                let mut pass = NativeLayoutPass {
                    projection: persistent, text_system, states: &mut self.states,
                };
                compute_root_layout(&mut pass, root_id, Size {
                    width: AvailableSpace::Definite(f32::from_bits(viewport.0)),
                    height: AvailableSpace::Definite(f32::from_bits(viewport.1)),
                });
                round_layout(&mut pass, root_id);
            }
            (ui_revision, style_revision)
        };

        self.last_ui_mutation_revision = ui_revision;
        self.last_root = Some(root);
        self.last_viewport = Some(viewport);
        ui.acknowledge_layout_mutations(ui_revision);
        ui.acknowledge_computed_style_changes(ComputedStyleConsumer::Layout, style_revision);
        Ok(())
    }

    fn apply_ui_mutation(
        &mut self,
        projection: &LogicalProjection<'_>,
        mutation: &UiMutation,
    ) -> Result<bool, LayoutComputeError> {
        match mutation.kind {
            UiMutationKind::Text => {
                if let Some(handle) = mutation.element {
                    self.refresh_subtree(projection, handle)?;
                    self.invalidate_path(projection, mutation.new_parent.unwrap_or(handle));
                }
                Ok(true)
            }
            UiMutationKind::Attribute | UiMutationKind::InlineStyle => Ok(false),
            UiMutationKind::Insert => {
                for handle in &mutation.affected { self.refresh_subtree(projection, *handle)?; }
                if let Some(context) = mutation.new_parent.or(mutation.element) {
                    self.invalidate_path(projection, context);
                }
                Ok(true)
            }
            UiMutationKind::Remove => {
                for handle in &mutation.affected {
                    self.states.remove(handle);
                    self.node_index.remove(*handle);
                }
                if let Some(parent) = mutation.old_parent { self.invalidate_path(projection, parent); }
                Ok(true)
            }
            UiMutationKind::Move => {
                if let Some(anchor) = mutation.element { self.refresh_subtree(projection, anchor)?; }
                else { for handle in &mutation.affected { self.refresh_subtree(projection, *handle)?; } }
                if let Some(parent) = mutation.old_parent { self.invalidate_path(projection, parent); }
                if let Some(parent) = mutation.new_parent { self.invalidate_path(projection, parent); }
                Ok(true)
            }
        }
    }

    fn refresh_subtree(
        &mut self,
        projection: &LogicalProjection<'_>,
        root: ElementHandle,
    ) -> Result<(), LayoutComputeError> {
        let mut pending = vec![root];
        let mut visited = HashSet::new();
        while let Some(handle) = pending.pop() {
            if !visited.insert(handle) || projection.element(handle).is_none() { continue; }
            self.node_index.insert(handle).map_err(|()| LayoutComputeError::NodeIdSpaceExhausted)?;
            let mut next = layout_state_from_element(projection, handle)?;
            next.cache_invalidations = self.states.get(&handle)
                .map_or(0, |old| old.cache_invalidations.saturating_add(1));
            self.states.insert(handle, next);
            let mut children = projection.children_of(handle);
            children.reverse();
            pending.extend(children);
        }
        Ok(())
    }

    fn invalidate_path(&mut self, projection: &LogicalProjection<'_>, start: ElementHandle) {
        let mut current = Some(start);
        while let Some(handle) = current {
            if let Some(state) = self.states.get_mut(&handle) {
                state.cache.clear();
                state.cache_invalidations = state.cache_invalidations.saturating_add(1);
            }
            current = projection.parent_of(handle);
        }
    }

    pub fn layout(&self, handle: ElementHandle) -> Option<Layout> {
        self.states.get(&handle).map(|state| state.final_layout)
    }
    pub fn node_id(&self, handle: ElementHandle) -> Option<LayoutNodeId> {
        self.node_index.node_id(handle).map(LayoutNodeId)
    }
    pub fn state_count(&self) -> usize { self.states.len() }
    pub fn node_count(&self) -> usize { self.node_index.len() }
    pub fn cache_invalidation_count(&self, handle: ElementHandle) -> Option<u64> {
        self.states.get(&handle).map(|state| state.cache_invalidations)
    }
}

struct NativeLayoutPass<'projection, 'index, 'states, 'text> {
    projection: PersistentTaffyTreeProjection<'projection, 'index>,
    text_system: &'text mut TextSystem,
    states: &'states mut HashMap<ElementHandle, ElementLayoutState>,
}

impl NativeLayoutPass<'_, '_, '_, '_> {
    fn handle(&self, node_id: taffy::NodeId) -> ElementHandle {
        self.projection.handle_for_taffy_id(node_id)
    }
    fn state(&self, node_id: taffy::NodeId) -> &ElementLayoutState {
        &self.states[&self.handle(node_id)]
    }
    fn state_mut(&mut self, node_id: taffy::NodeId) -> &mut ElementLayoutState {
        let handle = self.handle(node_id);
        self.states.get_mut(&handle).expect("layout state exists for persistent node id")
    }
}

impl taffy::TraversePartialTree for NativeLayoutPass<'_, '_, '_, '_> {
    type ChildIter<'b>
        = std::vec::IntoIter<taffy::NodeId>
    where
        Self: 'b;

    fn child_ids(&self, parent_node_id: taffy::NodeId) -> Self::ChildIter<'_> {
        self.projection
            .projection()
            .children_of(self.handle(parent_node_id))
            .into_iter()
            .map(|handle| self.projection.node_id(handle).expect("canonical child has a persistent layout ID").0)
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn child_count(&self, parent_node_id: taffy::NodeId) -> usize {
        self.projection
            .projection()
            .children_of(self.handle(parent_node_id))
            .len()
    }

    fn get_child_id(&self, parent_node_id: taffy::NodeId, child_index: usize) -> taffy::NodeId {
        let parent = self.handle(parent_node_id);
        let child = self.projection.projection().children_of(parent)[child_index];
        self.projection.node_id(child).expect("canonical child has a persistent layout ID").0
    }
}

impl taffy::TraverseTree for NativeLayoutPass<'_, '_, '_, '_> {}

impl taffy::LayoutPartialTree for NativeLayoutPass<'_, '_, '_, '_> {
    type CustomIdent = String;
    type CoreContainerStyle<'b>
        = &'b taffy::Style
    where
        Self: 'b;

    fn get_core_container_style(&self, node_id: taffy::NodeId) -> Self::CoreContainerStyle<'_> {
        &self.state(node_id).style
    }

    fn set_unrounded_layout(&mut self, node_id: taffy::NodeId, layout: &Layout) {
        self.state_mut(node_id).unrounded = *layout;
    }

    fn compute_child_layout(
        &mut self,
        node_id: taffy::NodeId,
        inputs: LayoutInput,
    ) -> LayoutOutput {
        if inputs.run_mode == taffy::RunMode::PerformHiddenLayout
            || self.state(node_id).style.display == taffy::Display::None
        {
            return compute_hidden_layout(self, node_id);
        }

        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            let style = tree.state(node_id).style.clone();
            let mut output = if style.display == taffy::Display::Flex {
                compute_flexbox_layout(tree, node_id, inputs)
            } else {
                let text_content = tree.state(node_id).text_content.clone();
                let font_size = tree.state(node_id).font_size;
                let font_family = tree.state(node_id).font_family.clone();
                let font_weight = tree.state(node_id).font_weight;
                let font_style = tree.state(node_id).font_style.clone();
                let font_stretch = tree.state(node_id).font_stretch.clone();
                let line_height = tree.state(node_id).line_height;
                compute_leaf_layout(
                    inputs,
                    &style,
                    |_value, _basis| 0.0,
                    |known_dimensions, available_space| {
                        if let Some(text) = text_content.as_deref() {
                            let max_width = match available_space.width {
                                AvailableSpace::Definite(width) => Some(width),
                                AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
                            };
                            let (measured_width, measured_height) = tree.text_system.measure(
                                text,
                                max_width,
                                font_size,
                                FontAttributes {
                                    family: &font_family,
                                    weight: font_weight,
                                    style: &font_style,
                                    stretch: &font_stretch,
                                },
                                line_height,
                            );
                            Size {
                                width: known_dimensions.width.unwrap_or(measured_width),
                                height: known_dimensions.height.unwrap_or(measured_height),
                            }
                        } else {
                            Size {
                                width: known_dimensions.width.unwrap_or(0.0),
                                height: known_dimensions.height.unwrap_or(0.0),
                            }
                        }
                    },
                )
            };

            if inputs.run_mode == taffy::RunMode::PerformLayout {
                compute_oof_layout(tree, node_id, &mut output);
            }
            output
        })
    }
}

impl CacheTree for NativeLayoutPass<'_, '_, '_, '_> {
    fn cache_get(&self, node_id: taffy::NodeId, inputs: &LayoutInput) -> Option<LayoutOutput> {
        self.state(node_id).cache.get(inputs)
    }

    fn cache_store(&mut self, node_id: taffy::NodeId, inputs: &LayoutInput, output: LayoutOutput) {
        self.state_mut(node_id).cache.store(inputs, output);
    }

    fn cache_clear(&mut self, node_id: taffy::NodeId) {
        self.state_mut(node_id).cache.clear();
    }
}

impl taffy::LayoutContainingBlock for NativeLayoutPass<'_, '_, '_, '_> {
    type OofItemStyle<'b>
        = &'b taffy::Style
    where
        Self: 'b;

    fn get_oof_item_style(&self, node_id: taffy::NodeId) -> Self::OofItemStyle<'_> {
        &self.state(node_id).style
    }

    fn clear_hoisted_children(&mut self, node_id: taffy::NodeId) {
        self.state_mut(node_id).hoisted.clear();
    }

    fn add_hoisted_children(&mut self, node_id: taffy::NodeId, hoisted: &[taffy::NodeId]) {
        self.state_mut(node_id).hoisted.extend_from_slice(hoisted);
    }
}

impl taffy::LayoutFlexboxContainer for NativeLayoutPass<'_, '_, '_, '_> {
    type FlexboxContainerStyle<'b>
        = &'b taffy::Style
    where
        Self: 'b;
    type FlexboxItemStyle<'b>
        = &'b taffy::Style
    where
        Self: 'b;

    fn get_flexbox_container_style(
        &self,
        node_id: taffy::NodeId,
    ) -> Self::FlexboxContainerStyle<'_> {
        &self.state(node_id).style
    }

    fn get_flexbox_child_style(&self, child_node_id: taffy::NodeId) -> Self::FlexboxItemStyle<'_> {
        &self.state(child_node_id).style
    }
}

impl taffy::RoundTree for NativeLayoutPass<'_, '_, '_, '_> {
    fn get_unrounded_layout(&self, node_id: taffy::NodeId) -> Layout {
        self.state(node_id).unrounded
    }

    fn set_final_layout(&mut self, node_id: taffy::NodeId, layout: &Layout) {
        self.state_mut(node_id).final_layout = *layout;
    }

    fn is_out_of_flow(&self, node_id: taffy::NodeId) -> bool {
        self.state(node_id).style.position.is_out_of_flow()
    }

    fn hoisted_child_count(&self, node_id: taffy::NodeId) -> usize {
        self.state(node_id).hoisted.len()
    }

    fn get_hoisted_child_id(&self, node_id: taffy::NodeId, index: usize) -> taffy::NodeId {
        self.state(node_id).hoisted[index]
    }
}

#[cfg(test)]
mod compute_tests {
    use super::*;
    use crate::deque_tree::End;
    use crate::runtime::ui::{InitialDeque, InitialElement, UiElement, UiModel};

    fn element(tag: &str, display: &str, width: &str, height: &str) -> UiElement {
        let mut element = UiElement::new(tag);
        element.computed_styles = [
            ("display".into(), display.into()),
            ("width".into(), width.into()),
            ("height".into(), height.into()),
        ]
        .into();
        element
    }

    #[test]
    fn flex_layout_computes_real_rectangles_from_canonical_children() {
        let mut ui = UiModel::from_initial_groups(vec![InitialDeque {
            handle: super::super::ui::DequeHandle(90_001),
            elements: vec![
                InitialElement {
                    handle: ElementHandle(90_101),
                    element: element("second", "block", "100px", "20px"),
                    subtree_span: 1,
                },
                InitialElement {
                    handle: ElementHandle(90_102),
                    element: element("first", "block", "50px", "20px"),
                    subtree_span: 1,
                },
                InitialElement {
                    handle: ElementHandle(90_103),
                    element: {
                        let mut root = element("main", "flex", "300px", "100px");
                        root.computed_styles
                            .insert("flex-direction".into(), "row".into());
                        root.computed_styles
                            .insert("column-gap".into(), "10px".into());
                        root
                    },
                    subtree_span: 3,
                },
            ],
        }])
        .unwrap();

        let root = ElementHandle(90_103);
        let first = ElementHandle(90_102);
        let second = ElementHandle(90_101);
        let mut text_system = TextSystem::new();
        let mut pass = NativeLayoutEngine::new();
        pass.compute(
            &mut ui,
            &mut text_system,
            root,
            Size {
                width: 300.0,
                height: 100.0,
            },
        )
        .unwrap();

        let root_layout = pass.layout(root).unwrap();
        let first_layout = pass.layout(first).unwrap();
        let second_layout = pass.layout(second).unwrap();

        assert_eq!(root_layout.size.width, 300.0);
        assert_eq!(root_layout.size.height, 100.0);
        assert_eq!(first_layout.location.x, 0.0);
        assert_eq!(first_layout.location.y, 0.0);
        assert_eq!(first_layout.size.width, 50.0);
        assert_eq!(second_layout.location.x, 60.0);
        assert_eq!(second_layout.size.width, 100.0);
    }

    #[test]
    fn display_none_hides_the_entire_canonical_subtree_without_affecting_flex_siblings() {
        let mut ui = UiModel::from_initial_groups(vec![InitialDeque {
            handle: super::super::ui::DequeHandle(90_002),
            elements: vec![
                InitialElement {
                    handle: ElementHandle(90_201),
                    element: element("descendant", "block", "80px", "20px"),
                    subtree_span: 1,
                },
                InitialElement {
                    handle: ElementHandle(90_202),
                    element: element("hidden", "none", "100px", "40px"),
                    subtree_span: 2,
                },
                InitialElement {
                    handle: ElementHandle(90_203),
                    element: element("visible", "block", "50px", "20px"),
                    subtree_span: 1,
                },
                InitialElement {
                    handle: ElementHandle(90_204),
                    element: element("root", "flex", "300px", "100px"),
                    subtree_span: 4,
                },
            ],
        }])
        .unwrap();

        let descendant = ElementHandle(90_201);
        let hidden = ElementHandle(90_202);
        let visible = ElementHandle(90_203);
        let root = ElementHandle(90_204);
        let mut text_system = TextSystem::new();
        let mut pass = NativeLayoutEngine::new();
        pass.compute(
            &mut ui,
            &mut text_system,
            root,
            Size {
                width: 300.0,
                height: 100.0,
            },
        )
        .unwrap();

        for handle in [hidden, descendant] {
            let layout = pass.layout(handle).unwrap();
            assert_eq!(layout.size.width, 0.0);
            assert_eq!(layout.size.height, 0.0);
            assert_eq!(layout.location.x, 0.0);
            assert_eq!(layout.location.y, 0.0);
        }

        let visible_layout = pass.layout(visible).unwrap();
        assert_eq!(visible_layout.location.x, 0.0);
        assert_eq!(visible_layout.location.y, 0.0);
        assert_eq!(visible_layout.size.width, 50.0);
        assert_eq!(visible_layout.size.height, 20.0);
    }

    #[test]
    fn text_nodes_are_measured_by_the_native_text_system() {
        let mut ui = UiModel::from_initial_groups(vec![InitialDeque {
            handle: super::super::ui::DequeHandle(90_003),
            elements: vec![
                InitialElement {
                    handle: ElementHandle(90_301),
                    element: UiElement::text("Native text measurement"),
                    subtree_span: 1,
                },
                InitialElement {
                    handle: ElementHandle(90_302),
                    element: {
                        let mut root = element("root", "flex", "300px", "100px");
                        root.computed_styles
                            .insert("font-size".into(), "24px".into());
                        root
                    },
                    subtree_span: 2,
                },
            ],
        }])
        .unwrap();

        let text = ElementHandle(90_301);
        let root = ElementHandle(90_302);
        let mut text_system = TextSystem::new();
        let mut pass = NativeLayoutEngine::new();
        pass.compute(
            &mut ui,
            &mut text_system,
            root,
            Size {
                width: 300.0,
                height: 100.0,
            },
        )
        .unwrap();

        let measured = pass.layout(text).unwrap();
        assert!(measured.size.width > 0.0);
        assert!(measured.size.height > 0.0);
        assert!(measured.size.height >= 24.0);
    }

    #[test]
    fn persistent_engine_reuses_ids_and_skips_unchanged_layout_work() {
        let mut ui = UiModel::new();
        let deque = ui.create_root_deque().unwrap();
        let root = ui.create_element(deque, "main", End::Back).unwrap();
        let child = ui.create_element(deque, "section", End::Front).unwrap();
        ui.replace_computed_styles(root, [
            ("display".into(), "flex".into()), ("width".into(), "300px".into()),
            ("height".into(), "100px".into()),
        ].into()).unwrap();
        ui.replace_computed_styles(child, [
            ("display".into(), "block".into()), ("width".into(), "80px".into()),
            ("height".into(), "20px".into()),
        ].into()).unwrap();
        let viewport = Size { width: 300.0, height: 100.0 };
        let mut text = TextSystem::new();
        let mut engine = NativeLayoutEngine::new();
        engine.compute(&mut ui, &mut text, root, viewport).unwrap();
        let root_id = engine.node_id(root).unwrap();
        let child_id = engine.node_id(child).unwrap();
        let root_invalidations = engine.cache_invalidation_count(root).unwrap();
        let child_invalidations = engine.cache_invalidation_count(child).unwrap();
        let root_layout = engine.layout(root).unwrap();
        let child_layout = engine.layout(child).unwrap();
        engine.compute(&mut ui, &mut text, root, viewport).unwrap();
        assert_eq!(engine.node_id(root), Some(root_id));
        assert_eq!(engine.node_id(child), Some(child_id));
        assert_eq!(engine.state_count(), 2);
        assert_eq!(engine.cache_invalidation_count(root), Some(root_invalidations));
        assert_eq!(engine.cache_invalidation_count(child), Some(child_invalidations));
        assert_eq!(engine.layout(root), Some(root_layout));
        assert_eq!(engine.layout(child), Some(child_layout));
    }

    #[test]
    fn text_mutation_refreshes_measurement_without_changing_node_id() {
        let mut ui = UiModel::new();
        let deque = ui.create_root_deque().unwrap();
        let root = ui.create_element(deque, "main", End::Back).unwrap();
        let text_handle = ui.create_text(deque, "x", End::Front).unwrap();
        ui.replace_computed_styles(root, [
            ("display".into(), "flex".into()), ("width".into(), "600px".into()),
            ("height".into(), "100px".into()), ("font-size".into(), "24px".into()),
        ].into()).unwrap();
        let viewport = Size { width: 600.0, height: 100.0 };
        let mut text = TextSystem::new();
        let mut engine = NativeLayoutEngine::new();
        engine.compute(&mut ui, &mut text, root, viewport).unwrap();
        let node_id = engine.node_id(text_handle).unwrap();
        let before = engine.layout(text_handle).unwrap().size.width;
        let invalidations = engine.cache_invalidation_count(text_handle).unwrap();
        ui.set_text(text_handle, "a much longer native text measurement").unwrap();
        engine.compute(&mut ui, &mut text, root, viewport).unwrap();
        assert!(engine.layout(text_handle).unwrap().size.width > before);
        assert_eq!(engine.node_id(text_handle), Some(node_id));
        assert!(engine.cache_invalidation_count(text_handle).unwrap() > invalidations);
    }

    #[test]
    fn geometry_style_delta_recomputes_but_paint_only_delta_skips_layout() {
        let mut ui = UiModel::new();
        let deque = ui.create_root_deque().unwrap();
        let root = ui.create_element(deque, "main", End::Back).unwrap();
        ui.replace_computed_styles(root, [
            ("display".into(), "flex".into()), ("width".into(), "300px".into()),
            ("height".into(), "100px".into()), ("color".into(), "rgb(255, 0, 0)".into()),
        ].into()).unwrap();
        let viewport = Size { width: 500.0, height: 200.0 };
        let mut text = TextSystem::new();
        let mut engine = NativeLayoutEngine::new();
        engine.compute(&mut ui, &mut text, root, viewport).unwrap();
        let node_id = engine.node_id(root).unwrap();
        let initial_invalidations = engine.cache_invalidation_count(root).unwrap();
        assert_eq!(engine.layout(root).unwrap().size.width, 300.0);

        ui.replace_computed_styles(root, [
            ("display".into(), "flex".into()), ("width".into(), "400px".into()),
            ("height".into(), "100px".into()), ("color".into(), "rgb(255, 0, 0)".into()),
        ].into()).unwrap();
        engine.compute(&mut ui, &mut text, root, viewport).unwrap();
        assert_eq!(engine.layout(root).unwrap().size.width, 400.0);
        let geometry_invalidations = engine.cache_invalidation_count(root).unwrap();
        assert!(geometry_invalidations > initial_invalidations);

        ui.replace_computed_styles(root, [
            ("display".into(), "flex".into()), ("width".into(), "400px".into()),
            ("height".into(), "100px".into()), ("color".into(), "rgb(0, 0, 255)".into()),
        ].into()).unwrap();
        engine.compute(&mut ui, &mut text, root, viewport).unwrap();
        assert_eq!(engine.node_id(root), Some(node_id));
        assert_eq!(engine.cache_invalidation_count(root), Some(geometry_invalidations));
        assert_eq!(engine.layout(root).unwrap().size.width, 400.0);
    }

    #[test]
    fn removing_element_retires_layout_state_and_backend_id() {
        let mut ui = UiModel::new();
        let deque = ui.create_root_deque().unwrap();
        let root = ui.create_element(deque, "main", End::Back).unwrap();
        let child = ui.create_element(deque, "section", End::Front).unwrap();
        ui.replace_computed_styles(root, [
            ("display".into(), "flex".into()), ("width".into(), "300px".into()),
            ("height".into(), "100px".into()),
        ].into()).unwrap();
        ui.replace_computed_styles(child, [
            ("display".into(), "block".into()), ("width".into(), "80px".into()),
            ("height".into(), "20px".into()),
        ].into()).unwrap();
        let mut text = TextSystem::new();
        let mut engine = NativeLayoutEngine::new();
        engine.compute(&mut ui, &mut text, root, Size { width: 300.0, height: 100.0 }).unwrap();
        assert_eq!(engine.state_count(), 2);
        assert!(engine.node_id(child).is_some());
        ui.remove_subtree(child).unwrap();
        engine.compute(&mut ui, &mut text, root, Size { width: 300.0, height: 100.0 }).unwrap();
        assert_eq!(engine.state_count(), 1);
        assert_eq!(engine.layout(child), None);
        assert_eq!(engine.node_id(child), None);
        assert!(engine.layout(root).is_some());
    }

}
