use clipboard::ClipboardSupport;
use copypasta::ClipboardContext;
use error::{Result, OverlayError};
use glium::glutin;
use glium::glutin::event::{Event, WindowEvent};
use glium::glutin::event_loop::{ControlFlow, EventLoop};
use glium::glutin::platform::windows::WindowExtWindows;
use glium::glutin::window::{WindowBuilder, Window};
use glium::{Display, Surface};
use imgui::{Context, FontConfig, FontSource, Io};
use imgui_glium_renderer::Renderer;
use imgui_winit_support::{HiDpiMode, WinitPlatform};
use input::InputSystem;
use window_tracker::WindowTracker;
use windows::core::PCSTR;
use std::ffi::CString;
use std::time::Instant;
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::Graphics::Dwm::{
    DwmEnableBlurBehindWindow, DWM_BB_BLURREGION, DWM_BB_ENABLE, DWM_BLURBEHIND,
};
use windows::Win32::Graphics::Gdi::CreateRectRgn;
use windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrA, SetWindowLongA, SetWindowLongPtrA, SetWindowPos,
    GWL_EXSTYLE, GWL_STYLE, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, WS_CLIPSIBLINGS,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE, MessageBoxA, MB_ICONERROR, MB_OK, ShowWindow, SW_SHOW,
};

mod clipboard;
mod error;
mod input;
mod window_tracker;

pub fn show_error_message(title: &str, message: &str) {
    let title = CString::new(title).unwrap_or_else(|_| CString::new("[[ NulError ]]").unwrap());
    let message = CString::new(message).unwrap_or_else(|_| CString::new("[[ NulError ]]").unwrap());
    unsafe {
        MessageBoxA(
            HWND::default(), 
            PCSTR::from_raw(message.as_ptr() as *const u8), 
            PCSTR::from_raw(title.as_ptr() as *const u8),
            MB_ICONERROR | MB_OK
        );
    }
}

pub struct System {
    pub event_loop: EventLoop<()>,
    pub display: glium::Display,
    pub imgui: Context,
    pub platform: WinitPlatform,
    pub renderer: Renderer,
    pub font_size: f32,
    pub window_tracker: WindowTracker,
}

pub fn init(title: &str, target_window: &str) -> Result<System> {
    let window_tracker = WindowTracker::new(target_window)?;

    let event_loop = EventLoop::new();
    let context = glutin::ContextBuilder::new().with_vsync(false);

    /* TODO: Replace with target which ether is a monitor or a window! */
    let target_monitor = event_loop
        .primary_monitor()
        .or_else(|| event_loop.available_monitors().next())
        .ok_or(OverlayError::NoMonitorAvailable)?;

    let builder = WindowBuilder::new()
        .with_resizable(false)
        .with_title(title.to_owned())
        .with_inner_size(target_monitor.size())
        .with_position(target_monitor.position())
        .with_visible(false);

    let display = Display::new(builder, context, &event_loop)
        .map_err(OverlayError::DisplayError)?;

    let mut imgui = Context::create();
    imgui.set_ini_filename(None);

    match ClipboardContext::new() {
        Ok(backend) => imgui.set_clipboard_backend(ClipboardSupport(backend)),
        Err(error) => log::warn!("Failed to initialize clipboard: {}", error),
    };

    let mut platform = WinitPlatform::init(&mut imgui);
    {
        let gl_window = display.gl_window();
        let window = gl_window.window();
        platform.attach_window(imgui.io_mut(), window, HiDpiMode::Default);
    }

    // Fixed font size. Note imgui_winit_support uses "logical
    // pixels", which are physical pixels scaled by the devices
    // scaling factor. Meaning, 13.0 pixels should look the same size
    // on two different screens, and thus we do not need to scale this
    // value (as the scaling is handled by winit)
    let font_size = 18.0;

    imgui.fonts().add_font(&[FontSource::TtfData {
        data: include_bytes!("../resources/Roboto-Regular.ttf"),
        size_pixels: font_size,
        config: Some(FontConfig {
            // As imgui-glium-renderer isn't gamma-correct with
            // it's font rendering, we apply an arbitrary
            // multiplier to make the font a bit "heavier". With
            // default imgui-glow-renderer this is unnecessary.
            rasterizer_multiply: 1.5,
            // Oversampling font helps improve text rendering at
            // expense of larger font atlas texture.
            oversample_h: 4,
            oversample_v: 4,
            ..FontConfig::default()
        }),
    }]);

    {
        let window = display.gl_window();
        let window = window.window();

        window.set_decorations(false);
        window.set_undecorated_shadow(false);

        let hwnd = HWND(window.hwnd());
        unsafe {
            // Make it transparent
            SetWindowLongA(
                hwnd,
                GWL_STYLE,
                (WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS).0 as i32,
            );
            SetWindowLongPtrA(
                hwnd,
                GWL_EXSTYLE,
                (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE).0
                    as isize,
            );

            let mut bb: DWM_BLURBEHIND = Default::default();
            bb.dwFlags = DWM_BB_ENABLE | DWM_BB_BLURREGION;
            bb.fEnable = BOOL::from(true);
            bb.hRgnBlur = CreateRectRgn(0, 0, 1, 1);
            DwmEnableBlurBehindWindow(hwnd, &bb)?;

            // Move the window to the top
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        }
    }

    let renderer = Renderer::init(&mut imgui, &display)
        .map_err(OverlayError::RenderError)?;

    Ok(System {
        event_loop,
        display,
        imgui,
        platform,
        renderer,
        font_size,
        window_tracker,
    })
}

