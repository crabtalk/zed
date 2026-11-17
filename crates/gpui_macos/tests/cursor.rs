fn main() {
    #[cfg(target_os = "macos")]
    native::run();
}

#[cfg(target_os = "macos")]
mod native {
    use gpui::{
        App, AppContext, Application, Context, CursorStyle, IntoElement, Platform, Render, Window,
        WindowOptions, div,
    };
    use gpui_macos::MacPlatform;
    use libtest_mimic::{Arguments, Trial};
    use objc2::{MainThreadMarker, msg_send, runtime::AnyObject};
    use objc2_app_kit::{NSCursor, NSView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{ptr, rc::Rc};

    struct CursorView;

    impl Render for CursorView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    pub fn run() {
        let mut arguments = Arguments::from_args();
        arguments.test_threads = Some(1);
        libtest_mimic::run(
            &arguments,
            vec![Trial::test(
                "native_cursor_survives_composition_updates",
                || {
                    check_native_cursor();
                    Ok(())
                },
            )],
        )
        .exit();
    }

    fn check_native_cursor() {
        let platform = Rc::new(MacPlatform::new(false));
        let cursor_platform = platform.clone();
        Application::with_platform(platform).run(move |cx: &mut App| {
            let handle = cx
                .open_window(
                    WindowOptions {
                        show: true,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|_| CursorView),
                )
                .expect("open cursor test window");
            cursor_platform.activate(true);
            handle
                .update(cx, |_, window, _| {
                    window.on_next_frame(move |window, _| {
                        window.on_next_frame(move |window, cx| {
                            let RawWindowHandle::AppKit(raw) =
                                HasWindowHandle::window_handle(window)
                                    .expect("native window handle")
                                    .as_raw()
                            else {
                                panic!("expected AppKit window");
                            };
                            // SAFETY: the window owns this live NSView, and this test runs on AppKit's main thread.
                            let view = unsafe { &*raw.ns_view.as_ptr().cast::<NSView>() };
                            let native_window = view.window().expect("view belongs to a window");
                            native_window.makeKeyAndOrderFront(None);
                            assert!(native_window.isKeyWindow(), "cursor test window is key");

                            for style in [
                                CursorStyle::PointingHand,
                                CursorStyle::IBeam,
                                CursorStyle::Arrow,
                            ] {
                                cursor_platform.set_cursor_style(style);
                                let expected = NSCursor::currentCursor();
                                if style == CursorStyle::PointingHand {
                                    assert_eq!(expected, NSCursor::pointingHandCursor());
                                } else if style == CursorStyle::IBeam {
                                    assert_eq!(expected, NSCursor::IBeamCursor());
                                } else {
                                    assert_eq!(expected, NSCursor::arrowCursor());
                                }

                                NSCursor::crosshairCursor().set();
                                cursor_platform.set_cursor_style(style);
                                assert_eq!(
                                    NSCursor::currentCursor(),
                                    expected,
                                    "reapply {style:?}"
                                );

                                let child =
                                    NSView::new(MainThreadMarker::new().expect("main thread"));
                                view.addSubview(&child);
                                NSCursor::crosshairCursor().set();
                                // SAFETY: NSView forwards cursorUpdate: through its responder chain;
                                // GPUIView's callback does not read the event argument.
                                unsafe {
                                    let _: () =
                                        msg_send![&*child, cursorUpdate: ptr::null::<AnyObject>()];
                                }
                                assert_eq!(
                                    NSCursor::currentCursor(),
                                    expected,
                                    "cursor update {style:?}"
                                );
                                child.removeFromSuperview();
                            }
                            window.remove_window();
                            cx.quit();
                        });
                    });
                })
                .expect("schedule native cursor check");
        });
    }
}
