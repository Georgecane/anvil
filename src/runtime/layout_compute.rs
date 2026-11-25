use super::layout::{LayoutStyleError, TaffyTreeProjection, taffy_style_from_computed};
use super::text::{FontAttributes, TextSystem};
use super::ui::{ElementHandle, LogicalProjection, UiElementKind};
use std::collections::HashMap;
use taffy::{
    AvailableSpace, Cache, CacheTree, Layout, LayoutInput, LayoutOutput, Size,
    compute_cached_layout, compute_flexbox_layout, compute_hidden_layout, compute_leaf_layout,
    compute_oof_layout, compute_root_layout, round_layout,
};

/// A failure to construct or execute a native layout pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutComputeError {
    MissingRoot,
    UnknownElement(ElementHandle),
    UnsupportedNode {
        handle: ElementHandle,
        reason: &'static str,
    },
    InvalidStyle {
        handle: ElementHandle,
        source: LayoutStyleError,
    },
}

/// Derived layout data for one canonical element. It is keyed by stable
/// ElementHandle and never owns element topology.
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
}

impl ElementLayoutState {
    fn new(style: taffy::Style) -> Self {
        Self {
            style,
            text_content: None,
            font_size: 16.0,
            font_family: "sans-serif".into(),
            font_weight: 400,
            font_style: "normal".into(),
            font_stretch: "100%".into(),
            line_height: 19.2,
            cache: Cache::new(),
            unrounded: Layout::with_order(0),
            final_layout: Layout::with_order(0),
            hoisted: Vec::new(),
        }
    }
}

/// One-shot native layout computation over the canonical logical projection.
///
/// The projection remains the only source of parent/child relationships.
/// Taffy NodeIds are ephemeral indices for this pass; all persistent derived
/// state is keyed by ElementHandle.
pub struct NativeLayoutPass<'a, 'text> {
    projection: TaffyTreeProjection<'a>,
    text_system: &'text mut TextSystem,
    states: HashMap<ElementHandle, ElementLayoutState>,
}

impl<'a, 'text> NativeLayoutPass<'a, 'text> {
    pub fn new(
        projection: LogicalProjection<'a>,
        text_system: &'text mut TextSystem,
    ) -> Result<Self, LayoutComputeError> {
        let taffy_projection = TaffyTreeProjection::new(projection);
        let mut states = HashMap::new();

        for handle in taffy_projection.projection().handles() {
            let element = taffy_projection
                .projection()
                .element(handle)
                .ok_or(LayoutComputeError::UnknownElement(handle))?;

            let inherited_text_styles = if element.kind == UiElementKind::Text {
                taffy_projection
                    .projection()
                    .parent_of(handle)
                    .and_then(|parent| taffy_projection.projection().element(parent))
                    .map(|parent| &parent.computed_styles)
            } else {
                None
            };
            let computed_styles = inherited_text_styles.unwrap_or(&element.computed_styles);
            let style = if element.kind == UiElementKind::Text {
                taffy::Style {
                    display: taffy::Display::Block,
                    ..taffy::Style::default()
                }
            } else {
                taffy_style_from_computed(computed_styles)
                    .map_err(|source| LayoutComputeError::InvalidStyle { handle, source })?
            };

            let children = taffy_projection.projection().children_of(handle);
            let mut hidden_by_ancestor = false;
            let mut ancestor = taffy_projection.projection().parent_of(handle);
            while let Some(parent) = ancestor {
                if states
                    .get(&parent)
                    .is_some_and(|state: &ElementLayoutState| {
                        state.style.display == taffy::Display::None
                    })
                {
                    hidden_by_ancestor = true;
                    break;
                }
                ancestor = taffy_projection.projection().parent_of(parent);
            }

            if !children.is_empty()
                && style.display != taffy::Display::Flex
                && style.display != taffy::Display::None
                && !hidden_by_ancestor
            {
                return Err(LayoutComputeError::UnsupportedNode {
                    handle,
                    reason: "only flex containers may have children in this layout milestone",
                });
            }

            let mut state = ElementLayoutState::new(style);
            if element.kind == UiElementKind::Text {
                state.text_content = Some(element.text_content().unwrap_or_default().to_owned());
                state.font_family = computed_styles
                    .get("font-family")
                    .cloned()
                    .unwrap_or_else(|| "sans-serif".into());
                state.font_weight = computed_styles
                    .get("font-weight")
                    .and_then(|value| value.parse::<u16>().ok())
                    .unwrap_or(400)
                    .clamp(1, 1000);
                state.font_style = computed_styles
                    .get("font-style")
                    .cloned()
                    .unwrap_or_else(|| "normal".into());
                state.font_stretch = computed_styles
                    .get("font-stretch")
                    .cloned()
                    .unwrap_or_else(|| "100%".into());
                state.font_size = computed_styles
                    .get("font-size")
                    .and_then(|value| value.strip_suffix("px"))
                    .and_then(|value| value.trim().parse::<f32>().ok())
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .unwrap_or(16.0);
                state.line_height = computed_styles
                    .get("line-height")
                    .map(String::as_str)
                    .and_then(|value| {
                        if value == "normal" {
                            Some(state.font_size * 1.2)
                        } else if let Some(px) = value.strip_suffix("px") {
                            px.trim().parse::<f32>().ok()
                        } else {
                            value
                                .parse::<f32>()
                                .ok()
                                .map(|scale| state.font_size * scale)
                        }
                    })
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .unwrap_or(state.font_size * 1.2);
            }
            states.insert(handle, state);
        }

        Ok(Self {
            projection: taffy_projection,
            text_system,
            states,
        })
    }

