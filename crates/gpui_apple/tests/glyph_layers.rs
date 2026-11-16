#![cfg(all(target_os = "macos", feature = "test-support", feature = "font-kit"))]

use anyhow::{Context as _, Result};
use gpui::{
    self, Bounds, ContentMask, MonochromeSprite, PlatformAtlas, PlatformTextSystem, Quad,
    RenderGlyphParams, ScaledPixels, Scene, SpriteAnimationId, TransformationMatrix,
    native_material::{MaterialStyle, NativeMaterial},
};
use gpui_apple::{AppleTextSystem, metal_renderer::MetalRenderer};
use std::{borrow::Cow, ops::Bound};

#[test]
fn selected_and_inactive_tab_glyphs_remain_visible_in_composition_layers() -> Result<()> {
    let fonts = AppleTextSystem::new();
    let font_id = fonts.font_id(&gpui::font("Arial"))?;
    let glyph_id = fonts
        .glyph_for_char(font_id, 'A')
        .context("missing A glyph")?;
    let params = RenderGlyphParams {
        font_id,
        glyph_id,
        font_size: gpui::px(32.),
        subpixel_variant: gpui::point(0, 0),
        scale_factor: 1.,
        is_emoji: false,
        subpixel_rendering: false,
        dilation: 0,
    };
    let raster_bounds = fonts.glyph_raster_bounds(&params)?;
    let (size, bytes) = fonts.rasterize_glyph(&params, raster_bounds)?;
    assert!(bytes.iter().any(|value| *value > 0));
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let tile = renderer
        .sprite_atlas()
        .get_or_insert_with(params.into(), &mut || {
            Ok(Some((size, Cow::Borrowed(&bytes))))
        })?
        .context("missing glyph tile")?;
    let mut scene = Scene::default();
    let bounds = Bounds::new(
        gpui::point(ScaledPixels(44.), ScaledPixels(4.)),
        size.map(|value| ScaledPixels(value.0 as f32)),
    );
    scene.insert_primitive(Quad {
        bounds: Bounds::new(
            gpui::point(ScaledPixels(120.), ScaledPixels(48.)),
            gpui::size(ScaledPixels(8.), ScaledPixels(8.)),
        ),
        content_mask: ContentMask::new(Bounds::new(
            gpui::point(ScaledPixels(0.), ScaledPixels(0.)),
            gpui::size(ScaledPixels(128.), ScaledPixels(64.)),
        )),
        background: gpui::rgb(0x222222).into(),
        ..Default::default()
    });
    let mask = ContentMask::new(Bounds::new(
        gpui::point(ScaledPixels(0.), ScaledPixels(0.)),
        gpui::size(ScaledPixels(128.), ScaledPixels(64.)),
    ));
    scene.push_layer();
    scene.begin_native_material(NativeMaterial {
        id: Default::default(),
        state: Default::default(),
        style: MaterialStyle::Popover {
            corner_radius: gpui::px(8.),
        },
        bounds,
        content_mask: mask,
        opacity: 1.,
        order: 0,
        end_order: 0,
        parent: None,
    });
    scene.insert_primitive(Quad {
        bounds: mask.bounds,
        content_mask: mask,
        background: gpui::rgb(0x111111).into(),
        ..Default::default()
    });
    for x in [4., 44., 84.] {
        scene.push_layer();
        scene.insert_primitive(MonochromeSprite {
            order: 0,
            animation: SpriteAnimationId::NONE,
            bounds: Bounds::new(gpui::point(ScaledPixels(x), bounds.origin.y), bounds.size),
            content_mask: mask,
            color: gpui::rgb(0xffffff).into(),
            tile,
            transformation: TransformationMatrix::unit(),
        });
        scene.pop_layer();
    }
    scene.end_native_material();
    scene.pop_layer();
    scene.finish();
    assert_eq!(scene.monochrome_sprites[0].order, scene.quads[1].order);
    let material = &scene.native_materials[0];
    for orders in [
        (
            Bound::Included(material.order + 1),
            Bound::Excluded(material.end_order),
        ),
        (
            Bound::Excluded(material.order),
            Bound::Excluded(material.end_order),
        ),
        (Bound::Unbounded, Bound::Unbounded),
    ] {
        let image = renderer.render_scene_layer_to_image(
            &scene,
            gpui::size(128.into(), 64.into()),
            orders,
            true,
        )?;
        for left in [4, 44, 84] {
            assert!(
                (left..left + size.width.0 as u32).any(|x| {
                    (4..4 + size.height.0 as u32).any(|y| image.get_pixel(x, y).0[0] > 128)
                }),
                "tab label at x={left} disappeared"
            );
        }
    }
    Ok(())
}
