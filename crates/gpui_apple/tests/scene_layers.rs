#![cfg(all(target_os = "macos", feature = "test-support"))]

use anyhow::Result;
use gpui::{
    self, AtlasKey, BackdropBlur, Bounds, ContentMask, DrawOrder, Hsla, ImageId, PaintedPath,
    PathBuilder, PlatformAtlas, PolychromeSprite, Quad, RenderImageParams, ScaledPixels, Scene,
    native_material::{MaterialState, MaterialStyle, NativeMaterial},
};
use gpui_apple::metal_renderer::MetalRenderer;
use std::borrow::Cow;

fn quad(order: DrawOrder, x: f32, color: Hsla) -> Quad {
    Quad {
        order,
        bounds: Bounds::new(
            gpui::point(ScaledPixels(x), ScaledPixels(0.)),
            gpui::size(ScaledPixels(8.), ScaledPixels(8.)),
        ),
        content_mask: ContentMask::new(Bounds::new(
            gpui::point(ScaledPixels(0.), ScaledPixels(0.)),
            gpui::size(ScaledPixels(16.), ScaledPixels(8.)),
        )),
        background: color.into(),
        ..Quad::default()
    }
}

#[test]
fn transparent_layers_preserve_original_instance_offsets() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let mut scene = Scene::default();
    scene.quads.extend([
        quad(0, 0., gpui::rgb(0xff0000).into()),
        quad(1, 8., gpui::rgb(0x00ff00).into()),
    ]);
    scene.finish();
    let output_size = gpui::size(16.into(), 8.into());
    let lower = renderer.render_scene_layer_to_image(&scene, output_size, 0..1, true)?;
    let upper = renderer.render_scene_layer_to_image(&scene, output_size, 1..2, true)?;
    let full = renderer.render_scene_to_image(&scene, output_size)?;
    assert_eq!(lower.get_pixel(4, 4).0, [255, 0, 0, 255]);
    assert_eq!(lower.get_pixel(12, 4).0, [0, 0, 0, 0]);
    assert_eq!(upper.get_pixel(4, 4).0, [0, 0, 0, 0]);
    assert_eq!(upper.get_pixel(12, 4).0, [0, 255, 0, 255]);
    assert_eq!(full.get_pixel(4, 4).0, lower.get_pixel(4, 4).0);
    assert_eq!(full.get_pixel(12, 4).0, upper.get_pixel(12, 4).0);
    Ok(())
}

#[test]
fn empty_layers_clear_previous_output_and_resize() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let mut scene = Scene::default();
    scene.quads.push(quad(5, 0., gpui::rgb(0xffffff).into()));
    scene.finish();
    renderer.render_scene_layer_to_image(&scene, gpui::size(8.into(), 8.into()), 5..6, true)?;
    let empty =
        renderer.render_scene_layer_to_image(&scene, gpui::size(3.into(), 2.into()), 0..5, true)?;
    assert_eq!(empty.dimensions(), (3, 2));
    assert!(empty.pixels().all(|pixel| pixel.0 == [0, 0, 0, 0]));
    let opaque = renderer.render_scene_layer_to_image(
        &scene,
        gpui::size(3.into(), 2.into()),
        0..5,
        false,
    )?;
    assert!(opaque.pixels().all(|pixel| pixel.0 == [0, 0, 0, 255]));
    Ok(())
}

#[test]
fn path_layers_keep_original_tile_offsets() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let mut scene = Scene::default();
    for (order, x, color) in [(0, 0., 0xff0000), (1, 8., 0x00ff00)] {
        let mut builder = PathBuilder::fill();
        builder.move_to(gpui::point(gpui::px(x), gpui::px(0.)));
        builder.line_to(gpui::point(gpui::px(x + 8.), gpui::px(0.)));
        builder.line_to(gpui::point(gpui::px(x + 8.), gpui::px(8.)));
        builder.line_to(gpui::point(gpui::px(x), gpui::px(8.)));
        builder.close();
        let mask = ContentMask::new(Bounds::new(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(16.), gpui::px(8.)),
        ));
        if order == 1 {
            scene.begin_native_material(NativeMaterial {
                id: Default::default(),
                state: MaterialState::default(),
                style: MaterialStyle::Group {
                    spacing: gpui::px(0.),
                },
                bounds: mask.bounds.scale(1.),
                content_mask: mask.scale(1.),
                opacity: 1.,
                order: 0,
                end_order: 0,
                parent: None,
            });
        }
        scene.insert_primitive(PaintedPath::new(
            builder.build()?,
            1.,
            mask,
            gpui::rgb(color).into(),
        ));
        if order == 1 {
            scene.end_native_material();
        }
    }
    scene.finish();
    let upper = renderer.render_scene_layer_to_image(
        &scene,
        gpui::size(16.into(), 8.into()),
        scene.native_materials[0].order..scene.native_materials[0].end_order,
        true,
    )?;
    assert_eq!(upper.get_pixel(4, 4).0, [0, 0, 0, 0]);
    assert_eq!(upper.get_pixel(12, 4).0, [0, 255, 0, 255]);
    Ok(())
}

