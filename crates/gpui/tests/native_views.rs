use gpui::{
    self, Bounds, ContentMask, Context, FocusHandle, HitboxBehavior, IntoElement, Pixels, Quad,
    Render, ScaledPixels, Scene, Styled as _, TestAppContext, Window,
    native_view::{NativeView, NativeViewState, Viewport},
};
use raw_window_handle::{HandleError, HasWindowHandle, WindowHandle};
use std::rc::Rc;

struct ViewHandle;

impl HasWindowHandle for ViewHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

fn bounds() -> Bounds<ScaledPixels> {
    Bounds::new(
        gpui::point(ScaledPixels(0.), ScaledPixels(0.)),
        gpui::size(ScaledPixels(100.), ScaledPixels(80.)),
    )
}

fn native(view: Rc<dyn HasWindowHandle>) -> NativeView {
    NativeView {
        view,
        viewport: Viewport::try_from(gpui::size(Pixels::from(100.), Pixels::from(80.))).unwrap(),
        state: Rc::new(NativeViewState::default()),
        focus: None,
        focused: false,
        window: None,
        presented: None,
        bounds: bounds(),
        content_mask: ContentMask::new(bounds()),
        order: 0,
        parent: None,
        opacity: 1.,
        interactive: false,
        hitbox: None,
        occlusions: Vec::new(),
    }
}

fn quad() -> Quad {
    Quad {
        bounds: bounds(),
        content_mask: ContentMask::new(bounds()),
        ..Quad::default()
    }
}

#[test]
fn a_native_view_splits_an_enclosing_gpu_layer() {
    let mut scene = Scene::default();
    scene.push_layer();
    scene.insert_primitive(quad());
    scene.insert_native_view(native(Rc::new(ViewHandle)));
    scene.insert_primitive(quad());
    scene.pop_layer();
    scene.finish();
    let order = scene.native_views[0].order;
    assert!(scene.quads[0].order < order);
    assert!(scene.quads[1].order > order);
    assert_eq!(scene.batches_in(..order).count(), 1);
    assert_eq!(scene.batches_in(order + 1..).count(), 1);
}

#[test]
fn cached_replay_retains_the_view_and_rebases_its_stacking() {
    let view: Rc<dyn HasWindowHandle> = Rc::new(ViewHandle);
    let weak = Rc::downgrade(&view);
    let mut previous = Scene::default();
    previous.insert_primitive(quad());
    previous.insert_native_view(native(view.clone()));
    previous.insert_primitive(quad());
    previous.finish();
    let mut next = Scene::default();
    next.insert_primitive(quad());
    next.replay(0..previous.len(), &previous);
    next.finish();
    let order = next.native_views[0].order;
    assert!(next.quads[1].order < order);
    assert!(next.quads[2].order > order);
    assert!(Rc::ptr_eq(&next.native_views[0].view, &view));
    drop(previous);
    drop(view);
    assert!(weak.upgrade().is_some());
    next.clear();
    assert!(weak.upgrade().is_none());
    next.insert_primitive(quad());
    next.finish();
    assert!(next.quads[0].order < order);
}

#[test]
fn a_fully_clipped_view_is_not_retained_by_the_scene() {
    let view: Rc<dyn HasWindowHandle> = Rc::new(ViewHandle);
    let weak = Rc::downgrade(&view);
    let mut frame = native(view);
    frame.content_mask = ContentMask::new(Bounds::new(
        gpui::point(ScaledPixels(200.), ScaledPixels(0.)),
        bounds().size,
    ));
    let mut scene = Scene::default();
    scene.insert_native_view(frame);
    assert!(scene.native_views.is_empty());
    assert_eq!(scene.len(), 0);
    assert!(weak.upgrade().is_none());
}

#[test]
fn cached_pages_share_live_input_but_keep_their_layout_viewport() {
    let mut previous = Scene::default();
    let view = native(Rc::new(ViewHandle));
    let state = view.state.clone();
    let viewport = view.viewport;
    previous.insert_native_view(view);
    previous.finish();
    let mut next = Scene::default();
    next.replay(0..previous.len(), &previous);
    next.finish();
    state.enabled.set(true);
    state.request_focus(true, None);
    let request = state.focus.get();
    let replay = &mut next.native_views[0];
    replay.bounds.size = gpui::size(ScaledPixels(25.), ScaledPixels(20.));
    assert_eq!(replay.viewport, viewport);
    assert!(replay.state.enabled.get());
    assert_eq!(replay.state.focus.get(), request);
    state.enabled.set(false);
    state.request_focus(false, None);
    assert!(!replay.state.enabled.get());
    assert!(!replay.state.focus.get().active);
    assert_ne!(replay.state.focus.get().generation, request.generation);
}

#[test]
fn invalid_viewports_cannot_reach_native_layout() {
    for (width, height) in [
        (0., 80.),
        (100., -1.),
        (f32::NAN, 80.),
        (100., f32::INFINITY),
    ] {
        assert!(Viewport::try_from(gpui::size(gpui::px(width), gpui::px(height))).is_err());
    }
    assert!(Viewport::try_from(gpui::size(gpui::px(1.), gpui::px(1.))).is_ok());
}

struct PageScene {
    state: Rc<NativeViewState>,
    focus: FocusHandle,
}

impl Render for PageScene {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        window.native_surfaces.views = true;
        let state = self.state.clone();
        let focus = self.focus.clone();
        gpui::canvas(
            {
                let focus = focus.clone();
                move |bounds, window, cx| {
                    window.set_focus_handle(&focus, cx);
                    window.insert_hitbox(bounds, HitboxBehavior::BlockMouse)
                }
            },
            move |bounds, hitbox, window, _| {
                let viewport =
                    Viewport::try_from(gpui::size(gpui::px(100.), gpui::px(80.))).unwrap();
                window
                    .paint_native_view(
                        NativeView::new(Rc::new(ViewHandle), viewport, state),
                        bounds,
                        Some(&focus),
                        &hitbox,
                    )
                    .unwrap();
            },
        )
        .size_full()
    }
}

#[gpui::test]
fn ordinary_focus_clears_native_keyboard_ownership(cx: &mut TestAppContext) {
    let state = Rc::new(NativeViewState::default());
    state.enabled.set(true);
    let handle = cx.add_window({
        let state = state.clone();
        move |_, cx| PageScene {
            state,
            focus: cx.focus_handle(),
        }
    });
    let page = handle.root(cx).unwrap();
    let native_focus = cx.update(|cx| page.read(cx).focus.clone());
    let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.focus(&native_focus, cx);
        assert!(state.focus.get().active);
        let request = state.focus.get();
        window.draw(cx).clear(cx);
        assert_eq!(state.focus.get(), request);
        let control = cx.focus_handle();
        window.focus(&control, cx);
        assert!(!state.focus.get().active);
        state.enabled.set(false);
        window.focus(&native_focus, cx);
        assert!(!state.focus.get().active);
    });
}
