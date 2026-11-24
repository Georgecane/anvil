use super::bridge::{self, UiCommand};
use super::ecmascript::EcmaScriptEngine;
use super::framework::{
    CommitAdapter, FrameworkAdapter, FrameworkCommit, FrameworkDequeId, FrameworkError,
};
use super::ui::{DequeHandle, UiModel};
use crate::deque_tree::End;
use boa_engine::{JsNativeError, JsResult, JsValue};
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};

pub struct UiModelAccess<'a> {
    ui: &'a mut UiModel,
    framework: &'a mut CommitAdapter,
    initial_revision: u64,
}
impl Deref for UiModelAccess<'_> {
    type Target = UiModel;
    fn deref(&self) -> &Self::Target {
        self.ui
    }
}
impl DerefMut for UiModelAccess<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ui
    }
}
impl Drop for UiModelAccess<'_> {
    fn drop(&mut self) {
        if self.ui.mutation_revision() != self.initial_revision {
            self.framework.synchronize_with_ui(self.ui);
        }
    }
}

pub struct HostRuntime {
    javascript: EcmaScriptEngine,
    ui: UiModel,
    framework: CommitAdapter,
}
impl HostRuntime {
    pub fn new() -> Self {
        let mut javascript = EcmaScriptEngine::new();
        bridge::UiCommandBuffer::install(javascript.context_mut())
            .expect("native bindings register");
        Self {
            javascript,
            ui: UiModel::new(),
            framework: CommitAdapter::new(),
        }
    }
    pub fn evaluate(&mut self, source: &str) -> JsResult<JsValue> {
        let result = self.javascript.evaluate(source);
        match result {
            Ok(value) => {
                self.apply_commands()?;
                Ok(value)
            }
            Err(error) => {
                bridge::discard(
                    self.javascript.context_mut(),
                    self.ui.root_handle().is_some(),
                );
                Err(error)
            }
        }
    }
    pub fn evaluate_to_string(&mut self, source: &str) -> JsResult<String> {
        let result = self.javascript.evaluate_to_string(source);
        match result {
            Ok(value) => {
                self.apply_commands()?;
                Ok(value)
            }
            Err(error) => {
                bridge::discard(
                    self.javascript.context_mut(),
                    self.ui.root_handle().is_some(),
                );
                Err(error)
            }
        }
    }
    fn validate_simple_commands(&self, commands: &[UiCommand]) -> JsResult<()> {
        let error = |message: String| JsNativeError::error().with_message(message).into();
        let mut root_exists = self.ui.root_handle().is_some();
        let mut new_deques = std::collections::HashMap::<DequeHandle, (bool, bool)>::new();
        let mut existing_sides = std::collections::HashMap::<DequeHandle, (bool, bool)>::new();
        let mut new_elements = std::collections::HashMap::<super::ui::ElementHandle, bool>::new();

        for command in commands {
            match command {
                UiCommand::CreateRoot { handle } => {
                    if root_exists || self.ui.topology_is_sealed() {
                        return Err(error(
                            "Root Deque already exists or topology is sealed".into(),
                        ));
                    }
                    if self.ui.deque_id(*handle).is_some() || new_deques.contains_key(handle) {
                        return Err(error("Duplicate Deque handle".into()));
                    }
                    root_exists = true;
                    new_deques.insert(*handle, (false, false));
                }
                UiCommand::CreateChild {
                    handle,
                    parent,
                    side,
                } => {
                    if self.ui.topology_is_sealed() {
                        return Err(error("Deque topology is sealed".into()));
                    }
                    if self.ui.deque_id(*handle).is_some() || new_deques.contains_key(handle) {
                        return Err(error("Duplicate Deque handle".into()));
                    }
                    let sides = if let Some(sides) = new_deques.get_mut(parent) {
                        sides
                    } else {
                        if !existing_sides.contains_key(parent) {
                            let parent_id = self
                                .ui
                                .deque_id(*parent)
                                .ok_or_else(|| error("Unknown parent Deque".into()))?;
                            let node = self
                                .ui
                                .tree()
                                .get(parent_id)
                                .ok_or_else(|| error("Unknown parent Deque".into()))?;
                            existing_sides
                                .insert(*parent, (node.left().is_some(), node.right().is_some()));
                        }
                        existing_sides.get_mut(parent).expect("inserted above")
                    };
                    let occupied = match side {
                        crate::deque_tree::ChildSide::Left => &mut sides.0,
                        crate::deque_tree::ChildSide::Right => &mut sides.1,
                    };
                    if *occupied {
                        return Err(error("Deque child side is already occupied".into()));
                    }
                    *occupied = true;
                    new_deques.insert(*handle, (false, false));
                }
                UiCommand::CreateElement { handle, deque, .. } => {
                    if self.ui.deque_id(*deque).is_none() && !new_deques.contains_key(deque) {
                        return Err(error("Unknown target Deque".into()));
                    }
                    if self.ui.element(*handle).is_some() || new_elements.contains_key(handle) {
                        return Err(error("Duplicate Element handle".into()));
                    }
                    new_elements.insert(*handle, false);
                }
                UiCommand::CreateText { handle, deque, .. } => {
                    if self.ui.deque_id(*deque).is_none() && !new_deques.contains_key(deque) {
                        return Err(error("Unknown target Deque".into()));
                    }
                    if self.ui.element(*handle).is_some() || new_elements.contains_key(handle) {
                        return Err(error("Duplicate Element handle".into()));
                    }
                    new_elements.insert(*handle, true);
                }
                UiCommand::SetText { element, .. } => {
                    let is_text = new_elements
                        .get(element)
                        .copied()
                        .or_else(|| self.ui.element(*element).map(|item| item.is_text()));
                    match is_text {
                        Some(true) => {}
                        Some(false) => {
                            return Err(error("SetText target is not a Text node".into()));
                        }
                        None => return Err(error("Unknown Element handle".into())),
                    }
                }
                UiCommand::SetProperty { element, .. } | UiCommand::SetStyle { element, .. } => {
                    if !new_elements.contains_key(element) && self.ui.element(*element).is_none() {
                        return Err(error("Unknown Element handle".into()));
                    }
                }
                UiCommand::RemoveSubtree { .. } | UiCommand::MoveSubtree { .. } => {
                    unreachable!("structural commands use the full batch validator")
                }
            }
        }
        Ok(())
    }

