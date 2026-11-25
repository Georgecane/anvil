use super::ui::{ElementHandle, LogicalProjection};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutStyleError {
    UnsupportedValue {
        property: &'static str,
        value: String,
    },
    InvalidValue {
        property: &'static str,
        value: String,
    },
}
fn unsupported(property: &'static str, value: &str) -> LayoutStyleError {
    LayoutStyleError::UnsupportedValue {
        property,
        value: value.to_owned(),
    }
}
fn invalid(property: &'static str, value: &str) -> LayoutStyleError {
    LayoutStyleError::InvalidValue {
        property,
        value: value.to_owned(),
    }
}
fn parse_numeric(value: &str, property: &'static str) -> Result<f32, LayoutStyleError> {
    let parsed = value.parse::<f32>().map_err(|_| invalid(property, value))?;
    if parsed.is_finite() {
        Ok(parsed)
    } else {
        Err(invalid(property, value))
    }
}
fn parse_dimension(
    value: &str,
    property: &'static str,
) -> Result<taffy::Dimension, LayoutStyleError> {
    let value = value.trim();
    match value {
        "auto" => Ok(taffy::Dimension::auto()),
        "min-content" => Ok(taffy::Dimension::min_content()),
        "max-content" => Ok(taffy::Dimension::max_content()),
        _ if value.ends_with("px") => Ok(taffy::Dimension::length(parse_numeric(
            value.trim_end_matches("px").trim(),
            property,
        )?)),
        _ if value.ends_with('%') => Ok(taffy::Dimension::percent(
            parse_numeric(value.trim_end_matches('%').trim(), property)? / 100.0,
        )),
        _ if value == "0" => Ok(taffy::Dimension::length(0.0)),
        _ => Err(unsupported(property, value)),
    }
}
fn parse_length_percentage_auto(
    value: &str,
    property: &'static str,
) -> Result<taffy::LengthPercentageAuto, LayoutStyleError> {
    let value = value.trim();
    match value {
        "auto" => Ok(taffy::LengthPercentageAuto::auto()),
        _ if value.ends_with("px") => Ok(taffy::LengthPercentageAuto::length(parse_numeric(
            value.trim_end_matches("px").trim(),
            property,
        )?)),
        _ if value.ends_with('%') => Ok(taffy::LengthPercentageAuto::percent(
            parse_numeric(value.trim_end_matches('%').trim(), property)? / 100.0,
        )),
        _ if value == "0" => Ok(taffy::LengthPercentageAuto::length(0.0)),
        _ => Err(unsupported(property, value)),
    }
}
fn parse_length_percentage(
    value: &str,
    property: &'static str,
) -> Result<taffy::LengthPercentage, LayoutStyleError> {
    let value = value.trim();
    match value {
        _ if value.ends_with("px") => Ok(taffy::LengthPercentage::length(parse_numeric(
            value.trim_end_matches("px").trim(),
            property,
        )?)),
        _ if value.ends_with('%') => Ok(taffy::LengthPercentage::percent(
            parse_numeric(value.trim_end_matches('%').trim(), property)? / 100.0,
        )),
        _ if value == "0" => Ok(taffy::LengthPercentage::length(0.0)),
        _ => Err(unsupported(property, value)),
    }
}
fn computed_value<'a>(
    properties: &'a BTreeMap<String, String>,
    property: &'static str,
) -> Option<&'a str> {
    properties.get(property).map(String::as_str)
}

fn parse_nonnegative(value: &str, property: &'static str) -> Result<f32, LayoutStyleError> {
    let parsed = parse_numeric(value.trim(), property)?;
    if parsed >= 0.0 {
        Ok(parsed)
    } else {
        Err(invalid(property, value))
    }
}

