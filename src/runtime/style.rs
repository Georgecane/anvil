use super::ui::{ElementHandle, UiError, UiModel};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedStyle {
    pub element: ElementHandle,
    pub properties: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Default)]
pub struct StyleCommit {
    pub computed: Vec<ComputedStyle>,
}
impl StyleCommit {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, style: ComputedStyle) {
        self.computed.push(style);
    }
}
#[derive(Debug)]
pub enum StyleError {
    UnknownElement(ElementHandle),
    DuplicateElement(ElementHandle),
    Ui(UiError),
}
impl From<UiError> for StyleError {
    fn from(value: UiError) -> Self {
        Self::Ui(value)
    }
}
pub trait StyleAdapter {
    fn apply_style_commit(
        &mut self,
        commit: StyleCommit,
        ui: &mut UiModel,
    ) -> Result<(), StyleError>;
}
#[derive(Default)]
pub struct StyleCommitAdapter;
impl StyleAdapter for StyleCommitAdapter {
    fn apply_style_commit(
        &mut self,
        commit: StyleCommit,
        ui: &mut UiModel,
    ) -> Result<(), StyleError> {
        let mut seen = std::collections::HashSet::new();
        for style in &commit.computed {
            if !seen.insert(style.element) {
                return Err(StyleError::DuplicateElement(style.element));
            }
            if ui.element(style.element).is_none() {
                return Err(StyleError::UnknownElement(style.element));
            }
        }
        for style in commit.computed {
            ui.replace_computed_styles(style.element, style.properties)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deque_tree::End;
    use crate::runtime::ui::UiModel;

    #[test]
    fn computed_styles_are_separate_from_inline_styles() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let element = ui.create_element(root, "div", End::Back).unwrap();
        ui.set_style(element, "color", "red").unwrap();
        let mut commit = StyleCommit::new();
        commit.push(ComputedStyle {
            element,
            properties: BTreeMap::from([
                ("color".into(), "rgb(255, 0, 0)".into()),
                ("display".into(), "block".into()),
            ]),
        });
        StyleCommitAdapter
            .apply_style_commit(commit, &mut ui)
            .unwrap();
        assert_eq!(ui.element(element).unwrap().styles["color"], "red");
        assert_eq!(
            ui.computed_styles(element).unwrap()["color"],
            "rgb(255, 0, 0)"
        );
    }

    #[test]
    fn applying_computed_styles_publishes_consumer_deltas_without_advancing_ui_input_revision() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let element = ui.create_element(root, "section", End::Back).unwrap();
        let input_revision = ui.mutation_revision();

        let mut first = StyleCommit::new();
        first.push(ComputedStyle {
            element,
            properties: BTreeMap::from([
                ("display".into(), "block".into()),
                ("color".into(), "rgb(255, 0, 0)".into()),
            ]),
        });
        StyleCommitAdapter
            .apply_style_commit(first, &mut ui)
            .unwrap();

        assert_eq!(ui.mutation_revision(), input_revision);
        assert_eq!(ui.computed_style_revision(), 1);
        assert_eq!(
            ui.computed_style_changes_for(super::super::ui::ComputedStyleConsumer::Layout)
                .next()
                .unwrap()
                .changed_properties,
            ["color", "display"]
        );

        let mut unchanged = StyleCommit::new();
        unchanged.push(ComputedStyle {
            element,
            properties: BTreeMap::from([
                ("display".into(), "block".into()),
                ("color".into(), "rgb(255, 0, 0)".into()),
            ]),
        });
        StyleCommitAdapter
            .apply_style_commit(unchanged, &mut ui)
            .unwrap();
        assert_eq!(ui.computed_style_revision(), 1);
    }

    #[test]
    fn invalid_style_batch_is_rejected_before_any_style_is_replaced() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let element = ui.create_element(root, "div", End::Back).unwrap();
        let mut commit = StyleCommit::new();
        commit.push(ComputedStyle {
            element,
            properties: BTreeMap::from([("display".into(), "block".into())]),
        });
        commit.push(ComputedStyle {
            element: ElementHandle(999_999),
            properties: BTreeMap::new(),
        });
        assert!(
            StyleCommitAdapter
                .apply_style_commit(commit, &mut ui)
                .is_err()
        );
        assert!(ui.computed_styles(element).unwrap().is_empty());
    }
}

/// CSS computation boundary implemented by Stylo or another standards-compatible engine.
pub trait CssStyleEngine {
    type Error;
    fn compute(
        &mut self,
        projection: &super::ui::LogicalProjection<'_>,
    ) -> Result<StyleCommit, Self::Error>;

    /// Called only after the computed-style commit was successfully applied.
    /// Engines use this boundary to acknowledge the corresponding UI mutation revision.
    fn acknowledge_mutations(&mut self, _revision: u64) {}
}

#[derive(Debug)]
pub enum StylePassError<E> {
    Engine(E),
    Commit(StyleError),
}

pub fn run_style_pass<E: CssStyleEngine>(
    engine: &mut E,
    adapter: &mut StyleCommitAdapter,
    ui: &mut UiModel,
) -> Result<(), StylePassError<E::Error>> {
    let (commit, revision) = {
        let projection = ui.projection();
        let revision = projection.mutation_revision();
        let commit = engine
            .compute(&projection)
            .map_err(StylePassError::Engine)?;
        (commit, revision)
    };
    adapter
        .apply_style_commit(commit, ui)
        .map_err(StylePassError::Commit)?;
    engine.acknowledge_mutations(revision);
    ui.acknowledge_mutations(revision);
    Ok(())
}

#[cfg(test)]
mod css_engine_tests {
    use super::*;
    use crate::deque_tree::End;
    use crate::runtime::ui::UiModel;

    struct TestCss;
    impl CssStyleEngine for TestCss {
        type Error = ();
        fn compute(
            &mut self,
            projection: &super::super::ui::LogicalProjection<'_>,
        ) -> Result<StyleCommit, Self::Error> {
            let mut commit = StyleCommit::new();
            projection.visit(|_, handle, element| {
                if element.tag == "button" {
                    commit.push(ComputedStyle {
                        element: handle,
                        properties: BTreeMap::from([("display".into(), "inline-block".into())]),
                    });
                }
            });
            Ok(commit)
        }
    }

    #[test]
    fn css_engine_consumes_projection_and_commits_styles_to_the_same_model() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let button = ui.create_element(root, "button", End::Back).unwrap();
        run_style_pass(&mut TestCss, &mut StyleCommitAdapter, &mut ui).unwrap();
        assert_eq!(
            ui.computed_styles(button).unwrap()["display"],
            "inline-block"
        );
    }
}
