use super::ui::{ElementHandle, UiElement, UiModel};
use crate::deque_tree::{NodeId, RenderFrame};

pub struct RenderElement<'a> {
    pub handle: ElementHandle,
    pub element: &'a UiElement,
}
pub struct RenderInput<'a> {
    pub node: NodeId,
    pub elements: Vec<RenderElement<'a>>,
}

/// Renderer boundary. Implementations consume the canonical DequeTree in its precomputed DFS order.
pub trait NativeRenderer {
    type Error;
    fn begin_frame(&mut self) -> Result<(), Self::Error>;
    fn render_deque(&mut self, input: RenderInput<'_>) -> Result<(), Self::Error>;
    fn end_frame(&mut self) -> Result<(), Self::Error>;
}

pub fn render_frame<R: NativeRenderer>(
    renderer: &mut R,
    ui: &UiModel,
    frame: &RenderFrame,
) -> Result<(), R::Error> {
    renderer.begin_frame()?;
    let render_result = (|| {
        for id in frame.stack().iter() {
            if let Some(node) = ui.tree().get(id) {
                if node.deque().is_ghost() {
                    continue;
                }
                let mut elements = Vec::with_capacity(node.deque().len());
                for (entry, element) in node.deque().ids().rev().zip(node.deque().iter().rev()) {
                    if let Some(handle) = ui.element_handle_for_entry(id, entry) {
                        elements.push(RenderElement { handle, element });
                    }
                }
                renderer.render_deque(RenderInput { node: id, elements })?;
            }
        }
        Ok(())
    })();
    let end_result = renderer.end_frame();
    render_result.and(end_result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deque_tree::{ChildSide, End};

    #[derive(Default)]
    struct RecordingRenderer {
        tags: Vec<String>,
    }
    impl NativeRenderer for RecordingRenderer {
        type Error = ();
        fn begin_frame(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn render_deque(&mut self, input: RenderInput<'_>) -> Result<(), Self::Error> {
            self.tags.extend(
                input
                    .elements
                    .into_iter()
                    .map(|render_element| render_element.element.tag.clone()),
            );
            Ok(())
        }
        fn end_frame(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn renderer_closes_frame_when_a_deque_render_fails() {
        struct FailingRenderer {
            ended: bool,
        }
        impl NativeRenderer for FailingRenderer {
            type Error = &'static str;
            fn begin_frame(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
            fn render_deque(&mut self, _input: RenderInput<'_>) -> Result<(), Self::Error> {
                Err("render failed")
            }
            fn end_frame(&mut self) -> Result<(), Self::Error> {
                self.ended = true;
                Ok(())
            }
        }

        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        ui.create_element(root, "main", End::Back).unwrap();
        let frame = ui.prepare_render();
        let mut renderer = FailingRenderer { ended: false };

        assert_eq!(
            render_frame(&mut renderer, &ui, &frame),
            Err("render failed")
        );
        assert!(renderer.ended);
    }

    #[test]
    fn renderer_observes_parent_first_preorder_across_the_deque_tree() {
        let mut ui = UiModel::new();
        let root = ui.create_root_deque().unwrap();
        let left = ui.create_child_deque(root, ChildSide::Left).unwrap();
        let right = ui.create_child_deque(root, ChildSide::Right).unwrap();
        let left_left = ui.create_child_deque(left, ChildSide::Left).unwrap();
        let left_right = ui.create_child_deque(left, ChildSide::Right).unwrap();
        let right_left = ui.create_child_deque(right, ChildSide::Left).unwrap();
        let right_right = ui.create_child_deque(right, ChildSide::Right).unwrap();
        for (deque, tag) in [
            (root, "A"),
            (left, "B"),
            (left_left, "D"),
            (left_right, "E"),
            (right, "C"),
            (right_left, "F"),
            (right_right, "G"),
        ] {
            ui.create_element(deque, tag, End::Back).unwrap();
        }
        let frame = ui.prepare_render();
        let mut renderer = RecordingRenderer::default();
        render_frame(&mut renderer, &ui, &frame).unwrap();
        assert_eq!(renderer.tags, ["a", "b", "d", "e", "c", "f", "g"]);
    }
}

#[derive(Debug)]
pub enum GpuInitError {
    Adapter(wgpu::RequestAdapterError),
    Device(wgpu::RequestDeviceError),
}

/// Shared native GPU context. Surface and swapchain ownership remain with the window backend.
pub struct GpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}
impl GpuContext {
    pub async fn request_default() -> Result<Self, GpuInitError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .map_err(GpuInitError::Adapter)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(GpuInitError::Device)?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }
    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }
    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }
}
