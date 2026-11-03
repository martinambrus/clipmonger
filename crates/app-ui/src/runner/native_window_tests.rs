//! Native regression control. It checks real HWND styles, not shell MRU order.

#![allow(unsafe_code)]

use super::*;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongW, SetWindowLongW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};
use winit::platform::windows::EventLoopBuilderExtWindows as _;

struct Idle;

impl App for Idle {
    fn logic(&mut self, _: &Context, _: WindowKind) {}
    fn ui(&mut self, _: &mut egui::Ui, _: WindowKind) {}
    fn desired(&self) -> Desired {
        Desired::default()
    }
}

fn style(window: &Window) -> u32 {
    let hwnd = HWND(window::hwnd(window).unwrap() as *mut std::ffi::c_void);
    // SAFETY: called on the event-loop thread while the borrowed window lives.
    unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE).cast_unsigned() }
}

fn assert_popup_style(window: &Window) {
    let style = style(window);
    assert_ne!(style & WS_EX_TOOLWINDOW.0, 0, "popup must be a tool window");
    assert_eq!(
        style & WS_EX_APPWINDOW.0,
        0,
        "popup must not be an app window"
    );
    assert_eq!(
        style & WS_EX_NOACTIVATE.0,
        0,
        "popup must still accept focus"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop; creates only owned fixture windows"]
fn popup_stays_transient_across_native_style_changes() {
    struct Fixture {
        runner: Runner<Idle>,
        checked: bool,
    }
    impl ApplicationHandler<UserEvent> for Fixture {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.runner.start(event_loop).unwrap();
            self.runner.set_settings_open(event_loop, true).unwrap();
            let popup = self.runner.popup.host.as_ref().unwrap();
            let popup_id = popup.window.id();
            assert_popup_style(&popup.window);
            let hwnd = HWND(window::hwnd(&popup.window).unwrap() as *mut std::ffi::c_void);
            for _ in 0..3 {
                self.runner.set_popup_open(event_loop, true, None).unwrap();
                let popup = self.runner.popup.host.as_ref().unwrap();
                assert_eq!(popup.window.id(), popup_id, "popup identity must be stable");
                assert_popup_style(&popup.window);
                popup.window.set_resizable(true);
                popup.window.set_resizable(false);
                assert_popup_style(&popup.window);
                // Simulate the complete replacement of EXSTYLE performed by
                // pinned winit, preserving an unrelated high bit as a control.
                let proposed =
                    (style(&popup.window) | WS_EX_APPWINDOW.0 | 0x0040_0000) & !WS_EX_TOOLWINDOW.0;
                // SAFETY: hwnd is this fixture's live popup on its owning thread.
                unsafe { SetWindowLongW(hwnd, GWL_EXSTYLE, proposed.cast_signed()) };
                assert_popup_style(&popup.window);
                assert_ne!(style(&popup.window) & 0x0040_0000, 0);
                self.runner.set_popup_open(event_loop, false, None).unwrap();
                assert_popup_style(&self.runner.popup.host.as_ref().unwrap().window);
            }
            let settings = self
                .runner
                .settings
                .as_ref()
                .unwrap()
                .host
                .as_ref()
                .unwrap();
            assert_ne!(style(&settings.window) & WS_EX_APPWINDOW.0, 0);
            assert_eq!(style(&settings.window) & WS_EX_TOOLWINDOW.0, 0);
            self.runner.set_settings_open(event_loop, false).unwrap();
            self.runner.set_settings_open(event_loop, true).unwrap();
            let settings = self
                .runner
                .settings
                .as_ref()
                .unwrap()
                .host
                .as_ref()
                .unwrap();
            assert_ne!(style(&settings.window) & WS_EX_APPWINDOW.0, 0);
            assert_eq!(style(&settings.window) & WS_EX_TOOLWINDOW.0, 0);
            self.checked = true;
            event_loop.exit();
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    let mut fixture = Fixture {
        runner: Runner::new(
            Config {
                popup_title: "Transient regression fixture".into(),
                popup_size: [100.0, 100.0],
                popup_position: None,
                settings_title: "Settings regression fixture".into(),
                settings_size: [100.0, 100.0],
                settings_state_path: None,
            },
            event_loop.create_proxy(),
            Box::new(|_| Idle),
        ),
        checked: false,
    };
    event_loop.run_app(&mut fixture).unwrap();
    assert!(fixture.checked);
}