/// Translate Stylo's computed snapshot into Taffy's input style. This does not
/// parse declarations, cascade rules, or resolve CSS. Values are expected to
/// be computed CSS px, percentages, or supported keywords. Calc expressions
/// are rejected until their opaque Stylo handles can be safely owned.
pub fn taffy_style_from_computed(
    properties: &BTreeMap<String, String>,
) -> Result<taffy::Style, LayoutStyleError> {
    let mut style = taffy::Style::default();
    if let Some(value) = computed_value(properties, "display") {
        style.display = match value {
            "none" => taffy::Display::None,
            "block" => taffy::Display::Block,
            "flow-root" => taffy::Display::FlowRoot,
            "flex" => taffy::Display::Flex,
            "grid" => taffy::Display::Grid,
            _ => return Err(unsupported("display", value)),
        };
    }
    if let Some(value) = computed_value(properties, "box-sizing") {
        style.box_sizing = match value {
            "border-box" => taffy::BoxSizing::BorderBox,
            "content-box" => taffy::BoxSizing::ContentBox,
            _ => return Err(unsupported("box-sizing", value)),
        };
    }
    if let Some(value) = computed_value(properties, "position") {
        style.position = match value {
            "static" => taffy::Position::Static,
            "relative" => taffy::Position::Relative,
            "absolute" => taffy::Position::Absolute,
            "fixed" => taffy::Position::Fixed,
            "sticky" => taffy::Position::Sticky,
            _ => return Err(unsupported("position", value)),
        };
    }
    if let Some(value) = computed_value(properties, "direction") {
        style.direction = match value {
            "ltr" => taffy::Direction::Ltr,
            "rtl" => taffy::Direction::Rtl,
            _ => return Err(unsupported("direction", value)),
        };
    }
    if let Some(value) = computed_value(properties, "flex-direction") {
        style.flex_direction = match value {
            "row" => taffy::FlexDirection::Row,
            "row-reverse" => taffy::FlexDirection::RowReverse,
            "column" => taffy::FlexDirection::Column,
            "column-reverse" => taffy::FlexDirection::ColumnReverse,
            _ => return Err(unsupported("flex-direction", value)),
        };
    }
    if let Some(value) = computed_value(properties, "flex-wrap") {
        style.flex_wrap = match value {
            "nowrap" => taffy::FlexWrap::NoWrap,
            "wrap" => taffy::FlexWrap::Wrap,
            "wrap-reverse" => taffy::FlexWrap::WrapReverse,
            _ => return Err(unsupported("flex-wrap", value)),
        };
    }
    if let Some(value) = computed_value(properties, "flex-grow") {
        style.flex_grow = parse_nonnegative(value, "flex-grow")?;
    }
    if let Some(value) = computed_value(properties, "flex-shrink") {
        style.flex_shrink = parse_nonnegative(value, "flex-shrink")?;
    }
    if let Some(value) = computed_value(properties, "flex-basis") {
        style.flex_basis = parse_dimension(value, "flex-basis")?;
    }
    if let Some(value) = computed_value(properties, "row-gap") {
        style.gap.height = parse_length_percentage(value, "row-gap")?;
    }
    if let Some(value) = computed_value(properties, "column-gap") {
        style.gap.width = parse_length_percentage(value, "column-gap")?;
    }
    for (property, target) in [
        ("width", &mut style.size.width),
        ("height", &mut style.size.height),
    ] {
        if let Some(value) = computed_value(properties, property) {
            *target = parse_dimension(value, property)?;
        }
    }
    for (property, target) in [
        ("min-width", &mut style.min_size.width),
        ("min-height", &mut style.min_size.height),
        ("max-width", &mut style.max_size.width),
        ("max-height", &mut style.max_size.height),
    ] {
        if let Some(value) = computed_value(properties, property) {
            *target = parse_length_percentage_auto(value, property)?;
        }
    }
    for (property, target) in [
        ("margin-top", &mut style.margin.top),
        ("margin-right", &mut style.margin.right),
        ("margin-bottom", &mut style.margin.bottom),
        ("margin-left", &mut style.margin.left),
    ] {
        if let Some(value) = computed_value(properties, property) {
            *target = parse_length_percentage_auto(value, property)?;
        }
    }
    for (property, target) in [
        ("padding-top", &mut style.padding.top),
        ("padding-right", &mut style.padding.right),
        ("padding-bottom", &mut style.padding.bottom),
        ("padding-left", &mut style.padding.left),
    ] {
        if let Some(value) = computed_value(properties, property) {
            *target = parse_length_percentage(value, property)?;
        }
    }
    for (property, target) in [
        ("border-top-width", &mut style.border.top),
        ("border-right-width", &mut style.border.right),
        ("border-bottom-width", &mut style.border.bottom),
        ("border-left-width", &mut style.border.left),
    ] {
        if let Some(value) = computed_value(properties, property) {
            *target = parse_length_percentage(value, property)?;
        }
    }
    for (property, target) in [
        ("top", &mut style.inset.top),
        ("right", &mut style.inset.right),
        ("bottom", &mut style.inset.bottom),
        ("left", &mut style.inset.left),
    ] {
        if let Some(value) = computed_value(properties, property) {
            *target = parse_length_percentage_auto(value, property)?;
        }
    }
    Ok(style)
}

