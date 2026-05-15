use std::cell::RefCell;
use std::rc::Rc;

use calloop::{EventLoop, LoopHandle};
use util::ResultExt;

use crate::linux::headless::HeadlessDisplay;
use crate::linux::{LinuxClient, LinuxCommon, LinuxKeyboardLayout};
use gpui::{
    AnyWindowHandle, CursorStyle, DisplayId, HeadlessWindow, PlatformDisplay,
    PlatformKeyboardLayout, PlatformWindow, WindowParams,
};

#[cfg(feature = "x11")]
use gpui_wgpu::{DEFAULT_OFFSCREEN_HEIGHT, DEFAULT_OFFSCREEN_WIDTH, WgpuHeadlessRenderer};

pub struct HeadlessClientState {
    pub(crate) _loop_handle: LoopHandle<'static, HeadlessClient>,
    pub(crate) event_loop: Option<calloop::EventLoop<'static, HeadlessClient>>,
    pub(crate) common: LinuxCommon,
    /// Open windows, in z-order (single-window today, kept as a Vec so adding
    /// multi-window later is a one-line change).
    windows: Vec<AnyWindowHandle>,
    /// Cached display so multiple `displays()` calls return the same `Rc`.
    display: Rc<dyn PlatformDisplay>,
}

#[derive(Clone)]
pub(crate) struct HeadlessClient(Rc<RefCell<HeadlessClientState>>);

impl HeadlessClient {
    pub(crate) fn new() -> Self {
        let event_loop = EventLoop::try_new().unwrap();

        let (common, main_receiver) = LinuxCommon::new(event_loop.get_signal());

        let handle = event_loop.handle();

        handle
            .insert_source(main_receiver, |event, _, _: &mut HeadlessClient| {
                if let calloop::channel::Event::Msg(runnable) = event {
                    runnable.run();
                }
            })
            .ok();

        let display: Rc<dyn PlatformDisplay> = Rc::new(HeadlessDisplay::new());

        HeadlessClient(Rc::new(RefCell::new(HeadlessClientState {
            event_loop: Some(event_loop),
            _loop_handle: handle,
            common,
            windows: Vec::new(),
            display,
        })))
    }
}

impl LinuxClient for HeadlessClient {
    fn with_common<R>(&self, f: impl FnOnce(&mut LinuxCommon) -> R) -> R {
        f(&mut self.0.borrow_mut().common)
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(LinuxKeyboardLayout::new("unknown".into()))
    }

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        vec![self.0.borrow().display.clone()]
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.0.borrow().display.clone())
    }

    fn display(&self, id: DisplayId) -> Option<Rc<dyn PlatformDisplay>> {
        let state = self.0.borrow();
        if state.display.id() == id {
            Some(state.display.clone())
        } else {
            None
        }
    }

    #[cfg(feature = "screen-capture")]
    fn screen_capture_sources(
        &self,
    ) -> futures::channel::oneshot::Receiver<anyhow::Result<Vec<Rc<dyn gpui::ScreenCaptureSource>>>>
    {
        let (tx, rx) = futures::channel::oneshot::channel();
        tx.send(Err(anyhow::anyhow!(
            "Headless mode does not support screen capture."
        )))
        .ok();
        rx
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        // Last-opened window, matching the X11/Wayland behaviour where the
        // most recently focused window is the "active" one. `dispatch_action`
        // routes through here, so returning `None` (the old stub) silently
        // dropped action dispatches in headless mode.
        self.0.borrow().windows.last().copied()
    }

    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        let stack = self.0.borrow().windows.clone();
        if stack.is_empty() { None } else { Some(stack) }
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        params: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        let display = self.0.borrow().display.clone();

        // The wgpu offscreen renderer is the *only* headless renderer the fork
        // ships. Gated on the `x11` feature for build-graph hygiene (that's
        // what brings `gpui_wgpu` in on Linux); leaving it off means the
        // user explicitly asked for a no-GPU build, in which case headless
        // open_window has to bail.
        #[cfg(feature = "x11")]
        let renderer: Option<Box<dyn gpui::PlatformHeadlessRenderer>> = {
            let width =
                (params.bounds.size.width.as_f32() as u32).clamp(1, DEFAULT_OFFSCREEN_WIDTH);
            let height =
                (params.bounds.size.height.as_f32() as u32).clamp(1, DEFAULT_OFFSCREEN_HEIGHT);
            match WgpuHeadlessRenderer::new(width, height) {
                Ok(r) => Some(Box::new(r) as Box<dyn gpui::PlatformHeadlessRenderer>),
                Err(e) => {
                    log::warn!(
                        "Headless wgpu renderer init failed ({e}); proceeding without offscreen \
                         rendering — `workspace.screenshot` will return an error."
                    );
                    None
                }
            }
        };
        #[cfg(not(feature = "x11"))]
        let renderer: Option<Box<dyn gpui::PlatformHeadlessRenderer>> = {
            log::warn!(
                "Headless build has no wgpu feature; offscreen rendering disabled. \
                 Build with the `x11` feature to enable `workspace.screenshot`."
            );
            None
        };

        let window =
            HeadlessWindow::new(handle, params, display, /* scale_factor */ 1.0, renderer);

        // Track the handle so `active_window` / `window_stack` reflect reality.
        self.0.borrow_mut().windows.push(handle);

        Ok(Box::new(window))
    }

    fn compositor_name(&self) -> &'static str {
        "headless"
    }

    fn set_cursor_style(&self, _style: CursorStyle) {}

    fn open_uri(&self, _uri: &str) {}

    fn reveal_path(&self, _path: std::path::PathBuf) {}

    fn write_to_primary(&self, _item: gpui::ClipboardItem) {}

    fn write_to_clipboard(&self, _item: gpui::ClipboardItem) {}

    fn read_from_primary(&self) -> Option<gpui::ClipboardItem> {
        None
    }

    fn read_from_clipboard(&self) -> Option<gpui::ClipboardItem> {
        None
    }

    fn run(&self) {
        let mut event_loop = self
            .0
            .borrow_mut()
            .event_loop
            .take()
            .expect("App is already running");

        event_loop.run(None, &mut self.clone(), |_| {}).log_err();
    }
}
