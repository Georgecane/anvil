use super::ui::{DequeHandle, ElementHandle, UiModel};
use crate::deque_tree::{ChildSide, End};
use boa_engine::{Context, JsNativeError, JsResult, JsValue, NativeFunction, Source};

#[derive(Debug)]
pub enum UiCommand {
    CreateRoot {
        handle: DequeHandle,
    },
    CreateChild {
        handle: DequeHandle,
        parent: DequeHandle,
        side: ChildSide,
    },
    CreateElement {
        handle: ElementHandle,
        deque: DequeHandle,
        tag: String,
        end: End,
    },
    CreateText {
        handle: ElementHandle,
        deque: DequeHandle,
        text: String,
        end: End,
    },
    SetText {
        element: ElementHandle,
        text: String,
    },
    SetProperty {
        element: ElementHandle,
        key: String,
        value: String,
    },
    SetStyle {
        element: ElementHandle,
        key: String,
        value: String,
    },
    RemoveSubtree {
        anchor: ElementHandle,
    },
    MoveSubtree {
        anchor: ElementHandle,
        target: DequeHandle,
        target_end: End,
    },
}
#[derive(Default)]
pub struct UiCommandBuffer {
    pub commands: Vec<UiCommand>,
    root_requested: bool,
}
impl UiCommandBuffer {
    pub fn install(context: &mut Context) -> JsResult<()> {
        context.insert_data(Self::default());
        let bindings = [
            (
                "anvilCreateRootDeque",
                0,
                NativeFunction::from_copy_closure(create_root),
            ),
            (
                "anvilCreateChildDeque",
                2,
                NativeFunction::from_copy_closure(create_child),
            ),
            (
                "anvilCreateElement",
                3,
                NativeFunction::from_copy_closure(create_element),
            ),
            (
                "anvilCreateText",
                3,
                NativeFunction::from_copy_closure(create_text),
            ),
            (
                "anvilSetText",
                2,
                NativeFunction::from_copy_closure(set_text),
            ),
            (
                "anvilSetProperty",
                3,
                NativeFunction::from_copy_closure(set_property),
            ),
            (
                "anvilSetStyle",
                3,
                NativeFunction::from_copy_closure(set_style),
            ),
            (
                "anvilRemoveSubtree",
                1,
                NativeFunction::from_copy_closure(remove_subtree),
            ),
            (
                "anvilMoveSubtree",
                3,
                NativeFunction::from_copy_closure(move_subtree),
            ),
        ];
        for (name, arity, function) in bindings {
            context.register_global_callable(name.into(), arity, function)?;
        }
        context.eval(Source::from_bytes("globalThis.Anvil = Object.freeze({createRootDeque:anvilCreateRootDeque,createChildDeque:anvilCreateChildDeque,createElement:anvilCreateElement,createText:anvilCreateText,setText:anvilSetText,setProperty:anvilSetProperty,setStyle:anvilSetStyle,removeSubtree:anvilRemoveSubtree,moveSubtree:anvilMoveSubtree});"))?;
        Ok(())
    }
}
fn buffer(context: &mut Context) -> JsResult<&mut UiCommandBuffer> {
    context
        .host_defined_mut()
        .get_mut::<UiCommandBuffer>()
        .ok_or_else(|| {
            JsNativeError::error()
                .with_message("Anvil bridge unavailable")
                .into()
        })
}
fn string_arg(args: &[JsValue], i: usize, context: &mut Context) -> JsResult<String> {
    args.get(i)
        .ok_or_else(|| JsNativeError::typ().with_message("Missing argument"))?
        .to_string(context)
        .map(|v| v.to_std_string_escaped())
}
fn u32_arg(args: &[JsValue], i: usize, context: &mut Context) -> JsResult<u32> {
    let number = args
        .get(i)
        .ok_or_else(|| JsNativeError::typ().with_message("Missing numeric argument"))?
        .to_number(context)?;
    if !number.is_finite() || number < 1.0 || number.fract() != 0.0 || number > u32::MAX as f64 {
        return Err(JsNativeError::typ()
            .with_message("Expected a positive 32-bit integer")
            .into());
    }
    Ok(number as u32)
}
fn u64_arg(args: &[JsValue], i: usize, context: &mut Context) -> JsResult<u64> {
    let n = args
        .get(i)
        .ok_or_else(|| JsNativeError::typ().with_message("Missing handle"))?
        .to_number(context)?;
    if !n.is_finite() || n < 1.0 || n.fract() != 0.0 || n > 9_007_199_254_740_991.0 {
        return Err(JsNativeError::typ()
            .with_message("Expected a positive safe integer")
            .into());
    }
    Ok(n as u64)
}
fn end_arg(args: &[JsValue], i: usize, context: &mut Context) -> JsResult<End> {
    match string_arg(args, i, context)?.as_str() {
        "front" => Ok(End::Front),
        "back" => Ok(End::Back),
        _ => Err(JsNativeError::typ()
            .with_message("End must be 'front' or 'back'")
            .into()),
    }
}
fn create_root(_: &JsValue, _: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let state = buffer(c)?;
    if state.root_requested {
        return Err(JsNativeError::typ()
            .with_message("Root already requested")
            .into());
    }
    let handle = UiModel::allocate_deque_handle();
    state.root_requested = true;
    state.commands.push(UiCommand::CreateRoot { handle });
    Ok(handle.0.into())
}
fn create_child(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let parent = DequeHandle(u32_arg(a, 0, c)?);
    let side = match string_arg(a, 1, c)?.as_str() {
        "left" => ChildSide::Left,
        "right" => ChildSide::Right,
        _ => {
            return Err(JsNativeError::typ()
                .with_message("Side must be 'left' or 'right'")
                .into());
        }
    };
    let handle = UiModel::allocate_deque_handle();
    buffer(c)?.commands.push(UiCommand::CreateChild {
        handle,
        parent,
        side,
    });
    Ok(handle.0.into())
}
fn create_element(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let deque = DequeHandle(u32_arg(a, 0, c)?);
    let tag = string_arg(a, 1, c)?;
    let end = end_arg(a, 2, c)?;
    let handle = UiModel::allocate_element_handle();
    buffer(c)?.commands.push(UiCommand::CreateElement {
        handle,
        deque,
        tag,
        end,
    });
    Ok(handle.0.into())
}
fn create_text(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let deque = DequeHandle(u32_arg(a, 0, c)?);
    let text = string_arg(a, 1, c)?;
    let end = end_arg(a, 2, c)?;
    let handle = UiModel::allocate_element_handle();
    buffer(c)?.commands.push(UiCommand::CreateText {
        handle,
        deque,
        text,
        end,
    });
    Ok(handle.0.into())
}
fn set_text(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let element = ElementHandle(u64_arg(a, 0, c)?);
    let text = string_arg(a, 1, c)?;
    buffer(c)?
        .commands
        .push(UiCommand::SetText { element, text });
    Ok(JsValue::undefined())
}
fn set_property(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let element = ElementHandle(u64_arg(a, 0, c)?);
    let key = string_arg(a, 1, c)?;
    let value = string_arg(a, 2, c)?;
    buffer(c)?.commands.push(UiCommand::SetProperty {
        element,
        key,
        value,
    });
    Ok(JsValue::undefined())
}
fn set_style(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let element = ElementHandle(u64_arg(a, 0, c)?);
    let key = string_arg(a, 1, c)?;
    let value = string_arg(a, 2, c)?;
    buffer(c)?.commands.push(UiCommand::SetStyle {
        element,
        key,
        value,
    });
    Ok(JsValue::undefined())
}
fn remove_subtree(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let anchor = ElementHandle(u64_arg(a, 0, c)?);
    buffer(c)?
        .commands
        .push(UiCommand::RemoveSubtree { anchor });
    Ok(JsValue::undefined())
}
fn move_subtree(_: &JsValue, a: &[JsValue], c: &mut Context) -> JsResult<JsValue> {
    let anchor = ElementHandle(u64_arg(a, 0, c)?);
    let target = DequeHandle(u32_arg(a, 1, c)?);
    let target_end = end_arg(a, 2, c)?;
    buffer(c)?.commands.push(UiCommand::MoveSubtree {
        anchor,
        target,
        target_end,
    });
    Ok(JsValue::undefined())
}
pub fn drain(context: &mut Context) -> Vec<UiCommand> {
    context
        .host_defined_mut()
        .get_mut::<UiCommandBuffer>()
        .map(|s| std::mem::take(&mut s.commands))
        .unwrap_or_default()
}
pub fn discard(context: &mut Context, root_exists: bool) {
    if let Some(state) = context.host_defined_mut().get_mut::<UiCommandBuffer>() {
        state.commands.clear();
        state.root_requested = root_exists;
    }
}
