use gpui::{
    self, Bounds, ContentMask, GlobalElementId, Quad, ScaledPixels, Scene, TestAppContext,
    WindowAppearance,
    native_material::{MaterialState, MaterialStyle, NativeMaterial},
};

fn material(id: GlobalElementId, style: MaterialStyle) -> NativeMaterial {
    let bounds = Bounds::new(
        gpui::point(ScaledPixels(0.), ScaledPixels(0.)),
        gpui::size(ScaledPixels(100.), ScaledPixels(80.)),
    );
    NativeMaterial {
        id,
        state: MaterialState::default(),
        style,
        bounds,
        content_mask: ContentMask::new(bounds),
        opacity: 1.,
        order: 0,
        end_order: 0,
        parent: None,
    }
}

#[gpui::test]
fn nested_scopes_partition_foreground_and_replay_with_new_orders(cx: &mut TestAppContext) {
    cx.add_empty_window().update(|window, _| {
        let group = window.with_global_id("group".into(), |id, _| {
            material(
                id.clone(),
                MaterialStyle::Group {
                    spacing: gpui::px(0.),
                },
            )
        });
        let glass = window.with_global_id("glass".into(), |id, _| {
            material(
                id.clone(),
                MaterialStyle::Glass {
                    clear: false,
                    tint: None,
                    corner_radius: gpui::px(12.),
                    interactive: false,
                },
            )
        });
        let quad = Quad {
            bounds: glass.bounds,
            content_mask: glass.content_mask,
            ..Quad::default()
        };
        let mut previous = Scene::default();
        previous.insert_primitive(quad);
        previous.push_layer(glass.bounds);
        previous.begin_native_material(group);
        previous.insert_primitive(quad);
        previous.begin_native_material(glass);
        previous.insert_primitive(quad);
        previous.end_native_material();
        previous.insert_primitive(quad);
        previous.end_native_material();
        previous.pop_layer();
        previous.insert_primitive(quad);
        previous.finish();
        let mut replay = Scene::default();
        replay.insert_primitive(quad);
        replay.replay(0..previous.len(), &previous);
        replay.finish();
        let group = &replay.native_materials[0];
        let glass = &replay.native_materials[1];
        assert_eq!(group.id, previous.native_materials[0].id);
        assert_eq!(glass.id, previous.native_materials[1].id);
        assert_eq!(group.parent, None);
        assert_eq!(glass.parent, Some(0));
        assert!(group.order < glass.order && glass.end_order < group.end_order);
        let orders: Vec<_> = replay.quads.iter().map(|quad| quad.order).collect();
        assert!(orders[1] < group.order);
        assert!(group.order < orders[2] && orders[2] < glass.order);
        assert!(glass.order < orders[3] && orders[3] < glass.end_order);
        assert!(glass.end_order < orders[4] && orders[4] < group.end_order);
        assert!(group.end_order < orders[5]);
        assert_ne!(group.order, previous.native_materials[0].order);
        replay.clear();
        replay.insert_primitive(quad);
        replay.begin_native_material(previous.native_materials[1].clone());
        replay.end_native_material();
        assert_eq!(replay.native_materials[0].parent, None);
    });
}

#[test]
fn content_appearance_is_shared_by_clones_and_isolated_between_surfaces() {
    let first = MaterialState::default();
    let replay = first.clone();
    let second = MaterialState::default();
    assert!(first.set_appearance(WindowAppearance::Dark));
    assert_eq!(replay.appearance(), Some(WindowAppearance::Dark));
    assert_eq!(second.appearance(), None);
    assert!(!replay.set_appearance(WindowAppearance::Dark));
    assert!(replay.set_appearance(WindowAppearance::Light));
    assert_eq!(first.appearance(), Some(WindowAppearance::Light));
    assert_eq!(second.appearance(), None);
}
