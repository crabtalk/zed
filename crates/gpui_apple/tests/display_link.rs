#![cfg(target_os = "macos")]

use anyhow::{Context as _, Result};
use gpui::{self, Scene};
use gpui_apple::metal_renderer::MetalRenderer;
use objc2::Message;

#[test]
fn resynchronizing_an_attached_display_link_preserves_its_frame() -> Result<()> {
    let mut renderer = MetalRenderer::new(Default::default(), false, false)?;
    renderer.update_drawable_size(gpui::size(64.into(), 64.into()));
    let layer = renderer.layer().context("missing window layer")?.retain();
    let drawable = layer.nextDrawable().context("missing window drawable")?;
    renderer.set_display_link_attached(true);
    renderer.set_frame_drawable(Some(drawable.clone()));
    renderer.set_display_link_attached(true);
    assert!(renderer.set_frame_drawable(None).is_some());

    renderer.set_frame_drawable(Some(drawable.clone()));
    renderer.set_display_link_attached(true);
    renderer.draw_layer(&Scene::default(), &layer, .., false)?;
    assert!(renderer.set_frame_drawable(None).is_none());

    renderer.set_frame_drawable(Some(drawable));
    renderer.set_display_link_attached(false);
    assert!(!renderer.is_display_link_attached());
    renderer.set_display_link_attached(true);
    assert!(renderer.set_frame_drawable(None).is_none());
    Ok(())
}
