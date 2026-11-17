use crate::{
    Axis, Point, TouchPhase,
    gestures::{OngoingScroll, SCROLL_EVENT_SEPARATION},
    point, px,
};
use scheduler::Instant;
use std::time::Duration;

#[test]
fn ongoing_scroll_locks_to_dominant_axis() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    let mut horizontal_delta = point(px(10.), px(2.));
    ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Started, now);
    assert_eq!(ongoing_scroll.axis, Some(Axis::Horizontal));
    assert_eq!(horizontal_delta, point(px(10.), px(0.)));

    let mut continued_delta = point(px(3.), px(2.));
    ongoing_scroll.filter_at(
        &mut continued_delta,
        TouchPhase::Moved,
        now + Duration::from_millis(1),
    );
    assert_eq!(ongoing_scroll.axis, Some(Axis::Horizontal));
    assert_eq!(continued_delta, point(px(3.), px(0.)));
}

#[test]
fn ongoing_scroll_unlocks_when_direction_changes() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    let mut horizontal_delta = point(px(10.), px(2.));
    ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Started, now);

    let mut vertical_delta = point(px(2.), px(10.));
    ongoing_scroll.filter_at(
        &mut vertical_delta,
        TouchPhase::Moved,
        now + Duration::from_millis(1),
    );
    assert_eq!(ongoing_scroll.axis, None);
    assert_eq!(vertical_delta, point(px(2.), px(10.)));
}

#[test]
fn ongoing_scroll_starts_new_gesture_at_timeout_boundary() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    let mut horizontal_delta = point(px(10.), px(2.));
    ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Moved, now);

    let mut vertical_delta = point(px(2.), px(10.));
    ongoing_scroll.filter_at(
        &mut vertical_delta,
        TouchPhase::Moved,
        now + SCROLL_EVENT_SEPARATION,
    );
    assert_eq!(ongoing_scroll.axis, Some(Axis::Vertical));
    assert_eq!(vertical_delta, point(px(0.), px(10.)));
}

#[test]
fn ongoing_scroll_ignores_zero_delta_and_resets_when_ended() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    let mut horizontal_delta = point(px(10.), px(2.));
    ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Started, now);

    let mut zero_delta = Point::default();
    ongoing_scroll.filter_at(
        &mut zero_delta,
        TouchPhase::Ended,
        now + Duration::from_millis(1),
    );
    assert_eq!(ongoing_scroll.axis, None);

    let mut vertical_delta = point(px(2.), px(10.));
    ongoing_scroll.filter_at(
        &mut vertical_delta,
        TouchPhase::Moved,
        now + Duration::from_millis(2),
    );
    assert_eq!(ongoing_scroll.axis, Some(Axis::Vertical));
    assert_eq!(vertical_delta, point(px(0.), px(10.)));
}

#[test]
fn ongoing_scroll_ignores_zero_delta_movement() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    let mut horizontal_delta = point(px(10.), px(2.));
    ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Started, now);

    let mut zero_delta = Point::default();
    ongoing_scroll.filter_at(
        &mut zero_delta,
        TouchPhase::Moved,
        now + SCROLL_EVENT_SEPARATION,
    );

    let mut vertical_delta = point(px(2.), px(10.));
    ongoing_scroll.filter_at(
        &mut vertical_delta,
        TouchPhase::Moved,
        now + SCROLL_EVENT_SEPARATION,
    );
    assert_eq!(ongoing_scroll.axis, Some(Axis::Vertical));
    assert_eq!(vertical_delta, point(px(0.), px(10.)));
}

#[test]
fn ongoing_scroll_supports_moved_only_platforms() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    let mut horizontal_delta = point(px(10.), px(2.));
    ongoing_scroll.filter_at(&mut horizontal_delta, TouchPhase::Moved, now);
    assert_eq!(ongoing_scroll.axis, Some(Axis::Horizontal));
    assert_eq!(horizontal_delta, point(px(10.), px(0.)));
}