    /// Compute a full layout against a definite viewport in CSS pixels.
    pub fn compute(
        &mut self,
        root: ElementHandle,
        viewport: Size<f32>,
    ) -> Result<(), LayoutComputeError> {
        let root_id = self
            .projection
            .node_id(root)
            .ok_or(LayoutComputeError::UnknownElement(root))?
            .0;

        compute_root_layout(
            self,
            root_id,
            Size {
                width: AvailableSpace::Definite(viewport.width),
                height: AvailableSpace::Definite(viewport.height),
            },
        );
        round_layout(self, root_id);
        Ok(())
    }

    /// Return the final pixel-rounded layout rectangle for an element.
    pub fn layout(&self, handle: ElementHandle) -> Option<Layout> {
        self.states.get(&handle).map(|state| state.final_layout)
    }

    fn handle(&self, node_id: taffy::NodeId) -> ElementHandle {
        self.projection.handle_for_taffy_id(node_id)
    }

    fn state(&self, node_id: taffy::NodeId) -> &ElementLayoutState {
        &self.states[&self.handle(node_id)]
    }

    fn state_mut(&mut self, node_id: taffy::NodeId) -> &mut ElementLayoutState {
        let handle = self.handle(node_id);
        self.states
            .get_mut(&handle)
            .expect("layout state exists for every projected element")
    }
}

impl taffy::TraversePartialTree for NativeLayoutPass<'_, '_> {
    type ChildIter<'b>
        = std::vec::IntoIter<taffy::NodeId>
    where
        Self: 'b;

    fn child_ids(&self, parent_node_id: taffy::NodeId) -> Self::ChildIter<'_> {
        self.projection
            .projection()
            .children_of(self.handle(parent_node_id))
            .into_iter()
            .filter_map(|handle| self.projection.node_ids.get(&handle).copied())
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
        self.projection.node_ids[&child]
    }
}

impl taffy::TraverseTree for NativeLayoutPass<'_, '_> {}

impl taffy::LayoutPartialTree for NativeLayoutPass<'_, '_> {
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

impl CacheTree for NativeLayoutPass<'_, '_> {
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

impl taffy::LayoutContainingBlock for NativeLayoutPass<'_, '_> {
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

impl taffy::LayoutFlexboxContainer for NativeLayoutPass<'_, '_> {
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

impl taffy::RoundTree for NativeLayoutPass<'_, '_> {
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
        let mut pass = NativeLayoutPass::new(ui.projection(), &mut text_system).unwrap();
        pass.compute(
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
        let mut pass = NativeLayoutPass::new(ui.projection(), &mut text_system).unwrap();
        pass.compute(
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
        let mut pass = NativeLayoutPass::new(ui.projection(), &mut text_system).unwrap();
        pass.compute(
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
}