/// A temporary Taffy node index mapped to a stable canonical Anvil element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutNodeId(pub(crate) taffy::NodeId);

/// Read-only Taffy traversal over the canonical Deque Tree.
/// No TaffyTree or parallel parent/child hierarchy is constructed.
pub struct TaffyTreeProjection<'a> {
    projection: LogicalProjection<'a>,
    handles: Vec<ElementHandle>,
    pub(crate) node_ids: HashMap<ElementHandle, taffy::NodeId>,
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

    pub(crate) fn handle_for_taffy_id(&self, node_id: taffy::NodeId) -> ElementHandle {
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
    use taffy::TraversePartialTree;

    #[test]
    fn computed_css_geometry_maps_to_taffy_without_losing_percent_or_auto() {
        let properties = BTreeMap::from([
            ("display".into(), "flex".into()),
            ("box-sizing".into(), "content-box".into()),
            ("position".into(), "absolute".into()),
            ("width".into(), "75%".into()),
            ("height".into(), "120px".into()),
            ("min-width".into(), "auto".into()),
            ("margin-left".into(), "auto".into()),
            ("margin-top".into(), "12px".into()),
            ("padding-left".into(), "5%".into()),
            ("padding-top".into(), "8px".into()),
            ("border-left-width".into(), "2px".into()),
            ("left".into(), "10%".into()),
        ]);
        let style = taffy_style_from_computed(&properties).unwrap();
        assert_eq!(style.display, taffy::Display::Flex);
        assert_eq!(style.box_sizing, taffy::BoxSizing::ContentBox);
        assert_eq!(style.position, taffy::Position::Absolute);
        assert_eq!(style.size.width, taffy::Dimension::percent(0.75));
        assert_eq!(style.size.height, taffy::Dimension::length(120.0));
        assert_eq!(style.min_size.width, taffy::LengthPercentageAuto::auto());
        assert_eq!(style.margin.left, taffy::LengthPercentageAuto::auto());
        assert_eq!(style.margin.top, taffy::LengthPercentageAuto::length(12.0));
        assert_eq!(style.padding.left, taffy::LengthPercentage::percent(0.05));
        assert_eq!(style.padding.top, taffy::LengthPercentage::length(8.0));
        assert_eq!(style.border.left, taffy::LengthPercentage::length(2.0));
        assert_eq!(style.inset.left, taffy::LengthPercentageAuto::percent(0.1));
    }

    #[test]
    fn computed_flex_css_maps_to_taffy_without_losing_axes_or_gaps() {
        let properties = BTreeMap::from([
            ("display".into(), "flex".into()),
            ("direction".into(), "rtl".into()),
            ("flex-direction".into(), "row-reverse".into()),
            ("flex-wrap".into(), "wrap-reverse".into()),
            ("flex-grow".into(), "2.5".into()),
            ("flex-shrink".into(), "0.25".into()),
            ("flex-basis".into(), "35%".into()),
            ("row-gap".into(), "12px".into()),
            ("column-gap".into(), "4%".into()),
        ]);
        let style = taffy_style_from_computed(&properties).unwrap();
        assert_eq!(style.direction, taffy::Direction::Rtl);
        assert_eq!(style.flex_direction, taffy::FlexDirection::RowReverse);
        assert_eq!(style.flex_wrap, taffy::FlexWrap::WrapReverse);
        assert_eq!(style.flex_grow, 2.5);
        assert_eq!(style.flex_shrink, 0.25);
        assert_eq!(style.flex_basis, taffy::Dimension::percent(0.35));
        assert_eq!(style.gap.height, taffy::LengthPercentage::length(12.0));
        assert_eq!(style.gap.width, taffy::LengthPercentage::percent(0.04));
    }

    #[test]
    fn computed_flex_factors_reject_negative_and_non_finite_values() {
        for value in ["-1", "NaN", "inf"] {
            let properties = BTreeMap::from([("flex-grow".into(), value.into())]);
            assert!(matches!(
                taffy_style_from_computed(&properties),
                Err(LayoutStyleError::InvalidValue {
                    property: "flex-grow",
                    ..
                })
            ));
        }
    }

    #[test]
    fn computed_css_calc_is_rejected_until_its_handle_lifetime_is_owned() {
        let properties = BTreeMap::from([("width".into(), "calc(100% - 12px)".into())]);
        assert!(matches!(
            taffy_style_from_computed(&properties),
            Err(LayoutStyleError::UnsupportedValue {
                property: "width",
                ..
            })
        ));
    }

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