#[test]
fn small_vertical_start_does_not_swallow_horizontal_trackpad_movement() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    let mut delta = point(px(-0.2), px(-0.6));
    ongoing_scroll.filter_at(&mut delta, TouchPhase::Started, now);
    let mut horizontal = delta.x;
    for step in 1..=10 {
        let mut delta = point(px(-2.5), px(-0.1));
        ongoing_scroll.filter_at(
            &mut delta,
            TouchPhase::Moved,
            now + Duration::from_millis(step),
        );
        horizontal += delta.x;
    }
    assert_eq!(horizontal, px(-25.2));
    assert_eq!(ongoing_scroll.axis, Some(Axis::Horizontal));
}

#[test]
fn small_direction_changes_accumulate_to_release_the_lock() {
    for axis in [Axis::Horizontal, Axis::Vertical] {
        let now = Instant::now();
        let mut ongoing_scroll = OngoingScroll::default();
        let orient = |along, across| match axis {
            Axis::Horizontal => point(px(along), px(across)),
            Axis::Vertical => point(px(across), px(along)),
        };
        ongoing_scroll.filter_at(&mut orient(10., 2.), TouchPhase::Started, now);
        for step in 1..=3 {
            let mut delta = orient(0.1, 2.5);
            ongoing_scroll.filter_at(
                &mut delta,
                TouchPhase::Moved,
                now + Duration::from_millis(step),
            );
            if step == 3 {
                assert_eq!(delta, orient(0.1, 2.5));
                assert_eq!(ongoing_scroll.axis, None);
            }
        }
    }
}

#[test]
fn sideways_noise_does_not_accumulate_across_on_axis_movements() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    ongoing_scroll.filter_at(&mut point(px(2.), px(10.)), TouchPhase::Started, now);
    for step in 1..=20 {
        let mut delta = if step % 2 == 0 {
            point(px(0.1), px(2.5))
        } else {
            point(px(2.5), px(0.1))
        };
        ongoing_scroll.filter_at(
            &mut delta,
            TouchPhase::Moved,
            now + Duration::from_millis(step),
        );
        assert_eq!(delta.x, px(0.));
        assert_eq!(ongoing_scroll.axis, Some(Axis::Vertical));
    }
}

#[test]
fn gesture_boundaries_clear_accumulated_direction() {
    for phase in [
        TouchPhase::Ended,
        TouchPhase::Cancelled,
        TouchPhase::Started,
    ] {
        let now = Instant::now();
        let mut ongoing_scroll = OngoingScroll::default();
        ongoing_scroll.filter_at(&mut point(px(4.), px(0.)), TouchPhase::Started, now);
        ongoing_scroll.filter_at(&mut Point::default(), phase, now + Duration::from_millis(1));
        let mut delta = point(px(0.1), px(4.));
        ongoing_scroll.filter_at(
            &mut delta,
            TouchPhase::Moved,
            now + Duration::from_millis(2),
        );
        assert_eq!(delta, point(px(0.1), px(4.)));
        assert_eq!(ongoing_scroll.axis, None);
    }
}

#[test]
fn moved_only_timeout_clears_accumulated_direction() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    ongoing_scroll.filter_at(&mut point(px(4.), px(0.)), TouchPhase::Moved, now);
    let mut delta = point(px(0.1), px(4.));
    ongoing_scroll.filter_at(&mut delta, TouchPhase::Moved, now + SCROLL_EVENT_SEPARATION);
    assert_eq!(delta, point(px(0.1), px(4.)));
    assert_eq!(ongoing_scroll.axis, None);
}

#[test]
fn releasing_a_lock_keeps_the_rest_of_the_gesture_free() {
    let now = Instant::now();
    let mut ongoing_scroll = OngoingScroll::default();
    ongoing_scroll.filter_at(&mut point(px(2.), px(10.)), TouchPhase::Started, now);
    ongoing_scroll.filter_at(
        &mut point(px(10.), px(2.)),
        TouchPhase::Moved,
        now + Duration::from_millis(1),
    );
    for step in 2..=20 {
        let mut delta = point(px(2.), px(1.));
        ongoing_scroll.filter_at(
            &mut delta,
            TouchPhase::Moved,
            now + Duration::from_millis(step),
        );
        assert_eq!(delta, point(px(2.), px(1.)));
    }
}
