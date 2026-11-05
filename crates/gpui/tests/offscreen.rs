#![cfg(feature = "test-support")]

use gpui::{Context, IntoElement, Render, TestAppContext, Window, div, px, size};

struct Empty;
impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[gpui::test]
fn invalid_output_dimensions_open_no_window(cx: &mut TestAppContext) {
    for (width, height, scale) in [
        (0., 10., 1.),
        (10., -1., 1.),
        (10., 10., f32::NAN),
        (10000., 10., 2.),
    ] {
        let result = cx
            .update(|cx| cx.render_view_to_image(size(px(width), px(height)), scale, |_, _| Empty));
        assert!(result.is_err());
        assert!(cx.windows().is_empty());
    }
}

#[gpui::test]
async fn an_unsupported_platform_reports_an_error_and_closes_its_hidden_window(
    cx: &mut TestAppContext,
) {
    let task = cx.update(|cx| {
        cx.render_view_to_image(size(px(100.), px(80.)), 2., |_, _| Empty)
            .unwrap()
    });
    assert_eq!(cx.windows().len(), 1);
    assert!(task.await.unwrap_err().to_string().contains("unavailable"));
    cx.run_until_parked();
    assert!(cx.windows().is_empty());
}

#[gpui::test]
fn cancelling_an_unpolled_render_closes_its_hidden_window(cx: &mut TestAppContext) {
    let task = cx.update(|cx| {
        cx.render_view_to_image(size(px(100.), px(80.)), 1., |_, _| Empty)
            .unwrap()
    });
    drop(task);
    cx.run_until_parked();
    assert!(cx.windows().is_empty());
}

struct BrokenImage;
impl Render for BrokenImage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::img(std::path::PathBuf::from("/cydonia-no-such-image.png"))
    }
}

#[gpui::test]
async fn image_loading_failures_are_reported_before_readback(cx: &mut TestAppContext) {
    let task = cx.update(|cx| {
        cx.render_view_to_image(size(px(100.), px(80.)), 1., |_, _| BrokenImage)
            .unwrap()
    });
    assert!(
        task.await
            .unwrap_err()
            .to_string()
            .contains("image failed to load")
    );
    cx.run_until_parked();
    assert!(cx.windows().is_empty());
}
