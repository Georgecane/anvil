use super::host::HostRuntime;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::error::EventLoopError;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Debug)]
pub enum WindowHostError {
    EventLoop(EventLoopError),
    WindowCreation(String),
}
impl std::fmt::Display for WindowHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoop(error) => write!(f, "native event loop failed: {error}"),
            Self::WindowCreation(error) => write!(f, "native window creation failed: {error}"),
        }
    }
}
impl std::error::Error for WindowHostError {}

/// Owns the native event loop and the application state; no browser window is involved.
pub struct WindowHost {
    event_loop: EventLoop<()>,
    application: WindowApplication,
}
struct WindowApplication {
    runtime: HostRuntime,
    window: Option<Arc<Window>>,
    window_error: Option<String>,
}
impl WindowHost {
    pub fn new(runtime: HostRuntime) -> Result<Self, EventLoopError> {
        Ok(Self {
            event_loop: EventLoop::new()?,
            application: WindowApplication {
                runtime,
                window: None,
                window_error: None,
            },
        })
    }
    pub fn run(mut self) -> Result<(), WindowHostError> {
        let result = self.event_loop.run_app(&mut self.application);
        if let Some(error) = self.application.window_error.take() {
            return Err(WindowHostError::WindowCreation(error));
        }
        result.map_err(WindowHostError::EventLoop)
    }
    pub fn runtime(&self) -> &HostRuntime {
        &self.application.runtime
    }
    pub fn runtime_mut(&mut self) -> &mut HostRuntime {
        &mut self.application.runtime
    }
    pub fn window(&self) -> Option<&Arc<Window>> {
        self.application.window.as_ref()
    }
}
impl ApplicationHandler for WindowApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let attributes = Window::default_attributes().with_title("Anvil");
            match event_loop.create_window(attributes) {
                Ok(window) => self.window = Some(Arc::new(window)),
                Err(error) => {
                    self.window_error = Some(error.to_string());
                    event_loop.exit();
                }
            }
        }
    }
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self
            .window
            .as_ref()
            .is_some_and(|window| window.id() != window_id)
        {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {}
            _ => {}
        }
    }
}
