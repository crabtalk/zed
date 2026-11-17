#![cfg(feature = "test-support")]

use gpui::{
    Context, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _, Render,
    ScrollDelta, ScrollEvent, ScrollHandle, ScrollSource, StatefulInteractiveElement as _,
    Styled as _, TestAppContext, TouchPhase, VisualTestContext, Window, div, point, px, size,
};

struct ScrollPane {
    scroll: ScrollHandle,
}

impl Render for ScrollPane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("trackpad-pane")
            .size_full()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .child(div().w(px(1000.)).h(px(1000.)))
    }
}

#[gpui::test]
fn slow_horizontal_trackpad_scroll_survives_a_vertical_start(cx: &mut TestAppContext) {
    let scroll = ScrollHandle::new();
    let window = cx.add_window(|_, _| ScrollPane {
        scroll: scroll.clone(),
    });
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_resize(size(px(400.), px(300.)));
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.simulate_event(ScrollEvent {
        position: point(px(100.), px(100.)),
        delta: ScrollDelta::Pixels(point(px(-0.2), px(-0.6))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Started,
        source: ScrollSource::Pointer,
    });
    for _ in 0..10 {
        let before = scroll.offset().x;
        visual.simulate_event(ScrollEvent {
            position: point(px(100.), px(100.)),
            delta: ScrollDelta::Pixels(point(px(-2.5), px(-0.1))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
            source: ScrollSource::Pointer,
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(scroll.offset().x, before - px(2.5));
    }
}
