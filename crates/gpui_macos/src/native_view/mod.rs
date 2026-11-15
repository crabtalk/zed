use crate::{
    native_view::{
        clip::CompositionView,
        material::MaterialHost,
        view::{FocusChange, HostedView},
    },
    renderer,
};
use anyhow::{Context as _, Result, ensure};
use gpui::{self, Bounds, DrawOrder, Pixels, Point, Scene};
use objc2::{DeclaredClass, MainThreadMarker, Message, rc::Retained};
use objc2_app_kit::NSView;
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CAMetalLayer, CATransaction};
use raw_window_handle::RawWindowHandle;
use std::{ptr, rc::Rc};

mod clip;
mod material;
mod view;

struct Overlay {
    view: Retained<CompositionView>,
    layer: Retained<CAMetalLayer>,
}

pub(crate) struct Compositor {
    root: Retained<CompositionView>,
    views: Vec<HostedView>,
    materials: Vec<MaterialHost>,
    overlays: Vec<Overlay>,
    changed: Rc<dyn Fn()>,
    pub(crate) retired: Vec<HostedView>,
    pub(crate) focus_changes: Vec<FocusChange>,
    pub(crate) presented: Vec<Rc<dyn Fn()>>,
}

#[derive(Clone, Copy)]
enum Node {
    View(usize),
    Material(usize),
}

struct Frame<'a> {
    parent: &'a NSView,
    renderer: &'a mut renderer::Renderer,
    scene: &'a Scene,
    scale: f32,
    mtm: MainThreadMarker,
    base: &'a CAMetalLayer,
    views: Vec<usize>,
    materials: Vec<usize>,
    overlays: usize,
}