#[test]
fn backdrop_operations_respect_layer_boundaries_and_trailing_order() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let mut scene = Scene::default();
    scene.quads.extend([
        quad(0, 0., gpui::rgb(0xff0000).into()),
        quad(2, 8., gpui::rgb(0x00ff00).into()),
    ]);
    let bounds = Bounds::new(
        gpui::point(ScaledPixels(0.), ScaledPixels(0.)),
        gpui::size(ScaledPixels(16.), ScaledPixels(8.)),
    );
    scene.backdrop_blurs.push(BackdropBlur {
        order: 1,
        bounds,
        content_mask: ContentMask::new(bounds),
        blur_radius: ScaledPixels(0.),
        corner_radii: Default::default(),
        lens: ScaledPixels(0.),
        reach: ScaledPixels(0.),
        magnify: 0.,
        dispersion: 0.,
        gain: 0.,
        saturation: 1.,
        tint: gpui::rgb(0x0000ff).into(),
        edge: 0.,
        edge_width: ScaledPixels(0.),
        edge_aa: ScaledPixels(0.),
        opacity: 1.,
    });
    scene.finish();
    let before = renderer.render_scene_layer_to_image(
        &scene,
        gpui::size(16.into(), 8.into()),
        0..1,
        true,
    )?;
    let through_blur = renderer.render_scene_layer_to_image(
        &scene,
        gpui::size(16.into(), 8.into()),
        0..2,
        true,
    )?;
    let full = renderer.render_scene_to_image(&scene, gpui::size(16.into(), 8.into()))?;
    assert_eq!(before.get_pixel(4, 4).0, [255, 0, 0, 255]);
    assert_eq!(through_blur.get_pixel(4, 4).0, [0, 0, 255, 255]);
    assert_eq!(full.get_pixel(4, 4).0, [0, 0, 255, 255]);
    assert_eq!(full.get_pixel(12, 4).0, [0, 255, 0, 255]);
    Ok(())
}

#[test]
fn sprite_layers_share_the_atlas_and_keep_original_indices() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let tile = renderer
        .sprite_atlas()
        .get_or_insert_with(
            AtlasKey::Image(RenderImageParams {
                image_id: ImageId(1),
                frame_index: 0,
            }),
            &mut || {
                Ok(Some((
                    gpui::size(8.into(), 8.into()),
                    Cow::Owned([255, 0, 0, 255].repeat(64)),
                )))
            },
        )?
        .ok_or_else(|| anyhow::anyhow!("atlas tile was not created"))?;
    let mut scene = Scene::default();
    for (order, x) in [(0, 0.), (1, 8.)] {
        let bounds = Bounds::new(
            gpui::point(ScaledPixels(x), ScaledPixels(0.)),
            gpui::size(ScaledPixels(8.), ScaledPixels(8.)),
        );
        scene.polychrome_sprites.push(PolychromeSprite {
            order,
            pad: 0,
            grayscale: false.into(),
            opacity: 1.,
            bounds,
            content_mask: ContentMask::new(bounds),
            corner_radii: Default::default(),
            tile,
        });
    }
    scene.finish();
    let lower = renderer.render_scene_layer_to_image(
        &scene,
        gpui::size(16.into(), 8.into()),
        0..1,
        true,
    )?;
    let upper = renderer.render_scene_layer_to_image(
        &scene,
        gpui::size(16.into(), 8.into()),
        1..2,
        true,
    )?;
    assert_eq!(lower.get_pixel(4, 4).0, [0, 0, 255, 255]);
    assert_eq!(lower.get_pixel(12, 4).0, [0, 0, 0, 0]);
    assert_eq!(upper.get_pixel(4, 4).0, [0, 0, 0, 0]);
    assert_eq!(upper.get_pixel(12, 4).0, [0, 0, 255, 255]);
    Ok(())
}

#[test]
fn full_scene_and_inclusive_ranges_keep_the_last_draw_order() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let mut scene = Scene::default();
    scene
        .quads
        .push(quad(DrawOrder::MAX, 0., gpui::rgb(0xff0000).into()));
    scene.finish();
    let all = renderer.render_scene_to_image(&scene, gpui::size(8.into(), 8.into()))?;
    let last = renderer.render_scene_layer_to_image(
        &scene,
        gpui::size(8.into(), 8.into()),
        DrawOrder::MAX..=DrawOrder::MAX,
        true,
    )?;
    assert_eq!(all.get_pixel(4, 4).0, [255, 0, 0, 255]);
    assert_eq!(last.get_pixel(4, 4).0, [255, 0, 0, 255]);
    Ok(())
}