/// Toggles the overlay noactive and transparent state
/// according to whenever ImGui wants mouse/cursor grab.
struct OverlayActiveTracker {
    currently_active: bool
}

impl OverlayActiveTracker {
    pub fn new() -> Self {
        Self { currently_active: false }
    }

    pub fn update(&mut self, window: &Window, io: &Io) {
        let window_active = io.want_capture_mouse | io.want_capture_keyboard;
        if window_active == self.currently_active {
            return;
        }

        self.currently_active = window_active;
        unsafe {
            let hwnd = HWND(window.hwnd());
            let mut style = GetWindowLongPtrA(hwnd, GWL_EXSTYLE);
            if window_active {
                style &= !(WS_EX_NOACTIVATE.0 as isize);
            } else {
                style |= WS_EX_NOACTIVATE.0 as isize;
            }

            //log::debug!("Set UI active: {window_active}");
            SetWindowLongPtrA(hwnd, GWL_EXSTYLE, style);
            if window_active {
                SetActiveWindow(hwnd);
            }
        }
    }
}

impl System {
    pub fn main_loop<U, R>(self, mut update: U, mut render: R) -> !
    where
        U: FnMut(&mut imgui::Context) -> bool + 'static,
        R: FnMut(&mut imgui::Ui) -> bool + 'static,
    {
        let System {
            event_loop,
            display,
            mut imgui,
            mut platform,
            mut renderer,
            mut window_tracker,
            ..
        } = self;
        let mut last_frame = Instant::now();

        let mut active_tracker = OverlayActiveTracker::new();
        let mut input_system = InputSystem::new();
        let mut initial_render = true;

        event_loop.run(move |event, _, control_flow| match event {
            Event::NewEvents(_) => {
                let now = Instant::now();
                imgui.io_mut().update_delta_time(now - last_frame);
                last_frame = now;
            }
            Event::MainEventsCleared => {
                let gl_window = display.gl_window();
                if let Err(error) = platform.prepare_frame(imgui.io_mut(), gl_window.window()) {
                    *control_flow = ControlFlow::ExitWithCode(1);
                    log::error!("Platform implementation prepare_frame failed: {}", error);
                    return;
                }

                let window = gl_window.window();
                input_system.update(window, imgui.io_mut());
                active_tracker.update(window, imgui.io());
                window_tracker.update(window);

                if !update(&mut imgui) {
                    *control_flow = ControlFlow::Exit;
                    return;
                }

                window.request_redraw();
            }
            Event::RedrawRequested(_) => {
                let gl_window = display.gl_window();
                let ui = imgui.frame();

                let mut run = render(ui);

                let mut target = display.draw();
                target.clear_all((0.0, 0.0, 0.0, 0.0), 0.0, 0);
                platform.prepare_render(ui, gl_window.window());

                let draw_data = imgui.render();

                if let Err(error) = renderer.render(&mut target, draw_data) {
                    log::error!("Failed to render ImGui draw data: {}", error);
                    run = false;
                } else if let Err(error) = target.finish() {
                    log::error!("Failed to swap render buffers: {}", error);
                    run = false;
                }
                
                if !run {
                    *control_flow = ControlFlow::Exit;
                }

                if initial_render {
                    initial_render = false;
                    // Note:
                    // We can not use `gl_window.window().set_visible(true)` as this will prevent the overlay
                    // to be click trough...
                    unsafe { ShowWindow(HWND(gl_window.window().hwnd() as isize), SW_SHOW); }
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            event => {
                let gl_window = display.gl_window();
                platform.handle_event(imgui.io_mut(), gl_window.window(), &event);
            }
        })
    }
}
