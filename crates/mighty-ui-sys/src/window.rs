//! winit window ownership + `pump_events` driving + event translation.
//!
//! The shim owns the winit `EventLoop` and `Window`. The IDE drives the loop
//! by repeatedly polling: each `mui_poll_event` first pumps winit (non-blocking)
//! to drain OS events into an internal FIFO, then returns one queued
//! [`MuiEvent`]. The shim NEVER calls back into Mighty.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::platform::pump_events::EventLoopExtPumpEvents;
use winit::window::{Window, WindowId};

use crate::ffi::*;

/// A FIFO of translated input events plus resize bookkeeping.
#[derive(Default)]
pub struct EventQueue {
    queue: VecDeque<MuiEvent>,
    /// Current keyboard modifier state, folded into emitted events.
    mods: u32,
    /// Last known cursor position (mouse button events carry no coords).
    cursor: (f32, f32),
    /// Set when a resize occurred; the main loop reads & reconfigures.
    pub pending_resize: Option<(u32, u32)>,
}

impl EventQueue {
    pub fn push(&mut self, ev: MuiEvent) {
        self.queue.push_back(ev);
    }

    /// Pop the oldest event, or `None` when empty.
    pub fn pop(&mut self) -> Option<MuiEvent> {
        self.queue.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

fn modifiers_bits(state: &winit::keyboard::ModifiersState) -> u32 {
    let mut m = 0;
    if state.shift_key() {
        m |= MUI_MOD_SHIFT;
    }
    if state.control_key() {
        m |= MUI_MOD_CTRL;
    }
    if state.alt_key() {
        m |= MUI_MOD_ALT;
    }
    if state.super_key() {
        m |= MUI_MOD_SUPER;
    }
    m
}

fn named_key_code(k: NamedKey) -> Option<u32> {
    Some(match k {
        NamedKey::ArrowLeft => MUI_KEY_LEFT,
        NamedKey::ArrowRight => MUI_KEY_RIGHT,
        NamedKey::ArrowUp => MUI_KEY_UP,
        NamedKey::ArrowDown => MUI_KEY_DOWN,
        NamedKey::Backspace => MUI_KEY_BACKSPACE,
        NamedKey::Enter => MUI_KEY_ENTER,
        NamedKey::Tab => MUI_KEY_TAB,
        NamedKey::Escape => MUI_KEY_ESCAPE,
        NamedKey::Delete => MUI_KEY_DELETE,
        NamedKey::Home => MUI_KEY_HOME,
        NamedKey::End => MUI_KEY_END,
        NamedKey::PageUp => MUI_KEY_PAGE_UP,
        NamedKey::PageDown => MUI_KEY_PAGE_DOWN,
        NamedKey::F12 => MUI_KEY_F12,
        NamedKey::F2 => MUI_KEY_F2,
        NamedKey::F5 => MUI_KEY_F5,
        NamedKey::F10 => MUI_KEY_F10,
        NamedKey::F11 => MUI_KEY_F11,
        _ => return None,
    })
}

fn mouse_button_code(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => MUI_MOUSE_LEFT,
        MouseButton::Right => MUI_MOUSE_RIGHT,
        MouseButton::Middle => MUI_MOUSE_MIDDLE,
        _ => MUI_MOUSE_OTHER,
    }
}

/// Translate a single winit `WindowEvent` into zero or more `MuiEvent`s,
/// pushing them onto `q`. Shared by the live pump and tests.
pub fn translate_window_event(q: &mut EventQueue, event: &WindowEvent) {
    match event {
        WindowEvent::CloseRequested => q.push(MuiEvent::close()),
        WindowEvent::Resized(size) => {
            let w = size.width.max(1);
            let h = size.height.max(1);
            q.pending_resize = Some((w, h));
            q.push(MuiEvent::resize(w, h));
        }
        WindowEvent::ModifiersChanged(mods) => {
            q.mods = modifiers_bits(&mods.state());
        }
        WindowEvent::CursorMoved { position, .. } => {
            q.cursor = (position.x as f32, position.y as f32);
        }
        WindowEvent::MouseInput { state, button, .. } => {
            let tag = if *state == ElementState::Pressed {
                MUI_EVENT_MOUSE_DOWN
            } else {
                MUI_EVENT_MOUSE_UP
            };
            q.push(MuiEvent::mouse(
                tag,
                mouse_button_code(*button),
                q.cursor.0,
                q.cursor.1,
                q.mods,
            ));
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (*x, *y),
                MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
            };
            q.push(MuiEvent::scroll(dx, dy, q.mods));
        }
        WindowEvent::KeyboardInput { event, .. } => {
            if event.state != ElementState::Pressed {
                return;
            }
            match &event.logical_key {
                Key::Named(named) => {
                    if let Some(code) = named_key_code(*named) {
                        q.push(MuiEvent::key(code, q.mods));
                    } else if let Some(text) = &event.text {
                        // Named key that still produces text (e.g. Space).
                        for ch in text.chars() {
                            q.push(MuiEvent::char(ch as u32, q.mods));
                        }
                    }
                }
                Key::Character(s) => {
                    // When Ctrl/Alt/Super are held, surface as a Char event too
                    // (the IDE inspects mods, e.g. Ctrl+S). cosmic text input
                    // suppression is the IDE's job.
                    for ch in s.chars() {
                        q.push(MuiEvent::char(ch as u32, q.mods));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// The winit application handler. Holds the window and forwards events into a
/// queue. GPU init is deferred to `resumed` (winit 0.30 requirement on some
/// platforms), but the window is created eagerly so the caller has a handle.
pub struct App {
    pub window: Option<Arc<Window>>,
    width: u32,
    height: u32,
    title: String,
    /// Raw pointer back to the owning context's queue. The context outlives the
    /// pump call, so this is valid for the duration of `pump_events`.
    queue: *mut EventQueue,
    /// Set true once a window has been created.
    pub created: bool,
}

impl App {
    fn new(width: u32, height: u32, title: String, queue: *mut EventQueue) -> Self {
        Self {
            window: None,
            width,
            height,
            title,
            queue,
            created: false,
        }
    }

    fn queue_mut(&mut self) -> &mut EventQueue {
        // Safety: the context that owns the queue outlives every pump call.
        unsafe { &mut *self.queue }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(winit::dpi::PhysicalSize::new(self.width, self.height));
        match event_loop.create_window(attrs) {
            Ok(w) => {
                self.window = Some(Arc::new(w));
                self.created = true;
            }
            Err(_) => {
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        translate_window_event(self.queue_mut(), &event);
    }
}

/// Owns the winit event loop + app; created lazily by the windowed context.
pub struct WindowHost {
    event_loop: EventLoop<()>,
    app: App,
}

impl WindowHost {
    /// Create the event loop and pump once to spin up the window. Returns the
    /// host plus the created window handle (or an error string).
    pub fn create(
        width: u32,
        height: u32,
        title: String,
        queue: *mut EventQueue,
    ) -> Result<(Self, Arc<Window>), String> {
        let mut event_loop =
            EventLoop::new().map_err(|e| format!("EventLoop::new failed: {e}"))?;
        let mut app = App::new(width, height, title, queue);

        // Pump until the window is created (resumed fires on the first pump).
        for _ in 0..16 {
            event_loop.pump_app_events(Some(Duration::from_millis(0)), &mut app);
            if app.window.is_some() {
                break;
            }
        }
        let window = app
            .window
            .clone()
            .ok_or_else(|| "window was not created after pump".to_string())?;
        Ok((Self { event_loop, app }, window))
    }

    /// Pump pending OS events (non-blocking) into the event queue.
    pub fn pump(&mut self) {
        self.event_loop
            .pump_app_events(Some(Duration::from_millis(0)), &mut self.app);
    }
}
