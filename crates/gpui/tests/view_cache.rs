use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, StyleRefinement, Styled as _, TestAppContext, VisualTestContext, Window, div, px, rgb,
    size,
};
use std::{cell::Cell, rc::Rc};

struct Body {
    renders: Rc<Cell<usize>>,
    interactive: bool,
}
impl Render for Body {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let body = div()
            .size_full()
            .bg(rgb(0xff0000))
            .debug_selector(|| "cached-body".into());
        if self.interactive {
            body.id("interactive-body")
                .on_mouse_down(gpui::MouseButton::Left, |_, _, _| {})
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
                    child: cx.new(|_| Body {
                        renders,
                        interactive,
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