impl Compositor {
    pub(crate) fn new(parent: &NSView, changed: Rc<dyn Fn()>, mtm: MainThreadMarker) -> Self {
        let root = CompositionView::new(mtm);
        root.ivars().borrow_mut().interactive = true;
        parent.addSubview(&root);
        Self {
            root,
            views: Vec::new(),
            materials: Vec::new(),
            overlays: Vec::new(),
            changed,
            retired: Vec::new(),
            focus_changes: Vec::new(),
            presented: Vec::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        // SAFETY: the compositor owns this main-thread hierarchy.
        if let Some(parent) = unsafe { self.root.superview() } {
            self.focus_changes
                .extend(
                    self.views
                        .iter()
                        .filter(|view| view.owns_native())
                        .map(|view| FocusChange::Release {
                            native: view.native.clone(),
                            parent: parent.clone(),
                        }),
                );
        }
        self.root.setHidden(true);
        self.root.setSubviews(&NSArray::new());
        self.retired.append(&mut self.views);
        self.materials.clear();
        self.overlays.clear();
    }

    pub(crate) fn draw(
        &mut self,
        parent: &NSView,
        renderer: &mut renderer::Renderer,
        scene: &Scene,
        scale: f32,
    ) -> Result<()> {
        ensure!(
            scale.is_finite() && scale > 0.,
            "invalid native composition scale"
        );
        ensure!(
            scene.backdrop_blurs.is_empty(),
            "backdrop sampling across native views is unavailable"
        );
        let mut handles = Vec::with_capacity(scene.native_views.len());
        for source in &scene.native_views {
            let RawWindowHandle::AppKit(handle) = source
                .view
                .window_handle()
                .map_err(|error| anyhow::anyhow!("native view handle: {error:?}"))?
                .as_raw()
            else {
                anyhow::bail!("native composition requires an AppKit view handle");
            };
            ensure!(
                handle.ns_view.as_ptr() != ptr::from_ref(parent).cast_mut().cast(),
                "the GPUI window view cannot be embedded in itself"
            );
            ensure!(
                !handles.contains(&handle.ns_view),
                "a native view cannot appear twice in one scene"
            );
            // SAFETY: the handle owner retains this NSView for this scene.
            let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
            ensure!(
                !parent.isDescendantOf(view),
                "an ancestor of the GPUI view cannot be embedded in it"
            );
            handles.push(handle.ns_view);
        }
        for (index, source) in scene.native_materials.iter().enumerate() {
            ensure!(
                !scene.native_materials[..index]
                    .iter()
                    .any(|other| other.id == source.id),
                "a native material id cannot appear twice in one scene"
            );
            ensure!(
                source.order < source.end_order,
                "unclosed native material scope"
            );
        }
        let mtm =
            MainThreadMarker::new().context("native composition must run on the main thread")?;
        let base = renderer
            .layer()
            .context("native composition requires a window renderer")?
            .retain();
        let previous_transaction = base.presentsWithTransaction();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        renderer.set_presents_with_transaction(true);
        self.root.setFrame(parent.bounds());
        self.root.setHidden(false);
        let mut frame = Frame {
            parent,
            renderer,
            scene,
            scale,
            mtm,
            base: &base,
            views: Vec::new(),
            materials: Vec::new(),
            overlays: 0,
        };
        let root = self.root.clone();
        let result = self.draw_scope(None, &root, Point::default(), &mut frame);
        if result.is_ok() {
            self.presented.extend(
                scene
                    .native_views
                    .iter()
                    .filter_map(|view| view.presented.clone()),
            );
            self.overlays.truncate(frame.overlays);
            for index in (0..self.views.len()).rev() {
                if !frame.views.contains(&index) {
                    if self.views[index].owns_native() {
                        self.focus_changes.push(FocusChange::Release {
                            native: self.views[index].native.clone(),
                            parent: parent.retain(),
                        });
                    }
                    self.retired.push(self.views.remove(index));
                }
            }
            for index in (0..self.materials.len()).rev() {
                if !frame.materials.contains(&index) {
                    self.materials.remove(index);
                }
            }
        }
        CATransaction::commit();
        frame
            .renderer
            .set_presents_with_transaction(previous_transaction);
        result
    }

    fn draw_scope(
        &mut self,
        scope: Option<usize>,
        target: &NSView,
        origin: Point<Pixels>,
        frame: &mut Frame<'_>,
    ) -> Result<()> {
        let scene = frame.scene;
        let parent = frame.parent;
        let scale = frame.scale;
        let mtm = frame.mtm;
        let mut start = scope.map_or(0, |index| scene.native_materials[index].order + 1);
        let end = scope.map(|index| scene.native_materials[index].end_order);
        let mut nodes: Vec<_> = scene
            .native_views
            .iter()
            .enumerate()
            .filter(|(_, view)| view.parent == scope)
            .map(|(index, view)| (view.order, Node::View(index)))
            .chain(
                scene
                    .native_materials
                    .iter()
                    .enumerate()
                    .filter(|(_, material)| material.parent == scope)
                    .map(|(index, material)| (material.order, Node::Material(index))),
            )
            .collect();
        nodes.sort_by_key(|(order, _)| *order);
        let mut arranged = Vec::new();
        let mut base_pending = scope.is_none();
        for (order, node) in nodes {
            self.draw_gpu(
                start,
                Some(order),
                &mut base_pending,
                origin,
                frame,
                &mut arranged,
            )?;
            match node {
                Node::View(index) => {
                    let source = &scene.native_views[index];
                    let slot = match self
                        .views
                        .iter()
                        .position(|view| Rc::ptr_eq(&view.source.view, &source.view))
                    {
                        Some(index) => index,
                        None => {
                            self.views.push(HostedView::new(source, mtm)?);
                            self.views.len() - 1
                        }
                    };
                    frame.views.push(slot);
                    let hosted = &mut self.views[slot];
                    hosted.update(source, scale, origin)?;
                    if let Some(change) = hosted.focus_change(parent) {
                        self.focus_changes.push(change);
                    }
                    arranged.push(Retained::into_super(hosted.clip.clone()));
                    start = order + 1;
                }
                Node::Material(index) => {
                    let source = &scene.native_materials[index];
                    let slot = match self.materials.iter().position(|material| {
                        material.source.id == source.id
                            && std::mem::discriminant(&material.source.style)
                                == std::mem::discriminant(&source.style)
                    }) {
                        Some(index) => index,
                        None => {
                            self.materials.push(MaterialHost::new(
                                source,
                                self.changed.clone(),
                                mtm,
                            ));
                            self.materials.len() - 1
                        }
                    };
                    frame.materials.push(slot);
                    let hosted = &mut self.materials[slot];
                    hosted.update(source, scale, origin)?;
                    let content = hosted.content.clone();
                    let clip = hosted.clip.clone();
                    let child_origin = source.bounds.origin.map(|value| gpui::px(value.0 / scale));
                    self.draw_scope(Some(index), &content, child_origin, frame)?;
                    arranged.push(Retained::into_super(clip));
                    start = source.end_order + 1;
                }
            }
        }
        self.draw_gpu(start, end, &mut base_pending, origin, frame, &mut arranged)?;
        target.setSubviews(&NSArray::from_slice(
            &arranged.iter().map(|view| &**view).collect::<Vec<_>>(),
        ));
        Ok(())
    }

    fn draw_gpu(
        &mut self,
        start: DrawOrder,
        end: Option<DrawOrder>,
        base_pending: &mut bool,
        origin: Point<Pixels>,
        frame: &mut Frame<'_>,
        arranged: &mut Vec<Retained<NSView>>,
    ) -> Result<()> {
        let scene = frame.scene;
        let parent = frame.parent;
        let scale = frame.scale;
        let mtm = frame.mtm;
        let base = frame.base;
        let renderer = &mut *frame.renderer;
        let end = end.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        let orders = (std::ops::Bound::Included(start), end);
        if std::mem::take(base_pending) {
            return renderer.draw_layer(scene, base, orders, !base.isOpaque());
        }
        if scene.batches_in(orders).next().is_none() {
            return Ok(());
        }
        let index = frame.overlays;
        frame.overlays += 1;
        if self.overlays.len() <= index {
            let layer = renderer.create_layer();
            let view = CompositionView::new(mtm);
            // SAFETY: metal-rs and objc2 wrap the same CALayer object.
            view.setLayer(Some(&layer));
            self.overlays.push(Overlay { view, layer });
        }
        let overlay = &self.overlays[index];
        let size = parent.bounds().size;
        overlay.view.setFrame(NSRect::new(
            NSPoint::new(-f64::from(origin.x), -f64::from(origin.y)),
            size,
        ));
        if let Some(layer) = overlay.view.layer() {
            layer.setContentsScale(scale as f64);
        }
        overlay.layer.setDrawableSize(CGSize::new(
            size.width * scale as f64,
            size.height * scale as f64,
        ));
        renderer.draw_layer(scene, &overlay.layer, orders, true)?;
        arranged.push(Retained::into_super(overlay.view.clone()));
        Ok(())
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        self.root.removeFromSuperview();
    }
}
pub(crate) fn rect(bounds: Bounds<Pixels>) -> NSRect {
    NSRect::new(
        NSPoint::new(f64::from(bounds.origin.x), f64::from(bounds.origin.y)),
        NSSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)),
    )
}