    fn validate_commands(&self, commands: &[UiCommand]) -> JsResult<()> {
        // Property/text updates and ordinary insertions need only indexed lookups.
        // Copying every Deque and Element for those common batches would turn an
        // incremental mutation into O(total UI size) work. Keep the full structural
        // simulator only for batches containing subtree removal or transfer.
        if !commands.iter().any(|command| {
            matches!(
                command,
                UiCommand::RemoveSubtree { .. } | UiCommand::MoveSubtree { .. }
            )
        }) {
            return self.validate_simple_commands(commands);
        }

        let mut state = self.ui.batch_state();
        for command in commands {
            let error = |message: String| JsNativeError::error().with_message(message).into();
            match command {
                UiCommand::CreateRoot { handle } => {
                    if state.root.is_some() || state.sealed {
                        return Err(error(
                            "Root Deque already exists or topology is sealed".into(),
                        ));
                    }
                    if state.deques.contains_key(handle) {
                        return Err(error("Duplicate Deque handle".into()));
                    }
                    state.root = Some(*handle);
                    state.deques.insert(*handle, VecDeque::new());
                    state.subtree_spans.insert(*handle, VecDeque::new());
                    state.sides.insert(*handle, (false, false));
                    state.children.insert(*handle, [None, None]);
                }
                UiCommand::CreateChild {
                    handle,
                    parent,
                    side,
                } => {
                    if state.sealed {
                        return Err(error("Deque topology is sealed".into()));
                    }
                    if state.deques.contains_key(handle) {
                        return Err(error("Duplicate Deque handle".into()));
                    }
                    let occupied = state
                        .sides
                        .get(parent)
                        .ok_or_else(|| error("Unknown parent Deque".into()))?;
                    let used = match side {
                        crate::deque_tree::ChildSide::Left => occupied.0,
                        crate::deque_tree::ChildSide::Right => occupied.1,
                    };
                    if used {
                        return Err(error("Deque child side is already occupied".into()));
                    }
                    let entry = state.sides.get_mut(parent).unwrap();
                    let child_index = match side {
                        crate::deque_tree::ChildSide::Left => {
                            entry.0 = true;
                            0
                        }
                        crate::deque_tree::ChildSide::Right => {
                            entry.1 = true;
                            1
                        }
                    };
                    state
                        .children
                        .get_mut(parent)
                        .ok_or_else(|| error("Missing parent Deque links".into()))?[child_index] =
                        Some(*handle);
                    state.deques.insert(*handle, VecDeque::new());
                    state.subtree_spans.insert(*handle, VecDeque::new());
                    state.sides.insert(*handle, (false, false));
                    state.children.insert(*handle, [None, None]);
                }
                UiCommand::CreateElement {
                    handle, deque, end, ..
                }
                | UiCommand::CreateText {
                    handle, deque, end, ..
                } => {
                    if state.elements.contains_key(handle) {
                        return Err(error("Duplicate Element handle".into()));
                    }
                    let contents = state
                        .deques
                        .get_mut(deque)
                        .ok_or_else(|| error("Unknown target Deque".into()))?;
                    let spans = state
                        .subtree_spans
                        .get_mut(deque)
                        .ok_or_else(|| error("Missing structural boundary array".into()))?;
                    let is_text = matches!(command, UiCommand::CreateText { .. });
                    state.elements.insert(*handle, (*deque, is_text));
                    match end {
                        End::Front => {
                            if let Some(root_span) = spans.back_mut() {
                                *root_span += 1;
                            }
                            contents.push_front(*handle);
                            spans.push_front(1);
                        }
                        End::Back => {
                            let span = contents.len() + 1;
                            contents.push_back(*handle);
                            spans.push_back(span);
                        }
                    }
                }
                UiCommand::SetText { element, .. } => match state.elements.get(element) {
                    Some((_, true)) => {}
                    Some(_) => return Err(error("SetText target is not a Text node".into())),
                    None => return Err(error("Unknown Element handle".into())),
                },
                UiCommand::SetProperty { element, .. } | UiCommand::SetStyle { element, .. } => {
                    if !state.elements.contains_key(element) {
                        return Err(error("Unknown Element handle".into()));
                    }
                }
                UiCommand::RemoveSubtree { anchor } => {
                    let (deque_id, _) = state
                        .elements
                        .get(anchor)
                        .copied()
                        .ok_or_else(|| error("Unknown subtree anchor".into()))?;
                    let (start, count, is_root) = {
                        let contents = state.deques.get(&deque_id).unwrap();
                        let spans = state.subtree_spans.get(&deque_id).unwrap();
                        let index = contents
                            .iter()
                            .position(|handle| handle == anchor)
                            .ok_or_else(|| {
                                error("Subtree anchor is absent from its Deque".into())
                            })?;
                        let count = spans[index];
                        if count == 0 || count > index + 1 {
                            return Err(error("Invalid subtree boundary".into()));
                        }
                        let start = index + 1 - count;
                        (
                            start,
                            count,
                            index + 1 == contents.len() && start == 0 && count == contents.len(),
                        )
                    };
                    if is_root {
                        let mut pending = vec![deque_id];
                        let mut descendants = Vec::new();
                        while let Some(current) = pending.pop() {
                            descendants.push(current);
                            if let Some(children) = state.children.get(&current) {
                                if let Some(right) = children[1] {
                                    pending.push(right);
                                }
                                if let Some(left) = children[0] {
                                    pending.push(left);
                                }
                            }
                        }
                        for current in descendants {
                            let removed: Vec<_> =
                                state.deques.get_mut(&current).unwrap().drain(..).collect();
                            state.subtree_spans.get_mut(&current).unwrap().clear();
                            for handle in removed {
                                state.elements.remove(&handle);
                            }
                        }
                    } else {
                        let contents = state.deques.get_mut(&deque_id).unwrap();
                        let spans = state.subtree_spans.get_mut(&deque_id).unwrap();
                        let end = start + count;
                        for (ancestor_index, span) in spans.iter_mut().enumerate().skip(end) {
                            let old_start = ancestor_index + 1 - *span;
                            if old_start <= start {
                                *span -= count;
                            }
                        }
                        let removed: Vec<_> = contents.drain(start..end).collect();
                        spans.drain(start..end);
                        for handle in removed {
                            state.elements.remove(&handle);
                        }
                    }
                }
                UiCommand::MoveSubtree {
                    anchor,
                    target,
                    target_end,
                } => {
                    let (source_id, _) = state
                        .elements
                        .get(anchor)
                        .copied()
                        .ok_or_else(|| error("Unknown subtree anchor".into()))?;
                    if !state.deques.contains_key(target) {
                        return Err(error("Unknown target Deque".into()));
                    }
                    if source_id == *target {
                        return Err(error("Source and target Deques must differ".into()));
                    }
                    let source_contents = state.deques.get(&source_id).unwrap();
                    let source_spans = state.subtree_spans.get(&source_id).unwrap();
                    let anchor_index = source_contents
                        .iter()
                        .position(|handle| handle == anchor)
                        .ok_or_else(|| {
                        error("Subtree anchor is absent from its Deque".into())
                    })?;
                    let is_root = anchor_index + 1 == source_contents.len()
                        && source_spans[anchor_index] == source_contents.len();
                    if is_root
                        && state
                            .children
                            .get(&source_id)
                            .is_some_and(|children| children.iter().any(Option::is_some))
                    {
                        return Err(error(
                            "Cannot move a Deque root while its descendant Deques remain attached"
                                .into(),
                        ));
                    }
                    let (moved, moved_spans) = {
                        let source = state.deques.get_mut(&source_id).unwrap();
                        let spans = state.subtree_spans.get_mut(&source_id).unwrap();
                        let index = anchor_index;
                        let count = spans[index];
                        if count == 0 || count > index + 1 {
                            return Err(error("Invalid subtree boundary".into()));
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
                    let target_contents = state.deques.get_mut(target).unwrap();
                    let target_spans = state.subtree_spans.get_mut(target).unwrap();
                    let target_old_len = target_contents.len();
                    match target_end {
                        End::Front => {
                            if let Some(root_span) = target_spans.back_mut() {
                                *root_span += moved.len();
                            }
                            for (handle, span) in
                                moved.iter().copied().zip(moved_spans.iter().copied()).rev()
                            {
                                target_contents.push_front(handle);
                                target_spans.push_front(span);
                            }
                        }
                        End::Back => {
                            for (handle, mut span) in
                                moved.iter().copied().zip(moved_spans.iter().copied())
                            {
                                if target_old_len > 0 && span == moved.len() {
                                    span += target_old_len;
                                }
                                target_contents.push_back(handle);
                                target_spans.push_back(span);
                            }
                        }
                    }
                    for handle in moved {
                        let is_text = state.elements[&handle].1;
                        state.elements.insert(handle, (*target, is_text));
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_commands(&mut self) -> JsResult<()> {
        let commands = bridge::drain(self.javascript.context_mut());
        if let Err(error) = self.validate_commands(&commands) {
            bridge::discard(
                self.javascript.context_mut(),
                self.ui.root_handle().is_some(),
            );
            return Err(error);
        }
        for command in commands {
            let result = match command {
                UiCommand::CreateRoot { handle } => self.ui.create_root_deque_with_handle(handle),
                UiCommand::CreateChild {
                    handle,
                    parent,
                    side,
                } => self.ui.create_child_deque_with_handle(parent, handle, side),
                UiCommand::CreateElement {
                    handle,
                    deque,
                    tag,
                    end,
                } => self.ui.create_element_with_handle(
                    deque,
                    handle,
                    super::ui::UiElement::new(tag),
                    end,
                ),
                UiCommand::CreateText {
                    handle,
                    deque,
                    text,
                    end,
                } => self.ui.create_element_with_handle(
                    deque,
                    handle,
                    super::ui::UiElement::text(text),
                    end,
                ),
                UiCommand::SetText { element, text } => self.ui.set_text(element, text),
                UiCommand::SetProperty {
                    element,
                    key,
                    value,
                } => self.ui.set_property(element, key, value),
                UiCommand::SetStyle {
                    element,
                    key,
                    value,
                } => self.ui.set_style(element, key, value),
                UiCommand::RemoveSubtree { anchor } => self
                    .ui
                    .remove_subtree(anchor)
                    .map(|removed| self.framework.remove_native_elements(&removed)),
                UiCommand::MoveSubtree {
                    anchor,
                    target,
                    target_end,
                } => self.ui.move_subtree(anchor, target, target_end),
            };
            result.map_err(|e| {
                JsNativeError::error().with_message(format!("Anvil UI mutation failed: {e:?}"))
            })?;
        }
        Ok(())
    }
    pub fn apply_framework_commit(
        &mut self,
        commit: FrameworkCommit,
    ) -> Result<(), FrameworkError> {
        self.framework.apply_commit(commit, &mut self.ui)?;
        Ok(())
    }
    pub fn bind_framework_root(&mut self, id: FrameworkDequeId) -> Result<(), FrameworkError> {
        self.framework.bind_existing_root(id, &self.ui)
    }
    pub fn framework_adapter(&self) -> &CommitAdapter {
        &self.framework
    }
    pub fn ui(&self) -> &UiModel {
        &self.ui
    }
    pub fn ui_mut(&mut self) -> UiModelAccess<'_> {
        let initial_revision = self.ui.mutation_revision();
        UiModelAccess {
            ui: &mut self.ui,
            framework: &mut self.framework,
            initial_revision,
        }
    }
}
impl Default for HostRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deque_tree::End;

    #[test]
    fn javascript_uses_explicit_deque_ends_and_structural_removal() {
        let mut host = HostRuntime::new();
        host.evaluate(
            r#"
            globalThis.root = Anvil.createRootDeque();
            globalThis.child = Anvil.createElement(root, "child", "front");
            globalThis.parent = Anvil.createElement(root, "parent", "back");
        "#,
        )
        .unwrap();
        assert_eq!(
            host.ui()
                .tree()
                .get(host.ui().tree().root().unwrap())
                .unwrap()
                .deque()
                .len(),
            2
        );
        host.evaluate("Anvil.removeSubtree(parent)").unwrap();
        assert_eq!(
            host.ui()
                .tree()
                .get(host.ui().tree().root().unwrap())
                .unwrap()
                .deque()
                .len(),
            0
        );
    }

    #[test]
    fn direct_ui_mutation_synchronizes_framework_handles_when_access_ends() {
        let mut host = HostRuntime::new();
        let mut commit = FrameworkCommit::new();
        commit.push(
            super::super::framework::FrameworkOperation::CreateRootDeque {
                id: FrameworkDequeId(710),
            },
        );
        commit.push(super::super::framework::FrameworkOperation::CreateElement {
            id: super::super::framework::FrameworkElementId(711),
            deque: FrameworkDequeId(710),
            tag: "main".into(),
            end: End::Back,
        });
        host.apply_framework_commit(commit).unwrap();
        let native = host
            .framework_adapter()
            .native_element(super::super::framework::FrameworkElementId(711))
            .unwrap();

        host.ui_mut().remove_subtree(native).unwrap();

        assert!(
            host.framework_adapter()
                .native_element(super::super::framework::FrameworkElementId(711))
                .is_none()
        );
    }

    #[test]
    fn javascript_subtree_removal_prunes_framework_element_handles() {
        let mut host = HostRuntime::new();
        let mut commit = FrameworkCommit::new();
        commit.push(
            super::super::framework::FrameworkOperation::CreateRootDeque {
                id: FrameworkDequeId(700),
            },
        );
        commit.push(super::super::framework::FrameworkOperation::CreateElement {
            id: super::super::framework::FrameworkElementId(701),
            deque: FrameworkDequeId(700),
            tag: "main".into(),
            end: End::Back,
        });
        host.apply_framework_commit(commit).unwrap();

        let native = host
            .framework_adapter()
            .native_element(super::super::framework::FrameworkElementId(701))
            .unwrap();
        host.evaluate(&format!("Anvil.removeSubtree({})", native.0))
            .unwrap();

        assert!(
            host.framework_adapter()
                .native_element(super::super::framework::FrameworkElementId(701))
                .is_none()
        );
    }

    #[test]
    fn javascript_deque_handles_reject_fractional_zero_negative_and_overflow_values() {
        let mut host = HostRuntime::new();
        host.evaluate("globalThis.root = Anvil.createRootDeque()")
            .unwrap();

        for expression in ["root + 0.5", "0", "-1", "4294967296", "NaN", "Infinity"] {
            assert!(
                host.evaluate(&format!("Anvil.createChildDeque({expression}, 'left')"))
                    .is_err(),
                "invalid Deque handle expression was accepted: {expression}"
            );
        }

        host.evaluate("globalThis.child = Anvil.createChildDeque(root, 'left')")
            .unwrap();
        assert_eq!(host.ui().tree().len(), 2);
    }

    #[test]
    fn javascript_parent_removal_clears_descendant_deque_branches() {
        let mut host = HostRuntime::new();
        host.evaluate(
            r#"
            globalThis.root = Anvil.createRootDeque();
            globalThis.left = Anvil.createChildDeque(root, "left");
            globalThis.right = Anvil.createChildDeque(root, "right");
            globalThis.parent = Anvil.createElement(root, "parent", "back");
            globalThis.leftChild = Anvil.createElement(left, "left-child", "back");
            globalThis.rightChild = Anvil.createElement(right, "right-child", "back");
            Anvil.removeSubtree(parent);
        "#,
        )
        .unwrap();
        for name in ["parent", "leftChild", "rightChild"] {
            let handle = crate::runtime::ElementHandle(
                host.evaluate_to_string(name).unwrap().parse().unwrap(),
            );
            assert!(host.ui().element(handle).is_none());
        }
        for name in ["root", "left", "right"] {
            let handle = crate::runtime::DequeHandle(
                host.evaluate_to_string(name).unwrap().parse().unwrap(),
            );
            assert!(
                host.ui()
                    .tree()
                    .get(host.ui().deque_id(handle).unwrap())
                    .unwrap()
                    .deque()
                    .is_ghost()
            );
        }
    }

    #[test]
    fn javascript_move_subtree_infers_the_range_and_keeps_logical_siblings() {
        let mut host = HostRuntime::new();
        host.evaluate(
            r#"
            globalThis.root = Anvil.createRootDeque();
            globalThis.left = Anvil.createChildDeque(root, "left");
            globalThis.parent = Anvil.createElement(root, "parent", "back");
            globalThis.first = Anvil.createElement(root, "first", "front");
            globalThis.second = Anvil.createElement(root, "second", "front");
            Anvil.moveSubtree(first, left, "back");
        "#,
        )
        .unwrap();
        let parent = crate::runtime::ElementHandle(
            host.evaluate_to_string("parent").unwrap().parse().unwrap(),
        );
        let first = crate::runtime::ElementHandle(
            host.evaluate_to_string("first").unwrap().parse().unwrap(),
        );
        let second = crate::runtime::ElementHandle(
            host.evaluate_to_string("second").unwrap().parse().unwrap(),
        );
        assert!(host.ui().element(parent).is_some());
        assert!(host.ui().element(first).is_some());
        assert!(host.ui().element(second).is_some());
    }

    #[test]
    fn javascript_rejects_moving_a_root_while_child_deques_are_attached() {
        let mut host = HostRuntime::new();
        host.evaluate(
            r#"
            globalThis.root = Anvil.createRootDeque();
            globalThis.left = Anvil.createChildDeque(root, "left");
            globalThis.parent = Anvil.createElement(root, "parent", "back");
            globalThis.child = Anvil.createElement(left, "child", "back");
        "#,
        )
        .unwrap();
        assert!(
            host.evaluate("Anvil.moveSubtree(parent, left, 'back')")
                .is_err()
        );
        let parent = crate::runtime::ElementHandle(
            host.evaluate_to_string("parent").unwrap().parse().unwrap(),
        );
        let child = crate::runtime::ElementHandle(
            host.evaluate_to_string("child").unwrap().parse().unwrap(),
        );
        assert!(host.ui().element(parent).is_some());
        assert!(host.ui().element(child).is_some());
    }

    #[test]
    fn javascript_removes_a_single_element_subtree_without_a_manual_count() {
        let mut host = HostRuntime::new();
        host.evaluate(r#"globalThis.root = Anvil.createRootDeque(); globalThis.item = Anvil.createElement(root, "x", "back");"#).unwrap();
        host.evaluate("Anvil.removeSubtree(item)").unwrap();
        assert!(
            host.ui()
                .tree()
                .get(host.ui().tree().root().unwrap())
                .unwrap()
                .deque()
                .is_empty()
        );
        let _ = End::Back;
    }
}

#[cfg(test)]
mod bridge_transaction_tests {
    use super::*;

    #[test]
    fn failed_javascript_evaluation_discards_queued_native_mutations() {
        let mut host = HostRuntime::new();
        assert!(
            host.evaluate("Anvil.createRootDeque(); throw new Error('abort')")
                .is_err()
        );
        assert!(host.ui().root_handle().is_none());
        host.evaluate("globalThis.root = Anvil.createRootDeque()")
            .unwrap();
        assert!(host.ui().root_handle().is_some());
    }

    #[test]
    fn initial_topology_can_be_built_across_multiple_evaluations_until_first_render() {
        let mut host = HostRuntime::new();
        host.evaluate("globalThis.root = Anvil.createRootDeque()")
            .unwrap();
        host.evaluate("globalThis.left = Anvil.createChildDeque(root, 'left')")
            .unwrap();
        host.ui_mut().prepare_render();
        assert_eq!(host.ui().tree().len(), 2);
        assert!(host.ui().topology_is_sealed());
    }
}

#[cfg(test)]
mod native_batch_validation_tests {
    use super::*;

    #[test]
    fn duplicate_child_side_in_simple_batch_is_rejected_atomically() {
        let mut host = HostRuntime::new();
        let result = host.evaluate(
            "globalThis.root = Anvil.createRootDeque(); Anvil.createChildDeque(root, 'left'); Anvil.createChildDeque(root, 'left')",
        );
        assert!(result.is_err());
        assert!(host.ui().root_handle().is_none());
        assert_eq!(host.ui().tree().len(), 0);
    }

    #[test]
    fn invalid_native_command_batch_does_not_partially_create_the_root() {
        let mut host = HostRuntime::new();
        let result =
            host.evaluate("Anvil.createRootDeque(); Anvil.createElement(999, 'bad', 'back')");
        assert!(result.is_err());
        assert!(host.ui().root_handle().is_none());
        host.evaluate("globalThis.root = Anvil.createRootDeque()")
            .unwrap();
        assert!(host.ui().root_handle().is_some());
    }

    #[test]
    fn invalid_text_mutation_is_rejected_before_any_batch_mutation() {
        let mut host = HostRuntime::new();
        host.evaluate("globalThis.root = Anvil.createRootDeque(); globalThis.item = Anvil.createElement(root, 'div', 'back')").unwrap();
        let result =
            host.evaluate("Anvil.setProperty(item, 'before', 'x'); Anvil.setText(item, 'invalid')");
        assert!(result.is_err());
        let handle = crate::runtime::ElementHandle(
            host.evaluate_to_string("item").unwrap().parse().unwrap(),
        );
        assert!(
            !host
                .ui()
                .element(handle)
                .unwrap()
                .properties
                .contains_key("before")
        );
    }
}
