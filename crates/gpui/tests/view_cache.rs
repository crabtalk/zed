use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, Modifiers, MouseButton, ParentElement as _, Render, StyleRefinement, Styled as _,
    TestAppContext, VisualTestContext, Window, div, point, px, rgb, size,
};
use std::{cell::Cell, rc::Rc};

struct Body {
    renders: Rc<Cell<usize>>,
    interactive: bool,
    focus: FocusHandle,
    clicks: usize,
    keys: usize,
}
impl Render for Body {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let body = div()
            .size_full()
            .bg(rgb(0xff0000))
            .debug_selector(|| "cached-body".into());
        if self.interactive {
            body.id("interactive-body")
                .track_focus(&self.focus)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.clicks += 1;
                        window.focus(&this.focus, cx);
                    }),
                )
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
                    if event.keystroke.key == "x" {
                        this.keys += 1;
                    }
                }))
                .into_any_element()
        } else {
            body.into_any_element()
        }
    }
}
struct Host {
    child: Entity<Body>,
    x: f32,
}
impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().relative().child(
            div().absolute().left(px(self.x)).top(px(20.)).child(
                self.child
                    .clone()
                    .cached_movable(StyleRefinement::default().w(px(100.)).h(px(80.))),
            ),
        )
    }
}
struct Root(Entity<Host>, usize, f32);
impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .opacity(self.2)
            .relative()
            .children((0..self.1).map(|index| {
                div()
                    .id(("prefix", index))
                    .on_mouse_down(MouseButton::Left, |_, _, _| {})
                    .absolute()
                    .left(px(450.))
                    .top(px(index as f32 * 10.))
                    .w(px(10.))
                    .h(px(10.))
                    .bg(rgb(0x00ff00))
                    .debug_selector(move || format!("prefix-{index}"))
            }))
            .child(
                self.0
                    .clone()
                    .cached(StyleRefinement::default().size_full()),
            )
    }
}
fn open(
    interactive: bool,
    cx: &mut TestAppContext,
) -> (
    Entity<Root>,
    Entity<Host>,
    Rc<Cell<usize>>,
    VisualTestContext,
) {
    let renders = Rc::new(Cell::new(0));
    let window = cx.add_window({
        let renders = renders.clone();
        move |_, cx| {
            Root(
                cx.new(|cx| Host {
                    child: cx.new(|cx| Body {
                        renders,
                        interactive,
                        focus: cx.focus_handle(),
                        clicks: 0,
                        keys: 0,
                    }),
                    x: 20.,
                }),
                0,
                1.,
            )
        }
    });
    let root = window.root(cx).unwrap();
    let host = cx.update(|cx| root.read(cx).0.clone());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_resize(size(px(600.), px(400.)));
    visual.update(|window, cx| window.draw(cx).clear(cx));
    (root, host, renders, visual)
}
#[gpui::test]
fn cached_child_survives_ancestor_redraw_and_passive_movement(cx: &mut TestAppContext) {
    let (root, host, renders, mut cx) = open(false, cx);
    let before = renders.get();
    root.update(&mut cx, |root, cx| {
        root.1 = 4;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(renders.get(), before);
    host.update(&mut cx, |_, cx| cx.notify());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(renders.get(), before);
    host.update(&mut cx, |host, cx| {
        host.x = 60.;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(renders.get(), before);
    assert_eq!(cx.debug_bounds("cached-body").unwrap().origin.x, px(60.));
    let child = cx.update(|_, cx| host.read(cx).child.clone());
    child.update(&mut cx, |_, cx| cx.notify());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(renders.get(), before + 1);
    let previous = renders.get();
    root.update(&mut cx, |root, cx| {
        root.2 = 0.5;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(renders.get() > previous);
}
#[gpui::test]
fn movement_rebuilds_interactive_or_previously_clipped_content(cx: &mut TestAppContext) {
    let (_, host, renders, mut cx) = open(true, cx);
    let before = renders.get();
    host.update(&mut cx, |host, cx| {
        host.x = 60.;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(renders.get(), before + 1);
    let (_, host, renders, mut cx) = open(false, &mut cx.cx);
    host.update(&mut cx, |host, cx| {
        host.x = -10.;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let clipped = renders.get();
    host.update(&mut cx, |host, cx| {
        host.x = 20.;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(renders.get(), clipped + 1);
}

#[gpui::test]
fn cached_interactions_survive_sibling_changes_and_movement(cx: &mut TestAppContext) {
    let (root, host, renders, mut cx) = open(true, cx);
    let child = cx.update(|_, cx| host.read(cx).child.clone());
    let focus = cx.update(|_, cx| child.read(cx).focus.clone());
    let before = renders.get();
    for count in [4, 8, 4] {
        root.update(&mut cx, |root, cx| {
            root.1 = count;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(renders.get(), before);
    }
    cx.simulate_click(point(px(70.), px(60.)), Modifiers::none());
    assert!(cx.update(|window, _| focus.is_focused(window)));
    cx.simulate_keystrokes("x");
    cx.update(|_, cx| {
        assert_eq!(child.read(cx).clicks, 1);
        assert_eq!(child.read(cx).keys, 1);
    });
    host.update(&mut cx, |host, cx| {
        host.x = 200.;
        cx.notify();
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.blur(cx);
    });
    cx.simulate_click(point(px(70.), px(60.)), Modifiers::none());
    assert!(!cx.update(|window, _| focus.is_focused(window)));
    cx.update(|_, cx| assert_eq!(child.read(cx).clicks, 1));
    cx.simulate_click(point(px(250.), px(60.)), Modifiers::none());
    assert!(cx.update(|window, _| focus.is_focused(window)));
    cx.simulate_keystrokes("x");
    cx.update(|_, cx| {
        assert_eq!(child.read(cx).clicks, 2);
        assert_eq!(child.read(cx).keys, 2);
    });
}

#[gpui::test]
fn fractional_movement_uses_device_pixels_and_scale_changes_rebuild(cx: &mut TestAppContext) {
    let (_, host, renders, mut cx) = open(false, cx);
    for scale in [1., 2.] {
        let previous = renders.get();
        cx.simulate_scale_factor_change(scale);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(renders.get() > previous);
        let before = renders.get();
        for x in [20.25, 20.5, 20.75, 21., 20.] {
            host.update(&mut cx, |host, cx| {
                host.x = x;
                cx.notify();
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(renders.get(), before, "x={x}, scale={scale}");
            let actual = cx.debug_bounds("cached-body").unwrap().origin.x;
            let expected = cx.update(|window, _| window.pixel_snap(px(x)));
            assert_eq!(actual, expected, "x={x}, scale={scale}");
            assert_eq!((f32::from(actual) * scale).fract(), 0.);
        }
    }
}