#[test]
fn native_materials_require_separate_export_but_allow_foreground_layer_readback() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let foreground = quad(0, 0., gpui::rgb(0xff0000).into());
    let mut scene = Scene::default();
    scene.begin_native_material(NativeMaterial {
        id: Default::default(),
        state: MaterialState::default(),
        style: MaterialStyle::Glass {
            clear: false,
            tint: None,
            corner_radius: gpui::px(2.),
            interactive: false,
        },
        bounds: foreground.bounds,
        content_mask: foreground.content_mask,
        opacity: 1.,
        order: 0,
        end_order: 0,
        parent: None,
    });
    scene.insert_primitive(foreground);
    scene.end_native_material();
    scene.finish();
    let size = gpui::size(16.into(), 8.into());
    assert!(
        renderer
            .render_scene_to_image(&scene, size)
            .unwrap_err()
            .to_string()
            .contains("pixel acquisition")
    );
    let scope = &scene.native_materials[0];
    let foreground =
        renderer.render_scene_layer_to_image(&scene, size, scope.order..scope.end_order, true)?;
    assert_eq!(foreground.get_pixel(4, 4).0, [255, 0, 0, 255]);
    assert_eq!(foreground.get_pixel(12, 4).0, [0, 0, 0, 0]);
    Ok(())
}

#[test]
fn foreground_batches_stay_above_opaque_quads_at_the_same_order() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let tile = renderer
        .sprite_atlas()
        .get_or_insert_with(
            AtlasKey::Image(RenderImageParams {
                image_id: ImageId(2),
                frame_index: 0,
            }),
            &mut || {
                Ok(Some((
                    gpui::size(8.into(), 8.into()),
                    Cow::Owned([0, 255, 0, 255].repeat(64)),
                )))
            },
        )?
        .ok_or_else(|| anyhow::anyhow!("atlas tile was not created"))?;
    for kind in ["path", "underline", "image"] {
        let mut scene = Scene::default();
        let background = quad(5, 0., gpui::rgb(0xff0000).into());
        scene.insert_primitive(quad(0, 8., gpui::rgb(0x0000ff).into()));
        scene.insert_primitive(background);
        match kind {
            "path" => {
                let mut path = PathBuilder::fill();
                path.move_to(gpui::point(gpui::px(0.), gpui::px(0.)));
                path.line_to(gpui::point(gpui::px(8.), gpui::px(0.)));
                path.line_to(gpui::point(gpui::px(8.), gpui::px(8.)));
                path.line_to(gpui::point(gpui::px(0.), gpui::px(8.)));
                path.close();
                let path = PaintedPath::new(
                    path.build()?,
                    1.,
                    ContentMask::new(background.bounds.map(|value| gpui::px(value.0))),
                    gpui::rgb(0x00ff00).into(),
                );
                scene.insert_primitive(path);
            }
            "underline" => scene.insert_primitive(gpui::Underline {
                order: 5,
                pad: 0,
                bounds: background.bounds,
                content_mask: background.content_mask,
                color: gpui::rgb(0x00ff00).into(),
                thickness: ScaledPixels(8.),
                wavy: false.into(),
            }),
            "image" => scene.insert_primitive(PolychromeSprite {
                order: 5,
                pad: 0,
                grayscale: false.into(),
                opacity: 1.,
                bounds: background.bounds,
                content_mask: background.content_mask,
                corner_radii: Default::default(),
                tile,
            }),
            _ => unreachable!(),
        }
        scene.finish();
        for orders in [
            (std::ops::Bound::Unbounded, std::ops::Bound::Unbounded),
            (std::ops::Bound::Included(1), std::ops::Bound::Included(1)),
        ] {
            let output = renderer.render_scene_layer_to_image(
                &scene,
                gpui::size(16.into(), 8.into()),
                orders,
                true,
            )?;
            assert_eq!(
                output.get_pixel(4, 4).0,
                [0, 255, 0, 255],
                "{kind} disappeared"
            );
        }
    }
    Ok(())
}

#[test]
fn transparent_foreground_layers_preserve_background_and_alpha() -> Result<()> {
    let mut renderer = MetalRenderer::new_headless(Default::default())?;
    let mut scene = Scene::default();
    scene.quads.extend([
        quad(0, 0., gpui::rgb(0xff0000).into()),
        quad(1, 0., gpui::rgba(0x0000ff80).into()),
    ]);
    scene.finish();
    let size = gpui::size(8.into(), 8.into());
    let foreground = renderer.render_scene_layer_to_image(&scene, size, 1..2, true)?;
    let full = renderer.render_scene_to_image(&scene, size)?;
    assert_eq!(foreground.get_pixel(4, 4).0, [0, 0, 128, 128]);
    assert_eq!(full.get_pixel(4, 4).0, [127, 0, 128, 255]);
    Ok(())
}
